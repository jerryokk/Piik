//! Site access gate tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use super::{TEST_ACCESS_PASSWORD, cookie_pair};
use crate::access::{SiteAccessGate, SiteAccessOptions, floor_seconds};

fn clock(now: &Arc<AtomicI64>) -> crate::Clock {
    let now = now.clone();
    Arc::new(move || now.load(Ordering::SeqCst))
}

fn gate(password: &str, secure: bool, now: &Arc<AtomicI64>, ttl_seconds: i64) -> SiteAccessGate {
    SiteAccessGate::new(SiteAccessOptions {
        password: password.to_owned(),
        secure,
        now: clock(now),
        ttl_seconds,
    })
    .expect("site access gate")
}

fn at(milliseconds: i64) -> Arc<AtomicI64> {
    Arc::new(AtomicI64::new(milliseconds))
}

#[test]
fn issues_a_stateless_day_long_cookie() {
    let access = gate(TEST_ACCESS_PASSWORD, false, &at(1_000), 0);
    assert!(access.required());
    assert!(!access.is_authenticated(""));
    assert!(!access.password_matches("wrong-password"));
    assert!(access.password_matches(TEST_ACCESS_PASSWORD));

    let set_cookie = access.create_cookie().expect("cookie");
    assert!(set_cookie.contains("piik-site-access=v1."), "{set_cookie}");
    for attribute in ["Path=/", "Max-Age=86400", "HttpOnly", "SameSite=Strict"] {
        assert!(
            set_cookie.contains(attribute),
            "{set_cookie} lacks {attribute}"
        );
    }
    assert!(
        !set_cookie.contains("Secure"),
        "{set_cookie} must not be Secure without HTTPS"
    );
    assert!(!set_cookie.contains(TEST_ACCESS_PASSWORD));
    assert!(access.is_authenticated(&cookie_pair(&set_cookie)));
}

#[test]
fn cookie_attribute_order_is_exact() {
    let insecure = gate(TEST_ACCESS_PASSWORD, false, &at(1_000), 12);
    let secure = insecure.with_secure(true);
    let pair = cookie_pair(&insecure.create_cookie().expect("cookie"));
    let signature = pair.split('.').nth(2).expect("signature").to_owned();
    assert_eq!(signature.len(), 43, "base64url without padding");
    // now = 1000 ms -> floor(1) + 12 = 13 seconds.
    let value = format!("v1.13.{signature}");
    assert_eq!(
        insecure.create_cookie().expect("cookie"),
        format!("piik-site-access={value}; Path=/; Max-Age=12; HttpOnly; SameSite=Strict")
    );
    assert_eq!(
        secure.create_cookie().expect("cookie"),
        format!(
            "__Host-piik-site-access={value}; Path=/; Max-Age=12; HttpOnly; SameSite=Strict; Secure"
        )
    );
}

#[test]
fn site_cookie_uses_an_independent_process_key() {
    let now = at(1_000);
    let first = gate("1", false, &now, 0);
    let second = gate("1", false, &now, 0);
    let cookie = cookie_pair(&first.create_cookie().expect("cookie"));
    assert!(first.is_authenticated(&cookie));
    assert!(
        !second.is_authenticated(&cookie),
        "cookies authenticate only their issuing process"
    );
    // LAN/HTTPS adapters copy the same gate; they do not allocate another key.
    assert!(first.clone().is_authenticated(&cookie));
}

#[test]
fn secure_cookie_uses_the_host_prefix() {
    let now = at(1_000);
    let access = gate(TEST_ACCESS_PASSWORD, true, &now, 0);
    let set_cookie = access.create_cookie().expect("cookie");
    for attribute in [
        "__Host-piik-site-access=",
        "Secure",
        "HttpOnly",
        "SameSite=Strict",
        "Path=/",
    ] {
        assert!(
            set_cookie.contains(attribute),
            "{set_cookie} lacks {attribute}"
        );
    }
    assert!(!set_cookie.contains("Domain="));
    assert!(access.is_authenticated(&cookie_pair(&set_cookie)));
    // An insecure instance never finds a __Host- cookie.
    let insecure = gate(TEST_ACCESS_PASSWORD, false, &now, 0);
    assert!(!insecure.is_authenticated(&cookie_pair(&set_cookie)));
}

#[test]
fn renews_across_successive_idle_deadlines() {
    let now = at(1_000);
    let access = gate(TEST_ACCESS_PASSWORD, false, &now, 12);
    let original = cookie_pair(&access.create_cookie().expect("cookie"));

    now.store(7_000, Ordering::SeqCst);
    assert!(access.is_authenticated(&original));
    let first = cookie_pair(&access.create_cookie().expect("cookie"));
    assert_ne!(first, original);

    now.store(13_000, Ordering::SeqCst);
    assert!(
        !access.is_authenticated(&original),
        "the original expires 12s after issue"
    );
    assert!(access.is_authenticated(&first));
    let second = cookie_pair(&access.create_cookie().expect("cookie"));
    assert_ne!(second, first);

    now.store(20_000, Ordering::SeqCst);
    assert!(!access.is_authenticated(&first));
    assert!(access.is_authenticated(&second));
}

#[test]
fn rejects_expired_or_tampered_cookies() {
    let now = at(1_000);
    let access = gate(TEST_ACCESS_PASSWORD, false, &now, 1);
    let cookie = cookie_pair(&access.create_cookie().expect("cookie"));
    let (name, value) = cookie.split_once('=').expect("pair");
    let segments: Vec<&str> = value.split('.').collect();

    assert!(!access.is_authenticated(&format!("{cookie}x")));
    let other = gate("another-access-password", false, &now, 1);
    assert!(!other.is_authenticated(&cookie));

    // 2000 ms is second 2 and the cookie expires at second 2.
    now.store(2_000, Ordering::SeqCst);
    assert!(!access.is_authenticated(&cookie));
    now.store(1_000, Ordering::SeqCst);

    let (expires, signature) = (segments[1], segments[2]);
    for (label, header) in [
        ("no cookie header", String::new()),
        ("different cookie name", format!("other={value}")),
        ("empty value", format!("{name}=")),
        ("no equals sign", name.to_owned()),
        ("wrong version", format!("{name}=v2.{expires}.{signature}")),
        ("missing version", format!("{name}={expires}.{signature}")),
        ("extra segment", format!("{name}={value}.extra")),
        ("non-digit expiry", format!("{name}=v1.1e3.{signature}")),
        ("signed expiry", format!("{name}=v1.+42.{signature}")),
        ("empty expiry", format!("{name}=v1..{signature}")),
        (
            "short signature",
            format!("{name}=v1.{expires}.{}", &signature[..42]),
        ),
        (
            "long signature",
            format!("{name}=v1.{expires}.{signature}A"),
        ),
        (
            "padded signature",
            format!("{name}=v1.{expires}.{}=", &signature[..42]),
        ),
        (
            "unsafe expiry",
            format!("{name}=v1.9007199254740992.{signature}"),
        ),
    ] {
        assert!(
            !access.is_authenticated(&header),
            "{label}: {header:?} must not authenticate"
        );
    }
    // Surrounding cookies and whitespace are tolerated.
    for header in [
        format!("a=1; {cookie}; b=2"),
        format!("a=1;{cookie}"),
        format!(" {name} = {value} "),
    ] {
        assert!(
            access.is_authenticated(&header),
            "{header:?} must authenticate"
        );
    }
}

#[test]
fn without_a_password_every_request_is_admitted() {
    let access = gate("", false, &at(1_000), 0);
    assert!(!access.required());
    assert!(access.is_authenticated(""));
    assert!(access.is_authenticated("piik-site-access=nonsense"));
    assert!(access.password_matches("") && access.password_matches("anything"));
    assert_eq!(access.create_cookie(), None);
}

#[test]
fn ttl_is_validated_and_defaults_to_a_day() {
    let invalid = SiteAccessGate::new(SiteAccessOptions {
        password: TEST_ACCESS_PASSWORD.to_owned(),
        secure: false,
        now: clock(&at(0)),
        ttl_seconds: -1,
    });
    assert!(invalid.is_err(), "a negative TTL must be rejected");
    let access = gate(TEST_ACCESS_PASSWORD, false, &at(0), 0);
    assert!(
        access
            .create_cookie()
            .expect("cookie")
            .contains("Max-Age=86400")
    );
}

#[test]
fn floor_seconds_rounds_toward_negative_infinity() {
    assert_eq!(floor_seconds(1_999), 1);
    assert_eq!(floor_seconds(-1), -1);
    assert_eq!(floor_seconds(-1_000), -1);
    assert_eq!(floor_seconds(-1_001), -2);
}
