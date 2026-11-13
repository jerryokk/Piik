use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use piik_config::{OriginError, is_bare_origin, origin, parse_url};
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

pub const CURRENT_VERSION: u32 = 2;

/// The strict `client.json` document. Empty fields are omitted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AppConfig {
    pub version: u32,
    #[serde(default)]
    pub local_access_password: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sites: Vec<String>,
}

/// Normalizes and deduplicates a Site list; any blank or invalid entry fails.
pub fn normalize_sites(sites: &[String]) -> Result<Vec<String>> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for value in sites {
        let site = normalize_site(value)
            .ok()
            .filter(|site| !site.is_empty())
            .ok_or_else(|| Error::invalid("App site list is invalid"))?;
        if seen.insert(site.clone()) {
            normalized.push(site);
        }
    }
    Ok(normalized)
}

/// `%AppData%\Piik\client.json`, the per-user configuration directory on
/// Windows.
pub fn default_path() -> Result<PathBuf> {
    match std::env::var_os("APPDATA") {
        Some(directory) if !directory.is_empty() => {
            Ok(PathBuf::from(directory).join("Piik").join("client.json"))
        }
        _ => Err(Error::Io {
            context: "find user configuration directory",
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "%AppData% is not defined"),
        }),
    }
}

/// Loads the configuration, creating the current default when absent. An
/// existing file that does not match the current version is an error.
pub fn load_or_create(path: &Path) -> Result<AppConfig> {
    match load(path) {
        Err(error) if error.is_not_found() => {}
        result => return result,
    }
    let config = AppConfig {
        version: CURRENT_VERSION,
        ..AppConfig::default()
    };
    let payload = encode(&config)?;
    create_directory(path)?;
    let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return load(path),
        file => file.map_err(Error::io("create App configuration"))?,
    };
    if let Err(error) = file.write_all(&payload) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(Error::io("write App configuration")(error));
    }
    Ok(config)
}

pub fn save(path: &Path, config: &AppConfig) -> Result<()> {
    save_file(path, &encode(config)?)
}

/// Keeps the last readable file until the replacement is complete. The
/// private temporary file lives in the same directory so the rename stays on
/// one volume, and it is removed when publication fails.
pub(crate) fn save_file(path: &Path, payload: &[u8]) -> Result<()> {
    create_directory(path)?;
    let directory = parent(path);
    let mut file = tempfile::Builder::new()
        .prefix(".piik-config-")
        .tempfile_in(directory)
        .map_err(Error::io("create App configuration replacement"))?;
    file.write_all(payload)
        .and_then(|()| file.as_file().sync_all())
        .map_err(Error::io("write App configuration"))?;
    file.persist(path)
        .map_err(|error| Error::io("replace App configuration")(error.error))?;
    Ok(())
}

/// Accepts a bare host (HTTPS is implied) or an HTTP(S) origin, returning its
/// WHATWG serialization; blank input is the empty Site.
pub fn normalize_site(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(String::new());
    }
    let value = if value.contains("://") {
        value.to_owned()
    } else {
        format!("https://{value}")
    };
    let invalid = || Error::invalid("Piik Site must be an HTTP or HTTPS origin");
    let parsed = parse_url(&value, &["http", "https"]).map_err(|error| match error {
        OriginError::Host => Error::invalid("Piik Site must have a valid host and port"),
        OriginError::Syntax | OriginError::Scheme => invalid(),
    })?;
    // Any userinfo, even an empty `@`, is refused.
    let authority = value
        .split_once("://")
        .map_or("", |(_, rest)| rest)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    if authority.contains('@') || !is_bare_origin(&parsed) {
        return Err(invalid());
    }
    Ok(origin(&parsed))
}

fn load(path: &Path) -> Result<AppConfig> {
    let payload = std::fs::read(path).map_err(Error::io("read App configuration"))?;
    let invalid = || Error::invalid("App configuration is invalid");
    let mut config: AppConfig = serde_json::from_slice(&payload).map_err(|_| invalid())?;
    if config.version != CURRENT_VERSION {
        return Err(invalid());
    }
    config.sites = normalize_sites(&config.sites)?;
    Ok(config)
}

pub(crate) fn encode(config: &AppConfig) -> Result<Vec<u8>> {
    let invalid = || Error::invalid("App configuration is invalid");
    if config.version != CURRENT_VERSION {
        return Err(invalid());
    }
    let mut config = config.clone();
    config.sites = normalize_sites(&config.sites)?;
    let mut payload = serde_json::to_vec_pretty(&config).map_err(|_| invalid())?;
    payload.push(b'\n');
    Ok(payload)
}

fn parent(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

fn create_directory(path: &Path) -> Result<()> {
    std::fs::create_dir_all(parent(path)).map_err(Error::io("create App configuration directory"))
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn load_or_create_persists_one_valid_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested").join("client.json");
        let first = load_or_create(&path).unwrap();
        let second = load_or_create(&path).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.version, CURRENT_VERSION);
        assert_eq!(first.local_access_password, "");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\n  \"version\": 2,\n  \"localAccessPassword\": \"\"\n}\n"
        );
    }

    #[test]
    fn user_chosen_local_access_password_is_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("client.json");
        let long = "x".repeat(256);
        for value in ["", "x", "中文", " ", "  中文 +&  ", long.as_str()] {
            let config = AppConfig {
                version: CURRENT_VERSION,
                local_access_password: value.to_owned(),
                ..AppConfig::default()
            };
            save(&path, &config).unwrap();
            assert_eq!(load_or_create(&path).unwrap().local_access_password, value);
        }
    }

    #[test]
    fn save_persists_approved_sites() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("client.json");
        let mut config = load_or_create(&path).unwrap();
        config.sites = strings(&[
            "https://share.example",
            "https://friends.example",
            "https://share.example/",
        ]);
        save(&path, &config).unwrap();
        let loaded = load_or_create(&path).unwrap();
        assert_eq!(
            loaded.sites,
            ["https://share.example", "https://friends.example"]
        );
        assert_eq!(loaded.local_access_password, config.local_access_password);
        let mut remaining = loaded.clone();
        remaining.sites.remove(0);
        save(&path, &remaining).unwrap();
        assert_eq!(
            load_or_create(&path).unwrap().sites,
            ["https://friends.example"]
        );
    }

    #[test]
    fn save_replacement_failure_preserves_destination_and_cleans_temporary_file() {
        let directory = tempfile::tempdir().unwrap();
        // A directory cannot be replaced by a configuration file. Its contents
        // must survive the failed commit and no private temporary payload may
        // be left.
        let path = directory.path().join("client.json");
        std::fs::create_dir(&path).unwrap();
        let marker = path.join("keep");
        std::fs::write(&marker, "original").unwrap();
        let config = AppConfig {
            version: CURRENT_VERSION,
            ..AppConfig::default()
        };
        assert!(save(&path, &config).is_err());
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "original");
        let entries: Vec<_> = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, ["client.json"]);
    }

    #[test]
    fn save_publishes_complete_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("client.json");
        let initial = AppConfig {
            version: CURRENT_VERSION,
            sites: strings(&["https://old.example"]),
            ..AppConfig::default()
        };
        save(&path, &initial).unwrap();
        let updated = AppConfig {
            version: CURRENT_VERSION,
            sites: strings(&["https://new.example"]),
            local_access_password: "new".to_owned(),
        };
        save(&path, &updated).unwrap();
        assert_eq!(load_or_create(&path).unwrap(), updated);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn save_does_not_modify_previous_file_in_place() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("client.json");
        let initial = AppConfig {
            version: CURRENT_VERSION,
            sites: strings(&["https://old.example"]),
            ..AppConfig::default()
        };
        save(&path, &initial).unwrap();
        let mut previous = std::fs::File::open(&path).unwrap();
        let updated = AppConfig {
            version: CURRENT_VERSION,
            sites: strings(&["https://new.example"]),
            ..AppConfig::default()
        };
        // Windows readers may deny replacement. Whether publication succeeds
        // or fails, an open previous file must never expose a truncated rewrite.
        let saved = save(&path, &updated);
        let mut actual = Vec::new();
        previous.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, encode(&initial).unwrap());
        drop(previous);
        let loaded = load_or_create(&path).unwrap();
        assert_eq!(loaded, if saved.is_ok() { updated } else { initial });
    }

    #[test]
    fn normalize_site_accepts_only_a_plain_web_origin() {
        for (value, want) in [
            ("", ""),
            (" \t ", ""),
            ("share.example", "https://share.example"),
            (" Share.Example:443/ ", "https://share.example"),
            ("share.example:8443", "https://share.example:8443"),
            ("[::1]:8443", "https://[::1]:8443"),
            ("bücher.example", "https://xn--bcher-kva.example"),
            (" HTTPS://Share.Example/ ", "https://share.example"),
            ("https://share.example", "https://share.example"),
            ("https://Share.Example:443/", "https://share.example"),
            ("http://Share.Example:80", "http://share.example"),
            ("https://share.example:0443", "https://share.example"),
            ("http://192.168.1.4:8787/", "http://192.168.1.4:8787"),
            ("http://share.example:443", "http://share.example:443"),
            ("https://[::1]:443", "https://[::1]"),
            (
                "https://bücher.example:443",
                "https://xn--bcher-kva.example",
            ),
            ("http://[0:0:0:0:0:0:0:1]:8787", "http://[::1]:8787"),
        ] {
            assert_eq!(normalize_site(value).unwrap(), want, "{value:?}");
        }
        for value in [
            "https://https://share.example",
            "share.example/r/9527",
            "share.example?mode=host",
            "share.example#credential",
            "user:pass@share.example",
            "share.example:65536",
            "ftp://share.example",
            "javascript:alert(1)",
            "file:///tmp/app",
            "https://user:pass@share.example",
            "https://@share.example",
            "https://share.example/room",
            "https://share.example?mode=host",
            "https://example.com:65536",
            "https://[fe80::1%25eth0]",
            "https://example：443",
            "https://example／evil",
            "https://\u{ad}",
        ] {
            assert!(normalize_site(value).is_err(), "accepted {value:?}");
        }
    }

    #[test]
    fn load_or_create_rejects_unknown_or_malformed_state() {
        for payload in [
            r#"{"version":2,"localAccessPassword":"abcdefghijklmnopqrstuvwxyzABCDEF","extra":true}"#,
            r#"{"version":1,"localAccessPassword":"abcdefghijklmnopqrstuvwxyzABCDEF"}"#,
            r#"{"version":3,"localAccessPassword":"abcdefghijklmnopqrstuvwxyzABCDEF"}"#,
            r#"{"version":2,"localAccessPassword":123}"#,
            r#"{"version":2,"sites":["ftp://share.example"]}"#,
            r#"{"version":2,"sites":[""]}"#,
            r#"{"version":2,"site":"https://share.example"}"#,
            r#"{"version":2,"localAccessPassword":"abcdefghijklmnopqrstuvwxyzABCDEF"} trailing"#,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("client.json");
            std::fs::write(&path, payload).unwrap();
            assert!(load_or_create(&path).is_err(), "accepted {payload}");
        }
    }

    #[test]
    fn default_path_is_under_app_data() {
        let path = default_path().unwrap();
        assert!(path.ends_with(Path::new("Piik").join("client.json")));
        assert!(path.starts_with(std::env::var_os("APPDATA").unwrap()));
    }
}
