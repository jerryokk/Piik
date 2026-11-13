//! Configuration tests by setting family. Shared fixtures and loaders live
//! in the parent module.

use super::*;

#[test]
fn load_rejects() {
    let nine = stun_list(9, "stun");
    let seven = stun_list(7, "ordinary");
    let cases: Vec<(&str, HashMap<String, String>, &str)> = vec![
        (
            "SFU port zero",
            env(&[("SFU_UDP_PORT", "0")]),
            "SFU_UDP_PORT must be a positive integer",
        ),
        (
            "SFU port negative",
            env(&[("SFU_UDP_PORT", "-1")]),
            "SFU_UDP_PORT must be a positive integer",
        ),
        (
            "SFU port fractional",
            env(&[("SFU_UDP_PORT", "7882.5")]),
            "SFU_UDP_PORT must be a positive integer",
        ),
        (
            "SFU port not a number",
            env(&[("SFU_UDP_PORT", "media")]),
            "SFU_UDP_PORT must be a positive integer",
        ),
        (
            "SFU port above UDP ceiling",
            env(&[("SFU_UDP_PORT", "65536")]),
            "SFU_UDP_PORT must be between 1 and 65535",
        ),
        (
            "SFU bind hostname",
            env(&[("SFU_UDP_PORT", "7882"), ("SFU_LISTEN_HOST", "sfu.test")]),
            "SFU_LISTEN_HOST must be an IPv4 address",
        ),
        (
            "SFU bind IPv6",
            env(&[("SFU_UDP_PORT", "7882"), ("SFU_LISTEN_HOST", "::1")]),
            "SFU_LISTEN_HOST must be an IPv4 address",
        ),
        (
            "SFU bind mapped IPv6",
            env(&[
                ("SFU_UDP_PORT", "7882"),
                ("SFU_LISTEN_HOST", "::ffff:192.0.2.5"),
            ]),
            "SFU_LISTEN_HOST must be an IPv4 address",
        ),
        (
            "SFU public URL",
            env(&[
                ("SFU_UDP_PORT", "7882"),
                ("SFU_PUBLIC_IP", "https://sfu.test"),
            ]),
            "SFU_PUBLIC_IP must be an IPv4 address or DNS hostname",
        ),
        (
            "SFU public port",
            env(&[("SFU_UDP_PORT", "7882"), ("SFU_PUBLIC_IP", "sfu.test:7882")]),
            "SFU_PUBLIC_IP must be an IPv4 address or DNS hostname",
        ),
        (
            "SFU public userinfo",
            env(&[("SFU_UDP_PORT", "7882"), ("SFU_PUBLIC_IP", "user@sfu.test")]),
            "SFU_PUBLIC_IP must be an IPv4 address or DNS hostname",
        ),
        (
            "SFU public path",
            env(&[("SFU_UDP_PORT", "7882"), ("SFU_PUBLIC_IP", "sfu.test/path")]),
            "SFU_PUBLIC_IP must be an IPv4 address or DNS hostname",
        ),
        (
            "SFU public IPv6",
            env(&[("SFU_UDP_PORT", "7882"), ("SFU_PUBLIC_IP", "2001:db8::5")]),
            "SFU_PUBLIC_IP must be an IPv4 address or DNS hostname",
        ),
        (
            "SFU public mapped IPv6",
            env(&[
                ("SFU_UDP_PORT", "7882"),
                ("SFU_PUBLIC_IP", "::ffff:198.51.100.5"),
            ]),
            "SFU_PUBLIC_IP must be an IPv4 address or DNS hostname",
        ),
        // Bounded integers.
        (
            "viewer limit above ceiling",
            env(&[("MAX_VIEWERS_PER_ROOM", "21")]),
            "MAX_VIEWERS_PER_ROOM must be between 1 and 20",
        ),
        (
            "viewer limit zero",
            env(&[("MAX_VIEWERS_PER_ROOM", "0")]),
            "MAX_VIEWERS_PER_ROOM must be a positive integer",
        ),
        (
            "fractional viewer limit",
            env(&[("MAX_VIEWERS_PER_ROOM", "1.5")]),
            "MAX_VIEWERS_PER_ROOM must be a positive integer",
        ),
        (
            "copy capacity zero",
            env(&[("ENDPOINT_MEDIA_COPY_CAPACITY", "0")]),
            "ENDPOINT_MEDIA_COPY_CAPACITY must be a positive integer",
        ),
        (
            "copy capacity above ceiling",
            env(&[("ENDPOINT_MEDIA_COPY_CAPACITY", "4")]),
            "ENDPOINT_MEDIA_COPY_CAPACITY must be between 1 and 3",
        ),
        (
            "fractional copy capacity",
            env(&[("ENDPOINT_MEDIA_COPY_CAPACITY", "1.5")]),
            "ENDPOINT_MEDIA_COPY_CAPACITY must be a positive integer",
        ),
        (
            "port zero",
            env(&[("PORT", "0")]),
            "PORT must be a positive integer",
        ),
        (
            "port not a number",
            env(&[("PORT", "eight")]),
            "PORT must be a positive integer",
        ),
        (
            "port above the TCP ceiling",
            env(&[("PORT", "65536")]),
            "PORT must be at most 65535",
        ),
        // Public base URL.
        (
            "public base URL credentials",
            env(&[("PUBLIC_BASE_URL", "https://user:pass@share.test")]),
            "PUBLIC_BASE_URL must be an origin",
        ),
        (
            "public base URL path",
            env(&[("PUBLIC_BASE_URL", "https://share.test/path")]),
            "PUBLIC_BASE_URL must be an origin",
        ),
        (
            "public base URL query",
            env(&[("PUBLIC_BASE_URL", "https://share.test?query=1")]),
            "PUBLIC_BASE_URL must be an origin",
        ),
        (
            "public base URL fragment",
            env(&[("PUBLIC_BASE_URL", "https://share.test#fragment")]),
            "PUBLIC_BASE_URL must be an origin",
        ),
        (
            "public base URL scheme",
            env(&[("PUBLIC_BASE_URL", "ftp://share.test")]),
            "PUBLIC_BASE_URL must use http or https",
        ),
        (
            "blank public base URL",
            env(&[("PUBLIC_BASE_URL", "")]),
            "PUBLIC_BASE_URL must be a valid http or https origin",
        ),
        (
            "plaintext public base URL in production",
            production(&[("PUBLIC_BASE_URL", "http://share.test")]),
            "PUBLIC_BASE_URL must use https in production",
        ),
        // Allowed origins.
        (
            "allowed origin scheme",
            env(&[("ALLOWED_ORIGINS", "ftp://share.test")]),
            "Allowed origins must use http or https",
        ),
        (
            "unparsable allowed origin",
            env(&[("ALLOWED_ORIGINS", "share.test")]),
            "Allowed origins must be valid http or https URLs",
        ),
        (
            "empty allowed origin entry",
            env(&[("ALLOWED_ORIGINS", "https://a.test,,https://b.test")]),
            "ALLOWED_ORIGINS contains an empty URL",
        ),
        // Production requirements.
        (
            "production without STUN",
            env(&[
                ("PIIK_ENV", "production"),
                ("PUBLIC_BASE_URL", "https://share.test"),
                ("SITE_ACCESS_PASSWORD", "host-password-12"),
            ]),
            "STUN is required in production",
        ),
        // STUN lists.
        (
            "STUN path",
            env(&[("STUN_URLS", "stun:stun.test/path")]),
            "STUN_URLS contains an invalid STUN URL",
        ),
        (
            "STUN hostname is not a local bind address",
            env(&[
                ("STUN_URLS", "stun:stun.test:3478"),
                ("STUN_LISTEN_HOST", "stun.test"),
            ]),
            "STUN_LISTEN_HOST must be an IPv4 address",
        ),
        (
            "STUN IPv6 listener is unsupported",
            env(&[
                ("STUN_URLS", "stun:stun.test:3478"),
                ("STUN_LISTEN_HOST", "::1"),
            ]),
            "STUN_LISTEN_HOST must be an IPv4 address",
        ),
        (
            "STUN query",
            env(&[("STUN_URLS", "stun:stun.test?transport=udp")]),
            "STUN_URLS contains an invalid STUN URL",
        ),
        (
            "STUN fragment",
            env(&[("STUN_URLS", "stun:stun.test#fragment")]),
            "STUN_URLS contains an invalid STUN URL",
        ),
        (
            "STUN without authority",
            env(&[("STUN_URLS", "stun:")]),
            "STUN_URLS contains an invalid STUN URL",
        ),
        (
            "STUN with an empty port",
            env(&[("STUN_URLS", "stun:stun.test:")]),
            "STUN_URLS contains an invalid STUN URL",
        ),
        (
            "STUN port zero",
            env(&[("STUN_URLS", "stun:stun.test:0")]),
            "STUN_URLS contains an invalid STUN URL",
        ),
        (
            "STUN userinfo",
            env(&[("STUN_URLS", "stun:user@stun.test:3478")]),
            "STUN_URLS contains an invalid STUN URL",
        ),
        (
            "TURN scheme",
            env(&[("STUN_URLS", "turn:turn.test:3478")]),
            "STUN_URLS contains an invalid STUN URL",
        ),
        (
            "STUNS scheme",
            env(&[("STUN_URLS", "stuns:stun.test:5349")]),
            "STUN_URLS contains an invalid STUN URL",
        ),
        (
            "empty STUN entry",
            env(&[("STUN_URLS", "stun:a.test:3478,,stun:b.test:3478")]),
            "STUN_URLS contains an empty URL",
        ),
        (
            "too many STUN URLs",
            env(&[("STUN_URLS", &nine)]),
            "STUN_URLS must contain at most 8 URLs",
        ),
        (
            "no NAT prediction slots",
            env(&[("STUN_URLS", &seven), ("NAT_PREDICTION_ENABLED", "true")]),
            "STUN_URLS must contain at most 6 URLs when NAT_PREDICTION_ENABLED=true",
        ),
        // NAT prediction.
        (
            "NAT prediction without STUN",
            env(&[("NAT_PREDICTION_ENABLED", "true")]),
            "NAT_PREDICTION_ENABLED requires a STUN_URLS entry on UDP 3478",
        ),
        (
            "NAT prediction cannot reinterpret explicit port 80",
            env(&[
                ("STUN_URLS", "stun:share.test:80"),
                ("NAT_PREDICTION_ENABLED", "true"),
            ]),
            "NAT_PREDICTION_ENABLED requires a STUN_URLS entry on UDP 3478",
        ),
        (
            "NAT prediction off the base port",
            env(&[
                ("STUN_URLS", "stun:share.test:5349"),
                ("NAT_PREDICTION_ENABLED", "true"),
            ]),
            "NAT_PREDICTION_ENABLED requires a STUN_URLS entry on UDP 3478",
        ),
        (
            "NAT prediction not a boolean",
            env(&[("NAT_PREDICTION_ENABLED", "sometimes")]),
            "NAT_PREDICTION_ENABLED must be true or false",
        ),
        // Room database path.
        (
            "room database path with a NUL",
            env(&[("ROOM_DATABASE_PATH", "rooms\0.sqlite")]),
            "ROOM_DATABASE_PATH must be an absolute file path",
        ),
        (
            "bare room database file name",
            env(&[("ROOM_DATABASE_PATH", "rooms.sqlite")]),
            "ROOM_DATABASE_PATH must be an absolute file path",
        ),
        (
            "relative room database path",
            env(&[("ROOM_DATABASE_PATH", "./state/rooms.sqlite")]),
            "ROOM_DATABASE_PATH must be an absolute file path",
        ),
        // Runtime environment.
        (
            "test environment is gone",
            env(&[("PIIK_ENV", "test")]),
            "PIIK_ENV must be development or production",
        ),
        (
            "blank environment",
            env(&[("PIIK_ENV", "")]),
            "PIIK_ENV must be development or production",
        ),
    ];
    for (name, environment, want) in cases {
        match load(&environment) {
            Ok(_) => panic!("{name}: want error containing {want:?}"),
            Err(error) => assert!(
                error.to_string().contains(want),
                "{name}: error {error:?}, want it to contain {want:?}"
            ),
        }
    }
}

/// A stale production environment file must not silently fall back to
/// development, where HTTPS and the Secure cookie are not required.
#[test]
fn load_rejects_replaced_variables() {
    for (name, want) in [
        ("NODE_ENV", "use PIIK_ENV"),
        ("ACCESS_PASSWORD", "use SITE_ACCESS_PASSWORD"),
        ("TURN_URLS", "ordinary ICE accepts STUN_URLS only"),
    ] {
        let error = load(&env(&[(name, "x")])).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("{name} is no longer read; {want}")
        );
    }
}
