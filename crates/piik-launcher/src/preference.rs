//! Launcher UI preferences. They live outside `client.json` because published
//! App versions reject unknown configuration fields; a missing preference
//! keeps their default.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use crate::Mode;

fn beside(config_path: &Path, suffix: &str) -> PathBuf {
    let mut path = OsString::from(config_path.as_os_str());
    path.push(suffix);
    PathBuf::from(path)
}

fn read(path: &Path) -> io::Result<Option<String>> {
    match std::fs::read(path) {
        Ok(payload) => Ok(Some(String::from_utf8_lossy(&payload).trim().to_owned())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// The last launcher mode from `<config>.mode`; `None` when never saved.
pub fn load_mode(config_path: &Path) -> io::Result<Option<Mode>> {
    let Some(value) = read(&beside(config_path, ".mode"))? else {
        return Ok(None);
    };
    Mode::parse(&value)
        .map(Some)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid saved launcher mode"))
}

pub fn save_mode(config_path: &Path, mode: Mode) -> io::Result<()> {
    std::fs::write(beside(config_path, ".mode"), format!("{}\n", mode.as_str()))
}

/// The startup Site from `<config>.startup-site`. It selects one approved
/// origin and never grants access: a choice that is no longer approved is
/// cleared, so a later approval of that same address cannot resurrect it.
pub fn load_startup_site(config_path: &Path, approved_sites: &[String]) -> io::Result<String> {
    let Some(site) = read(&beside(config_path, ".startup-site"))? else {
        return Ok(String::new());
    };
    if site.is_empty() || approved_sites.contains(&site) {
        return Ok(site);
    }
    save_startup_site(config_path, "").map(|()| String::new())
}

pub fn save_startup_site(config_path: &Path, site: &str) -> io::Result<()> {
    std::fs::write(beside(config_path, ".startup-site"), format!("{site}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_mode_preference_does_not_supply_an_invalid_choice() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("client.json");
        assert_eq!(load_mode(&path).unwrap(), None);
        std::fs::write(directory.path().join("client.json.mode"), "invalid").unwrap();
        assert!(load_mode(&path).is_err(), "corrupt preference was accepted");
        for mode in [Mode::Local, Mode::Link] {
            save_mode(&path, mode).unwrap();
            assert_eq!(load_mode(&path).unwrap(), Some(mode));
        }
    }

    #[test]
    fn startup_site_clears_when_no_longer_approved() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("client.json");
        let approved = vec!["https://share.example".to_owned()];
        assert_eq!(load_startup_site(&path, &approved).unwrap(), "");
        save_startup_site(&path, "https://share.example").unwrap();
        assert_eq!(
            load_startup_site(&path, &approved).unwrap(),
            "https://share.example"
        );
        assert_eq!(load_startup_site(&path, &[]).unwrap(), "");
        assert_eq!(
            load_startup_site(&path, &approved).unwrap(),
            "",
            "a revoked choice was resurrected by approval"
        );
    }
}
