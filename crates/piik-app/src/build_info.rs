//! Build identity injected by packagers (`PIIK_RELEASE`, `PIIK_REVISION`),
//! with fixed defaults for development builds.

pub const RELEASE: &str = match option_env!("PIIK_RELEASE") {
    Some(release) => release,
    None => "development",
};

pub const REVISION: &str = match option_env!("PIIK_REVISION") {
    Some(revision) => revision,
    None => "development",
};
