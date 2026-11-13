use std::path::{Path, PathBuf};

use piik_config::{DirectTls, TlsSource};
use x509_parser::extensions::GeneralName;

use super::*;

fn write_pair(directory: &Path, host: &str) -> (PathBuf, PathBuf, String) {
    let (certificate, key) = generate(host).expect("generate certificate");
    let certificate_file = directory.join("cert.pem");
    let key_file = directory.join("key.pem");
    std::fs::write(&certificate_file, certificate.pem()).expect("write certificate");
    std::fs::write(&key_file, key.serialize_pem()).expect("write key");
    (certificate_file, key_file, fingerprint(certificate.der()))
}

fn leaf_names(certificate: &CertificateDer<'_>) -> Vec<String> {
    let (_, parsed) = x509_parser::parse_x509_certificate(certificate).expect("parse");
    parsed
        .subject_alternative_name()
        .expect("SAN extension")
        .expect("SAN present")
        .value
        .general_names
        .iter()
        .map(|name| match name {
            GeneralName::DNSName(name) => format!("dns:{name}"),
            GeneralName::IPAddress(bytes) => format!("ip:{bytes:?}"),
            other => format!("{other:?}"),
        })
        .collect()
}

#[test]
fn self_signed_covers_the_exact_host_and_is_not_a_ca() {
    let host = "192.0.2.10";
    let generated = self_signed(host).expect("self-signed");
    assert_eq!(generated.fingerprint().len(), 64);
    assert!(
        generated
            .fingerprint()
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase())
    );

    let (certificate, _) = generate(host).expect("generate");
    verify_name(certificate.der(), host).expect("certificate covers the host");
    assert!(verify_name(certificate.der(), "192.0.2.11").is_err());
    let (_, parsed) = x509_parser::parse_x509_certificate(certificate.der()).expect("parse");
    let constraints = parsed
        .basic_constraints()
        .expect("basic constraints")
        .expect("basic constraints present");
    assert!(!constraints.value.ca);
    assert!(parsed.validity().is_valid());
    assert_eq!(
        parsed
            .subject()
            .iter_common_name()
            .next()
            .and_then(|name| name.as_str().ok()),
        Some("Piik Local")
    );
    assert_eq!(leaf_names(certificate.der()), vec!["ip:[192, 0, 2, 10]"]);

    let (dns, _) = generate("piik.example").expect("generate dns");
    verify_name(dns.der(), "piik.example").expect("certificate covers the DNS name");
    assert_eq!(leaf_names(dns.der()), vec!["dns:piik.example"]);
}

#[test]
fn direct_tls_configuration_loads_and_rejects_files() {
    let directory = tempfile::tempdir().expect("temp dir");
    let (certificate_file, key_file, expected) = write_pair(directory.path(), "192.0.2.10");

    let loaded = load_pem(&certificate_file, &key_file, "192.0.2.10").expect("configured pair");
    assert_eq!(
        loaded.fingerprint(),
        expected,
        "configured certificate was not retained"
    );

    let configured = direct(&DirectTls {
        source: TlsSource::PemFiles {
            certificate: certificate_file.clone(),
            key: key_file.clone(),
        },
        server_name: "192.0.2.10".to_owned(),
    })
    .expect("direct PEM configuration");
    assert_eq!(configured.fingerprint(), expected);
    direct(&DirectTls {
        source: TlsSource::SelfSigned,
        server_name: "192.0.2.10".to_owned(),
    })
    .expect("direct self-signed configuration");

    let missing = directory.path().join("missing.key");
    for (name, certificate, key, host, want) in [
        (
            "unreadable pair",
            certificate_file.as_path(),
            missing.as_path(),
            "192.0.2.10",
            "could not load",
        ),
        (
            "wrong host",
            certificate_file.as_path(),
            key_file.as_path(),
            "other.example",
            "does not cover PUBLIC_BASE_URL",
        ),
    ] {
        let error = load_pem(certificate, key, host).expect_err(name);
        assert!(
            error.to_string().contains(want),
            "{name}: TLS configuration error = {error}, want {want:?}"
        );
    }
}

#[test]
fn mismatched_key_is_a_load_failure() {
    let directory = tempfile::tempdir().expect("temp dir");
    let (certificate_file, _, _) = write_pair(directory.path(), "192.0.2.10");
    let other = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("key");
    let other_key = directory.path().join("other.pem");
    std::fs::write(&other_key, other.serialize_pem()).expect("write key");
    let error = load_pem(&certificate_file, &other_key, "192.0.2.10").expect_err("mismatch");
    assert!(error.to_string().contains("could not load"), "{error}");
}

#[test]
fn expired_certificate_is_rejected() {
    let directory = tempfile::tempdir().expect("temp dir");
    let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("key");
    let mut params = CertificateParams::new(vec!["192.0.2.10".to_owned()]).expect("params");
    let now = OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(30);
    params.not_after = now - time::Duration::days(1);
    let certificate = params.self_signed(&key).expect("certificate");
    let certificate_file = directory.path().join("cert.pem");
    let key_file = directory.path().join("key.pem");
    std::fs::write(&certificate_file, certificate.pem()).expect("write certificate");
    std::fs::write(&key_file, key.serialize_pem()).expect("write key");
    let error = load_pem(&certificate_file, &key_file, "192.0.2.10").expect_err("expired");
    assert_eq!(
        error.to_string(),
        "TLS_CERT_FILE is expired or not yet valid"
    );
}

#[test]
fn calendar_year_follows_go_add_date() {
    let leap = time::macros::datetime!(2028-02-29 12:00 UTC);
    assert_eq!(
        add_years(leap, 1),
        time::macros::datetime!(2029-03-01 12:00 UTC)
    );
    let ordinary = time::macros::datetime!(2026-11-05 08:30 UTC);
    assert_eq!(
        add_years(ordinary, 1),
        time::macros::datetime!(2027-11-05 08:30 UTC)
    );
}
