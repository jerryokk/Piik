//! Build identity shared by the App's local servers. Packagers inject
//! `PIIK_RELEASE` and `PIIK_REVISION` at compile time.

pub const RELEASE: &str = match option_env!("PIIK_RELEASE") {
    Some(value) => value,
    None => "development",
};

pub const REVISION: &str = match option_env!("PIIK_REVISION") {
    Some(value) => value,
    None => "development",
};

/// A canonical `vMAJOR.MINOR.PATCH` release without pre-release or build
/// metadata, otherwise `development`.
pub fn version() -> &'static str {
    let version = RELEASE.trim();
    if canonical_release(version) {
        version
    } else {
        "development"
    }
}

/// The `os-arch` package target, such as `windows-amd64`.
pub fn package_target() -> String {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "x86" => "386",
        "aarch64" => "arm64",
        other => other,
    };
    format!("{}-{arch}", std::env::consts::OS)
}

fn canonical_release(version: &str) -> bool {
    let Some(numbers) = version.strip_prefix('v') else {
        return false;
    };
    let parts: Vec<&str> = numbers.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && (*part == "0" || !part.starts_with('0'))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_canonical_releases_are_versions() {
        for valid in ["v1.2.3", "v0.0.0", "v10.20.30"] {
            assert!(canonical_release(valid), "{valid}");
        }
        for invalid in [
            "",
            "development",
            "1.2.3",
            "v1.2",
            "v1.2.3-rc.1",
            "v1.2.3+build",
            "v01.2.3",
        ] {
            assert!(!canonical_release(invalid), "{invalid}");
        }
        assert!(package_target().contains('-'));
    }
}
