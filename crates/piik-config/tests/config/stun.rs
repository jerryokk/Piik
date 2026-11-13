//! Configuration tests by setting family. Shared fixtures and loaders live
//! in the parent module.

use super::*;

#[test]
fn parse_stun_urls_names_its_source() {
    assert_eq!(
        parse_stun_urls(" ", "PIIK_STUN_URLS").unwrap(),
        Vec::<String>::new()
    );
    assert_eq!(
        parse_stun_urls("turn:t.test", "PIIK_STUN_URLS")
            .unwrap_err()
            .to_string(),
        "PIIK_STUN_URLS contains an invalid STUN URL"
    );
}
