//! Configuration tests by setting family. Shared fixtures and loaders live
//! in the parent module.

use super::*;

// ---------------------------------------------------------------------------
// ICE
// ---------------------------------------------------------------------------

#[test]
fn ice_configuration() {
    let cases = [
        (
            "returns only the configured STUN servers",
            Config {
                stun_urls: strings(&[
                    "stun:stun-a.example.test:3478",
                    "stun:stun-b.example.test:3478",
                ]),
                nat_prediction_enabled: true,
                nat_prediction_stun_urls: Some(strings(&[
                    "stun:survey-a.example.test:3478",
                    "stun:survey-b.example.test:3478",
                ])),
                ..Config::default()
            },
            r#"{"iceServers":[{"urls":["stun:stun-a.example.test:3478","stun:stun-b.example.test:3478"]}],"natPredictionStunUrls":["stun:survey-a.example.test:3478","stun:survey-b.example.test:3478"]}"#,
        ),
        (
            "keeps local development explicit when STUN is absent",
            Config::default(),
            r#"{"iceServers":[],"natPredictionStunUrls":[]}"#,
        ),
        (
            "does not expose auxiliary listeners when the capability is disabled",
            Config {
                stun_urls: strings(&["stun:share.example.test:3478"]),
                ..Config::default()
            },
            r#"{"iceServers":[{"urls":["stun:share.example.test:3478"]}],"natPredictionStunUrls":[]}"#,
        ),
        (
            "derives auxiliary listeners from an IPv6 STUN authority",
            Config {
                stun_urls: strings(&["STUN:[2001:db8::1]:3478"]),
                nat_prediction_enabled: true,
                ..Config::default()
            },
            r#"{"iceServers":[{"urls":["STUN:[2001:db8::1]:3478"]}],"natPredictionStunUrls":["stun:[2001:db8::1]:3479","stun:[2001:db8::1]:3480"]}"#,
        ),
        (
            "keeps a configured empty prediction list",
            Config {
                stun_urls: strings(&["stun:share.example.test:3478"]),
                nat_prediction_enabled: true,
                nat_prediction_stun_urls: Some(Vec::new()),
                ..Config::default()
            },
            r#"{"iceServers":[{"urls":["stun:share.example.test:3478"]}],"natPredictionStunUrls":[]}"#,
        ),
    ];
    for (name, config, want) in cases {
        let encoded = String::from_utf8(encode(&ice_config(&config))).unwrap();
        assert_eq!(encoded, want, "{name}");
    }
}

#[test]
fn nat_prediction_stun_url_derivation() {
    let cases: [(&str, &[&str], &[&str]); 12] = [
        ("no URLs", &[], &[]),
        (
            "implicit base port",
            &["stun:share.test"],
            &["stun:share.test:3479", "stun:share.test:3480"],
        ),
        (
            "explicit base port",
            &["stun:share.test:3478"],
            &["stun:share.test:3479", "stun:share.test:3480"],
        ),
        (
            "leading zeros in the port",
            &["stun:share.test:03478"],
            &["stun:share.test:3479", "stun:share.test:3480"],
        ),
        (
            "explicit port 80 is not the base port",
            &["stun:share.test:80"],
            &[],
        ),
        (
            "leading zero port 80 is not the base port",
            &["stun:share.test:00080"],
            &[],
        ),
        (
            "uppercase scheme and host",
            &["STUN:Share.TEST:3478"],
            &["stun:share.test:3479", "stun:share.test:3480"],
        ),
        (
            "IPv6 authority",
            &["stun:[2001:DB8::1]:3478"],
            &["stun:[2001:db8::1]:3479", "stun:[2001:db8::1]:3480"],
        ),
        (
            "first base listener wins",
            &["stun:a.test:80", "stun:b.test:3478", "stun:c.test:3478"],
            &["stun:b.test:3479", "stun:b.test:3480"],
        ),
        (
            "no base listener",
            &["stun:a.test:5349", "stun:b.test:19302"],
            &[],
        ),
        // TS: indexOf(":") is -1, so the whole text becomes the authority.
        (
            "no scheme separator",
            &["sharetest"],
            &["stun:sharetest:3479", "stun:sharetest:3480"],
        ),
        ("no authority", &["stun:"], &[]),
    ];
    for (name, input, want) in cases {
        let got = nat_prediction_stun_urls(&strings(input));
        assert_eq!(got, strings(want), "{name}");
        assert!(
            got.len() <= MAX_NAT_PREDICTION_AUXILIARY_STUN_URLS,
            "{name}"
        );
    }
}
