//! Server certificates for private HTTPS entry.
//!
//! [`self_signed`] creates an in-memory leaf for the exact invitation host;
//! callers never install a CA or silently change trust. [`load_pem`] loads a
//! configured certificate and key and checks that they cover the public host
//! and are currently valid. Both produce a [`ServerTls`]: a TLS 1.2+ rustls
//! configuration together with the leaf [`fingerprint`] for manual comparison.

use std::path::Path;
use std::sync::Arc;

use piik_config::{DirectTls, TlsSource};
use rcgen::{
    CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, SerialNumber,
};
use rustls::ServerConfig;
use rustls::client::verify_server_name;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use rustls::server::ParsedCertificate;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

pub use tokio_rustls::TlsAcceptor;

/// Self-signed leaves stay valid for one calendar year after creation.
const SELF_SIGNED_VALIDITY_YEARS: i32 = 1;
/// Tolerates small clock differences between Host and Viewer devices.
const SELF_SIGNED_BACKDATE: time::Duration = time::Duration::minutes(5);

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Generate(#[from] rcgen::Error),
    #[error("{0}")]
    Tls(#[from] rustls::Error),
    #[error("could not load TLS_CERT_FILE and TLS_KEY_FILE: {0}")]
    Load(String),
    #[error("{0}")]
    Parse(String),
    #[error("TLS_CERT_FILE does not cover PUBLIC_BASE_URL: {0}")]
    Hostname(String),
    #[error("TLS_CERT_FILE is expired or not yet valid")]
    Validity,
}

/// One server identity: the rustls configuration and its leaf fingerprint.
#[derive(Clone)]
pub struct ServerTls {
    config: Arc<ServerConfig>,
    fingerprint: String,
}

impl ServerTls {
    fn new(chain: Vec<CertificateDer<'static>>, key: PrivateKeyDer<'static>) -> Result<Self> {
        let fingerprint = chain
            .first()
            .map(|leaf| fingerprint(leaf))
            .unwrap_or_default();
        let config = server_config(chain, key)?;
        Ok(Self {
            config: Arc::new(config),
            fingerprint,
        })
    }

    pub fn config(&self) -> Arc<ServerConfig> {
        Arc::clone(&self.config)
    }

    /// Upper-case hex SHA-256 of the leaf certificate DER.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn acceptor(&self) -> TlsAcceptor {
        TlsAcceptor::from(self.config())
    }
}

impl std::fmt::Debug for ServerTls {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServerTls")
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

/// Builds the configured direct-TLS identity for `PUBLIC_BASE_URL`.
pub fn direct(tls: &DirectTls) -> Result<ServerTls> {
    match &tls.source {
        TlsSource::SelfSigned => self_signed(&tls.server_name),
        TlsSource::PemFiles { certificate, key } => load_pem(certificate, key, &tls.server_name),
    }
}

/// Creates a server certificate for the exact invitation host (an IP address
/// or DNS name). The private key stays in memory.
pub fn self_signed(server_name: &str) -> Result<ServerTls> {
    let (certificate, key) = generate(server_name)?;
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    ServerTls::new(vec![certificate.der().clone()], key)
}

/// Loads a PEM certificate chain and private key and verifies that the leaf
/// covers `server_name` and is currently valid.
pub fn load_pem(certificate: &Path, key: &Path, server_name: &str) -> Result<ServerTls> {
    let chain = CertificateDer::pem_file_iter(certificate)
        .and_then(|certificates| certificates.collect::<std::result::Result<Vec<_>, _>>())
        .map_err(|error| Error::Load(format!("{}: {error}", certificate.display())))?;
    if chain.is_empty() {
        return Err(Error::Load(format!(
            "{}: no certificate found",
            certificate.display()
        )));
    }
    let private_key = PrivateKeyDer::from_pem_file(key)
        .map_err(|error| Error::Load(format!("{}: {error}", key.display())))?;
    let leaf = &chain[0];
    let (_, parsed) = x509_parser::parse_x509_certificate(leaf.as_ref())
        .map_err(|error| Error::Parse(format!("parse TLS_CERT_FILE certificate: {error}")))?;
    verify_name(leaf, server_name)?;
    if !parsed.validity().is_valid() {
        return Err(Error::Validity);
    }
    // A mismatched key is part of loading the pair.
    let fingerprint = fingerprint(leaf);
    let config = server_config(chain, private_key).map_err(|error| match error {
        Error::Tls(error) => Error::Load(error.to_string()),
        other => other,
    })?;
    Ok(ServerTls {
        config: Arc::new(config),
        fingerprint,
    })
}

/// Upper-case hex SHA-256 of one DER certificate.
pub fn fingerprint(certificate: &CertificateDer<'_>) -> String {
    hex::encode_upper(Sha256::digest(certificate.as_ref()))
}

fn verify_name(leaf: &CertificateDer<'_>, server_name: &str) -> Result<()> {
    let name = ServerName::try_from(server_name)
        .map_err(|error| Error::Hostname(format!("{server_name}: {error}")))?;
    let parsed =
        ParsedCertificate::try_from(leaf).map_err(|error| Error::Hostname(error.to_string()))?;
    verify_server_name(&parsed, &name).map_err(|error| Error::Hostname(error.to_string()))
}

fn server_config(
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<ServerConfig> {
    // rustls offers only TLS 1.2 and 1.3, so TLS 1.2 is the minimum.
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(rustls::DEFAULT_VERSIONS)?
        .with_no_client_auth()
        .with_single_cert(chain, key)?;
    Ok(config)
}

fn generate(host: &str) -> Result<(rcgen::Certificate, KeyPair)> {
    let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;
    // CertificateParams::new records an IP SAN for an address and a DNS SAN otherwise.
    let mut params = CertificateParams::new(vec![host.to_owned()])?;
    params.serial_number = Some(SerialNumber::from_slice(
        &rand::random::<u128>().to_be_bytes(),
    ));
    let mut subject = DistinguishedName::new();
    subject.push(DnType::CommonName, "Piik Local");
    params.distinguished_name = subject;
    let now = OffsetDateTime::now_utc();
    params.not_before = now - SELF_SIGNED_BACKDATE;
    params.not_after = add_years(now, SELF_SIGNED_VALIDITY_YEARS);
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.is_ca = IsCa::ExplicitNoCa;
    let certificate = params.self_signed(&key)?;
    Ok((certificate, key))
}

/// Calendar-year addition that normalizes 29 February into 1 March of a
/// common year.
fn add_years(moment: OffsetDateTime, years: i32) -> OffsetDateTime {
    let year = moment.year() + years;
    moment.replace_year(year).unwrap_or_else(|_| {
        moment
            .replace_day(28)
            .and_then(|day| day.replace_year(year))
            .map(|day| day + time::Duration::days(1))
            .unwrap_or(moment + time::Duration::days(365))
    })
}

#[cfg(test)]
mod tests;
