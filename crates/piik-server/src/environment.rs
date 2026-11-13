//! Startup inputs: the optional `.env` file, the process environment and the
//! diagnostic selection.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

/// Reads optional startup overrides. An absent file is the ordinary case;
/// every other read failure stops startup. Returns the assignments for names
/// `present` does not already carry, so a service manager's environment
/// still wins over a stale file.
///
/// Syntax (DECISIONS D8): `KEY=value` lines, whole-line `#` comments, an
/// optional `export ` prefix, and a single- or double-quoted value kept
/// verbatim. Nothing is expanded.
pub fn environment_file(
    path: &Path,
    present: impl Fn(&str) -> bool,
) -> io::Result<Vec<(String, String)>> {
    let content = match std::fs::read(path) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let content = String::from_utf8_lossy(&content);
    let mut assignments: Vec<(String, String)> = Vec::new();
    for line in content.split('\n') {
        let line = line.trim();
        let line = line.strip_prefix("export ").unwrap_or(line).trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        // The first assignment of a name wins, as each one set the variable.
        if key.is_empty() || present(key) || assignments.iter().any(|(name, _)| name == key) {
            continue;
        }
        assignments.push((key.to_owned(), unquote(value.trim()).to_owned()));
    }
    Ok(assignments)
}

/// Removes one matching pair of surrounding single or double quotes.
fn unquote(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 && bytes[0] == bytes[bytes.len() - 1] && matches!(bytes[0], b'"' | b'\'') {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

/// The process environment in the form `piik_config::load` expects.
pub fn process_environment() -> HashMap<String, String> {
    std::env::vars_os()
        .map(|(name, value)| {
            (
                name.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect()
}

/// `PIIK_DEBUG` names the `server` or `route` section.
pub fn server_debug_enabled(value: &str) -> bool {
    value
        .split(',')
        .any(|component| matches!(component.trim(), "server" | "route"))
}

/// `PIIK_LOG_DIR`, otherwise a service manager's `LOGS_DIRECTORY`, otherwise
/// `logs` under the working directory.
pub fn server_log_directory(log_dir: Option<&str>, logs_directory: Option<&str>) -> PathBuf {
    if let Some(directory) = log_dir
        .map(str::trim)
        .filter(|directory| !directory.is_empty())
    {
        return PathBuf::from(directory);
    }
    if let Some(first) = logs_directory
        .and_then(|value| std::env::split_paths(value).next())
        .filter(|first| !first.as_os_str().is_empty())
    {
        return first;
    }
    PathBuf::from("logs")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_file_fills_only_absent_variables() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join(".env");
        let content = "# Application\n\
            PIIK_TEST_PRESET=from-the-file\n\
            export PIIK_TEST_EXPORTED=exported\n  \
            PIIK_TEST_QUOTED = \"a b#c\" \n\
            PIIK_TEST_SINGLE='  padded  '\n\
            PIIK_TEST_EMPTY=\n\
            PIIK_TEST_MARK=has#hash\n\
            \n\
            not an assignment\n\
            #PIIK_TEST_COMMENTED=never\r\n\
            PIIK_TEST_LAST=tail";
        std::fs::write(&path, content).expect("write .env");
        let assignments =
            environment_file(&path, |name| name == "PIIK_TEST_PRESET").expect("read .env");
        let found: HashMap<String, String> = assignments.into_iter().collect();
        let want = HashMap::from([
            ("PIIK_TEST_EXPORTED", "exported"),
            ("PIIK_TEST_QUOTED", "a b#c"),
            ("PIIK_TEST_SINGLE", "  padded  "),
            ("PIIK_TEST_EMPTY", ""),
            ("PIIK_TEST_MARK", "has#hash"),
            ("PIIK_TEST_LAST", "tail"),
        ]);
        assert_eq!(found.len(), want.len(), "{found:?}");
        for (name, value) in want {
            assert_eq!(found.get(name).map(String::as_str), Some(value), "{name}");
        }
        // A variable the service already supplies is never replaced.
        assert!(!found.contains_key("PIIK_TEST_PRESET"));
        assert!(!found.contains_key("PIIK_TEST_COMMENTED"));
    }

    #[test]
    fn environment_file_ignores_only_a_missing_file() {
        let directory = tempfile::tempdir().expect("tempdir");
        let missing =
            environment_file(&directory.path().join(".env"), |_| false).expect("missing file");
        assert!(missing.is_empty());
        // A directory in place of the file is a read failure, not an absent file.
        assert!(environment_file(directory.path(), |_| false).is_err());
    }

    #[test]
    fn server_diagnostic_selection() {
        for (value, want) in [
            ("server", true),
            ("route", true),
            ("client, route", true),
            ("", false),
            ("client", false),
            ("all", false),
            ("server-secret", false),
        ] {
            assert_eq!(server_debug_enabled(value), want, "{value:?}");
        }
        assert_eq!(
            server_log_directory(Some(""), Some("")),
            PathBuf::from("logs")
        );
        assert_eq!(server_log_directory(None, None), PathBuf::from("logs"));
        let list = std::env::join_paths(["service-logs", "other-logs"]).expect("path list");
        let list = list.to_string_lossy();
        assert_eq!(
            server_log_directory(Some(" "), Some(&list)),
            PathBuf::from("service-logs")
        );
        assert_eq!(
            server_log_directory(Some("explicit-logs"), Some(&list)),
            PathBuf::from("explicit-logs")
        );
    }
}
