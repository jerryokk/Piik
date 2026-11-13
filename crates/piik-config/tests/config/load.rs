//! Configuration tests by setting family. Shared fixtures and loaders live
//! in the parent module.

use super::*;

#[test]
fn load_development_defaults() {
    let config = must_load(&env(&[("PIIK_ENV", "development"), ("PORT", "9123")]));
    assert_eq!(config.env, Environment::Development);
    assert_eq!(config.listen_host, "0.0.0.0");
    assert_eq!(config.public_base_url.as_str(), "http://localhost:9123/");
    assert_eq!(config.allowed_origins, set(&["http://localhost:9123"]));
    assert_eq!(
        config.room_database_path,
        Some(std::path::absolute("rooms.sqlite").unwrap())
    );
    assert!(config.stun_urls.is_empty());
    assert!(config.stun_listen_addresses.is_empty());
    assert!(!config.nat_prediction_enabled);
    assert_eq!(config.nat_prediction_stun_urls, None);
    assert_eq!(config.max_viewers_per_room, 20);
    assert_eq!(config.endpoint_media_copy_capacity, 2);
    assert_eq!(config.sfu, None);
    assert_eq!(config.site_access_password, "");
    assert_eq!(config.tls, None);
}

#[test]
fn load_production_allows_an_unset_or_empty_site_access_password() {
    let mut unset = production(&[]);
    unset.remove("SITE_ACCESS_PASSWORD");
    for environment in [unset, production(&[("SITE_ACCESS_PASSWORD", "")])] {
        let config = must_load(&environment);
        assert_eq!(config.env, Environment::Production);
        assert_eq!(config.site_access_password, "");
    }
}

#[test]
fn load_preserves_the_chosen_site_access_password() {
    let long = "x".repeat(256);
    for password in ["", "x", "中文", " ", "  中文 +&  ", long.as_str()] {
        for environment in [
            env(&[("SITE_ACCESS_PASSWORD", password)]),
            production(&[("SITE_ACCESS_PASSWORD", password)]),
        ] {
            assert_eq!(must_load(&environment).site_access_password, password);
        }
    }
}

/// Rust tests share one process, so the default is checked against the
/// current directory instead of changing it, and no runtime file may appear
/// beside it.
#[test]
fn load_room_storage() {
    let default_path = std::path::absolute("rooms.sqlite").unwrap();
    let existed = default_path.exists();
    for base in [env(&[]), production(&[])] {
        for (value, want) in [
            (None, Some(default_path.clone())),
            (Some(""), Some(default_path.clone())),
            (Some(" \t"), Some(default_path.clone())),
            (Some(":memory:"), None),
        ] {
            let mut environment = base.clone();
            if let Some(value) = value {
                environment.insert("ROOM_DATABASE_PATH".to_owned(), value.to_owned());
            }
            assert_eq!(
                must_load(&environment).room_database_path,
                want,
                "{value:?}"
            );
        }
    }
    assert_eq!(
        default_path.exists(),
        existed,
        "configuration loading created runtime files"
    );
}

#[test]
fn load_sfu() {
    let config = must_load(&env(&[
        ("LISTEN_HOST", "127.0.0.1"),
        ("SFU_UDP_PORT", " 7882 "),
        ("SFU_LISTEN_HOST", " 192.0.2.5 "),
        ("SFU_PUBLIC_IP", " 198.51.100.5 "),
    ]));
    assert_eq!(
        config.sfu,
        Some(SfuConfig {
            listen_host: Ipv4Addr::new(192, 0, 2, 5),
            port: 7882,
            public_ip: "198.51.100.5".to_owned(),
            only: false,
        })
    );
}

#[test]
fn load_sfu_public_hostname_without_resolving() {
    for (input, want) in [
        (" Media.Example.COM ", "media.example.com"),
        ("home.example.com.", "home.example.com."),
        ("例子.example", "xn--fsqu00a.example"),
    ] {
        let config = must_load(&env(&[("SFU_UDP_PORT", "7882"), ("SFU_PUBLIC_IP", input)]));
        assert_eq!(config.sfu.unwrap().public_ip, want, "{input:?}");
    }
}

#[test]
fn load_sfu_optional() {
    for values in [
        env(&[]),
        env(&[("SFU_UDP_PORT", "")]),
        env(&[("SFU_UDP_PORT", "  ")]),
        env(&[
            ("SFU_LISTEN_HOST", "192.0.2.5"),
            ("SFU_PUBLIC_IP", "198.51.100.5"),
        ]),
    ] {
        assert_eq!(must_load(&values).sfu, None, "{values:?}");
    }
    for port in [1u16, 7882, u16::MAX] {
        let config = must_load(&production(&[("SFU_UDP_PORT", &port.to_string())]));
        assert_eq!(
            config.sfu,
            Some(SfuConfig {
                listen_host: Ipv4Addr::UNSPECIFIED,
                port,
                public_ip: String::new(),
                only: false,
            })
        );
    }
}

#[test]
fn sfu_only_requires_an_enabled_listener() {
    let config = must_load(&env(&[("SFU_UDP_PORT", "7882"), ("SFU_ONLY", "true")]));
    assert!(config.sfu.unwrap().only, "SFU_ONLY was ignored");
    for values in [
        env(&[("SFU_ONLY", "true")]),
        env(&[("SFU_ONLY", "yes"), ("SFU_UDP_PORT", "7882")]),
    ] {
        assert!(
            load(&values).is_err(),
            "accepted invalid SFU policy: {values:?}"
        );
    }
}

#[test]
fn room_empty_timeout_configuration() {
    for (value, want) in [("", 0), ("0", 0), ("3600", 3600), ("31536000", 31_536_000)] {
        let config = must_load(&env(&[("ROOM_EMPTY_TIMEOUT_SECONDS", value)]));
        assert_eq!(config.room_empty_timeout_seconds, want, "{value:?}");
    }
    for value in ["-1", "1.5", "31536001", "NaN", "Infinity"] {
        assert!(
            load(&env(&[("ROOM_EMPTY_TIMEOUT_SECONDS", value)])).is_err(),
            "accepted invalid timeout {value:?}"
        );
    }
}

#[test]
fn load_accepts() {
    let development_path =
        std::path::absolute(PathBuf::from("state").join("rooms.sqlite")).unwrap();
    let production_path =
        std::path::absolute(PathBuf::from("production-state").join("rooms.sqlite")).unwrap();
    let padded_development_path = format!(" {} ", development_path.display());

    let check = |environment: HashMap<String, String>, assert: &dyn Fn(&Config)| {
        assert(&must_load(&environment));
    };
    check(env(&[]), &|c| assert_eq!(c.max_viewers_per_room, 20));
    check(env(&[("MAX_VIEWERS_PER_ROOM", "1")]), &|c| {
        assert_eq!(c.max_viewers_per_room, 1);
    });
    check(
        env(&[(
            "MAX_VIEWERS_PER_ROOM",
            &MAX_VIEWERS_PER_ROOM_LIMIT.to_string(),
        )]),
        &|c| assert_eq!(c.max_viewers_per_room, MAX_VIEWERS_PER_ROOM_LIMIT),
    );
    for capacity in [1, 2, 3] {
        check(
            env(&[("ENDPOINT_MEDIA_COPY_CAPACITY", &capacity.to_string())]),
            &|c| assert_eq!(c.endpoint_media_copy_capacity, capacity),
        );
    }
    check(env(&[("LISTEN_HOST", " 127.0.0.1 ")]), &|c| {
        assert_eq!(c.listen_host, "127.0.0.1");
    });
    check(production(&[]), &|c| {
        assert_eq!(c.stun_urls, ["stun:stun.test:3478"]);
        assert_eq!(c.stun_listen_addresses, addresses(&["0.0.0.0:3478"]));
    });
    check(
        production(&[
            ("LISTEN_HOST", "127.0.0.1"),
            ("STUN_LISTEN_HOST", " 192.0.2.5 "),
            ("NAT_PREDICTION_ENABLED", "true"),
        ]),
        &|c| {
            assert_eq!(
                c.stun_listen_addresses,
                addresses(&["192.0.2.5:3478", "192.0.2.5:3479", "192.0.2.5:3480"]),
                "binding and advertisement were coupled"
            );
            assert_eq!(c.stun_urls, ["stun:stun.test:3478"]);
        },
    );
    check(production(&[("SITE_ACCESS_PASSWORD", "easy-key")]), &|c| {
        assert_eq!(c.site_access_password, "easy-key");
    });
    check(env(&[("STUN_URLS", "STUN:[2001:db8::1]:3478")]), &|c| {
        assert_eq!(c.stun_urls, ["STUN:[2001:db8::1]:3478"]);
    });
    check(env(&[("STUN_URLS", "stun:stun.test:80")]), &|c| {
        assert_eq!(c.stun_urls, ["stun:stun.test:80"]);
        assert_eq!(c.nat_prediction_stun_urls, None);
    });
    check(
        env(&[
            ("STUN_URLS", "stun:share.test:3478"),
            ("NAT_PREDICTION_ENABLED", "true"),
        ]),
        &|c| assert!(c.nat_prediction_enabled),
    );
    check(
        env(&[("ROOM_DATABASE_PATH", &padded_development_path)]),
        &|c| assert_eq!(c.room_database_path.as_ref(), Some(&development_path)),
    );
    check(
        production(&[("ROOM_DATABASE_PATH", production_path.to_str().unwrap())]),
        &|c| assert_eq!(c.room_database_path.as_ref(), Some(&production_path)),
    );
    check(
        env(&[(
            "ALLOWED_ORIGINS",
            " https://Example.com:443/path , http://a.test:80 ",
        )]),
        &|c| {
            assert_eq!(
                c.allowed_origins,
                set(&["https://example.com", "http://a.test"])
            );
        },
    );
    // Number(string) coercion, which the TS parsers inherited.
    check(env(&[("PORT", " 9123 ")]), &|c| assert_eq!(c.port, 9123));
    check(env(&[("PORT", "0x1F5B")]), &|c| assert_eq!(c.port, 8027));
}
