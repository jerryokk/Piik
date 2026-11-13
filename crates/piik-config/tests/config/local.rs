//! Configuration tests by setting family. Shared fixtures and loaders live
//! in the parent module.

use super::*;

// ---------------------------------------------------------------------------
// Local
// ---------------------------------------------------------------------------

fn must_local(options: LocalOptions) -> Config {
    local(options).unwrap_or_else(|error| panic!("local: unexpected error {error}"))
}

#[test]
fn local_maps_client_inputs() {
    let config = must_local(LocalOptions {
        port: 9123,
        public_address: "192.168.1.10".to_owned(),
        allowed_addresses: strings(&["10.0.0.8", "192.168.1.10"]),
        site_access_password: "local-access-password".to_owned(),
        ..LocalOptions::default()
    });
    assert_eq!(config.env, Environment::Production);
    assert_eq!(config.port, 9123);
    assert_eq!(config.listen_host, "0.0.0.0");
    assert_eq!(config.site_access_password, "local-access-password");
    assert_eq!(config.max_viewers_per_room, 20);
    assert_eq!(config.endpoint_media_copy_capacity, 2);
    assert_eq!(config.stun_urls, ["stun:192.168.1.10:9123"]);
    assert_eq!(
        config.stun_listen_addresses,
        addresses(&["192.168.1.10:9123"])
    );
    assert!(!config.nat_prediction_enabled);
    assert_eq!(
        config.public_base_url.as_str(),
        "https://192.168.1.10:9123/"
    );
    assert_eq!(
        config.allowed_origins,
        set(&[
            "http://localhost:9123",
            "http://127.0.0.1:9123",
            "https://192.168.1.10:9123",
            "https://10.0.0.8:9123",
        ])
    );
    assert_eq!(config.sfu, None);
    assert_eq!(config.room_database_path, None);
}

#[test]
fn local_tunnelled_client() {
    let config = must_local(LocalOptions {
        port: 9234,
        public_address: "192.168.50.4".to_owned(),
        allowed_addresses: strings(&["192.168.50.4", "10.10.0.4"]),
        site_access_password: "persistent-local-password".to_owned(),
        public_origin: "https://small-bright-room.trycloudflare.com".to_owned(),
        stun_urls: strings(&["stun:stun.example:3478", "stun:stun.example:3479"]),
        nat_prediction_stun_urls: strings(&[
            "stun:survey-a.example:3478",
            "stun:survey-b.example:3478",
        ]),
    });
    assert_eq!(config.port, 9234);
    assert_eq!(
        origin(&config.public_base_url),
        "https://small-bright-room.trycloudflare.com"
    );
    for wanted in [
        "https://small-bright-room.trycloudflare.com",
        "http://10.10.0.4:9234",
    ] {
        assert!(config.allowed_origins.contains(wanted), "missing {wanted}");
    }
    assert_eq!(
        config.stun_urls,
        ["stun:stun.example:3478", "stun:stun.example:3479"]
    );
    assert!(config.nat_prediction_enabled);
    assert!(
        config.stun_listen_addresses.is_empty(),
        "public discovery created Local STUN listeners"
    );
    assert_eq!(config.sfu, None);
    assert_eq!(
        config.nat_prediction_stun_urls,
        Some(strings(&[
            "stun:survey-a.example:3478",
            "stun:survey-b.example:3478"
        ]))
    );
}

#[test]
fn local_tunnelled_client_without_lan_address() {
    let config = must_local(LocalOptions {
        port: 9235,
        public_origin: "https://quiet-fancy-room.trycloudflare.com".to_owned(),
        allowed_addresses: strings(&["172.18.0.1", "192.168.1.10"]),
        site_access_password: "persistent-local-password".to_owned(),
        ..LocalOptions::default()
    });
    assert_eq!(
        origin(&config.public_base_url),
        "https://quiet-fancy-room.trycloudflare.com"
    );
    assert_eq!(
        config.allowed_origins,
        set(&[
            "http://localhost:9235",
            "http://127.0.0.1:9235",
            "http://172.18.0.1:9235",
            "http://192.168.1.10:9235",
            "https://quiet-fancy-room.trycloudflare.com",
        ])
    );
}

#[test]
fn local_defaults() {
    let config = must_local(LocalOptions {
        public_address: "192.168.1.10".to_owned(),
        ..LocalOptions::default()
    });
    assert_eq!(config.site_access_password, "", "want the open site");
    assert_eq!(config.port, 8787);
    assert_eq!(config.stun_urls, ["stun:192.168.1.10:8787"]);
    assert_eq!(
        config.stun_listen_addresses,
        addresses(&["192.168.1.10:8787"])
    );
    assert_eq!(origin(&config.public_base_url), "https://192.168.1.10:8787");
    // A configured (empty) prediction list stays set so ice_config does not
    // derive one, which is TS's `?? []` spread.
    assert_eq!(config.nat_prediction_stun_urls, Some(Vec::new()));
}

#[test]
fn local_preserves_the_chosen_password() {
    let long = "x".repeat(256);
    for password in ["", "x", "中文", " ", "  中文 +&  ", long.as_str()] {
        let config = must_local(LocalOptions {
            public_address: "192.0.2.10".to_owned(),
            site_access_password: password.to_owned(),
            ..LocalOptions::default()
        });
        assert_eq!(config.site_access_password, password);
    }
}

#[test]
fn local_rejects() {
    let base = |address: &str, origin: &str| LocalOptions {
        public_address: address.to_owned(),
        public_origin: origin.to_owned(),
        site_access_password: "valid-password".to_owned(),
        ..LocalOptions::default()
    };
    let with_stun = |stun: &[&str], prediction: &[&str]| LocalOptions {
        stun_urls: strings(stun),
        nat_prediction_stun_urls: strings(prediction),
        ..base("192.168.1.2", "https://public.example.test")
    };
    let cases = [
        (
            "hostname public address",
            base("localhost", ""),
            "Local server public address must be an IPv4 address",
        ),
        (
            "missing public address without an origin",
            base("", ""),
            "Local server public address must be reachable from the LAN",
        ),
        (
            "IPv6 public address",
            base("::ffff:192.168.1.10", ""),
            "Local server public address must be an IPv4 address",
        ),
        (
            "loopback public address",
            base("127.0.0.1", ""),
            "Local server public address must be reachable from the LAN",
        ),
        (
            "unspecified public address",
            base("0.0.0.0", ""),
            "Local server public address must be reachable from the LAN",
        ),
        (
            "hostname allowed address",
            LocalOptions {
                allowed_addresses: strings(&["share.local"]),
                ..base("192.168.1.2", "")
            },
            "Local server allowed address must be an IPv4 address",
        ),
        (
            "plaintext public origin",
            base("192.168.1.2", "http://public.example"),
            "Local public origin must be an HTTPS origin",
        ),
        (
            "public origin with a path",
            base("192.168.1.2", "https://public.example/path"),
            "Local public origin must be an HTTPS origin",
        ),
        (
            "public origin with userinfo",
            base("192.168.1.2", "https://user@public.example"),
            "Local public origin must be an HTTPS origin",
        ),
        (
            "unparsable public origin",
            base("192.168.1.2", "public.example"),
            "Local public origin must be an HTTPS origin",
        ),
        (
            "invalid STUN URL",
            with_stun(&["turn:turn.test:3478"], &[]),
            "Local STUN URLs are invalid",
        ),
        (
            "invalid prediction STUN URL",
            with_stun(
                &["stun:a.test:3478"],
                &["stun:a.test:3479", "stuns:b.test:3480"],
            ),
            "Local STUN URLs are invalid",
        ),
        (
            "single prediction STUN URL",
            with_stun(&["stun:a.test:3478"], &["stun:a.test:3479"]),
            "Local STUN URLs are invalid",
        ),
        (
            "prediction without ordinary STUN",
            with_stun(&[], &["stun:a.test:3479", "stun:a.test:3480"]),
            "Local STUN URLs are invalid",
        ),
        (
            "more ICE URLs than the wire carries",
            with_stun(
                &[
                    "stun:a.test:3478",
                    "stun:b.test:3478",
                    "stun:c.test:3478",
                    "stun:d.test:3478",
                    "stun:e.test:3478",
                    "stun:f.test:3478",
                    "stun:g.test:3478",
                ],
                &["stun:a.test:3479", "stun:a.test:3480"],
            ),
            "Local STUN URLs are invalid",
        ),
    ];
    for (name, options, want) in cases {
        match local(options) {
            Ok(_) => panic!("{name}: want error containing {want:?}"),
            Err(error) => assert!(
                error.to_string().contains(want),
                "{name}: error {error:?}, want it to contain {want:?}"
            ),
        }
    }
}
