//! Configuration tests by setting family. Shared fixtures and loaders live
//! in the parent module.

use super::*;

#[test]
fn origin_normalization() {
    for (raw, want) in [
        ("https://Example.com:443/", "https://example.com"),
        ("http://Example.com:80", "http://example.com"),
        ("https://share.test:0443", "https://share.test"),
        ("http://localhost:9123", "http://localhost:9123"),
        ("https://[2001:DB8::1]:8443", "https://[2001:db8::1]:8443"),
        ("http://[::1]:7880", "http://[::1]:7880"),
        (
            "https://bücher.example:443",
            "https://xn--bcher-kva.example",
        ),
        ("https://faß.de", "https://xn--fa-hia.de"),
        ("https://İ.example", "https://xn--i-9bb.example"),
        ("https://ẞ.example", "https://xn--zca.example"),
        ("https://example.ΟΣ", "https://example.xn--0xai"),
        ("https://EXAMPLE。com./", "https://example.com."),
        ("https://foo_bar.ab--cd.test", "https://foo_bar.ab--cd.test"),
        ("http://[0:0:0:0:0:0:0:1]:8787", "http://[::1]:8787"),
        ("http://[::ffff:192.0.2.1]:80", "http://[::ffff:c000:201]"),
    ] {
        let config = must_load(&env(&[("PUBLIC_BASE_URL", raw)]));
        assert_eq!(origin(&config.public_base_url), want, "{raw}");
        assert_eq!(config.public_base_url.as_str(), format!("{want}/"), "{raw}");
        let config = must_load(&env(&[("ALLOWED_ORIGINS", raw)]));
        assert_eq!(config.allowed_origins, set(&[want]), "{raw}");
    }
}

#[test]
fn origin_configuration_rejects_invalid_host_and_port() {
    for raw in [
        "https://example.com:65536",
        "https://[fe80::1%25eth0]",
        "https://[not-an-ip]",
        "https://example／evil",
        "https://example：443",
        "https://example＠evil",
        "https://a％2eb",
        "https://＊.example",
        "https://\u{ad}",
        "https://a\u{200d}b.test",
    ] {
        for key in ["PUBLIC_BASE_URL", "ALLOWED_ORIGINS"] {
            assert!(load(&env(&[(key, raw)])).is_err(), "{key} accepted {raw:?}");
        }
        let options = LocalOptions {
            public_origin: raw.to_owned(),
            ..LocalOptions::default()
        };
        let error = local(options).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Local public origin must be an HTTPS origin",
            "{raw:?}"
        );
    }
}

#[test]
fn origin_from_request_host() {
    assert_eq!(
        origin_from_host("https", "Share.Test:443"),
        "https://share.test"
    );
    assert_eq!(
        origin_from_host("http", "share.test:8787"),
        "http://share.test:8787"
    );
    assert_eq!(origin_from_host("http", "share.test/path"), "");
    assert_eq!(origin_from_host("http", "user@share.test"), "");
    assert_eq!(origin_from_host("http", ""), "");
}

#[test]
fn direct_tls_configuration() {
    let origin = "https://192.0.2.10:8787";
    let config = must_load(&env(&[
        ("PUBLIC_BASE_URL", origin),
        ("TLS_SELF_SIGNED", "true"),
    ]));
    assert_eq!(
        config.tls,
        Some(DirectTls {
            source: TlsSource::SelfSigned,
            server_name: "192.0.2.10".to_owned(),
        })
    );
    let config = must_load(&env(&[
        ("PUBLIC_BASE_URL", "https://[2001:db8::1]"),
        ("TLS_CERT_FILE", " cert.pem "),
        ("TLS_KEY_FILE", "key.pem"),
    ]));
    assert_eq!(
        config.tls,
        Some(DirectTls {
            source: TlsSource::PemFiles {
                certificate: PathBuf::from("cert.pem"),
                key: PathBuf::from("key.pem"),
            },
            server_name: "2001:db8::1".to_owned(),
        })
    );
    for (name, values, want) in [
        (
            "plaintext origin",
            env(&[
                ("PUBLIC_BASE_URL", "http://192.0.2.10:8787"),
                ("TLS_SELF_SIGNED", "true"),
            ]),
            "direct TLS requires PUBLIC_BASE_URL to use https",
        ),
        (
            "mixed sources",
            env(&[
                ("PUBLIC_BASE_URL", origin),
                ("TLS_SELF_SIGNED", "true"),
                ("TLS_CERT_FILE", "cert.pem"),
            ]),
            "choose TLS_SELF_SIGNED or TLS_CERT_FILE with TLS_KEY_FILE",
        ),
        (
            "missing key",
            env(&[("PUBLIC_BASE_URL", origin), ("TLS_CERT_FILE", "cert.pem")]),
            "TLS_CERT_FILE and TLS_KEY_FILE must be configured together",
        ),
        (
            "not a boolean",
            env(&[("PUBLIC_BASE_URL", origin), ("TLS_SELF_SIGNED", "yes")]),
            "TLS_SELF_SIGNED must be true or false",
        ),
    ] {
        let error = load(&values).unwrap_err();
        assert_eq!(error.to_string(), want, "{name}");
    }
}
