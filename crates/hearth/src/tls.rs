//! The certificate the Hearth makes for itself. It lives in `<data>/tls` and is replaced only
//! when the names it has to cover change, because clients pin its fingerprint.

use std::io;

use axum_server::tls_rustls::RustlsConfig;
use hearth_api::settings::Settings;
use sha2::{Digest, Sha256};

use crate::paths::Paths;

pub struct SelfSigned {
    pub config: RustlsConfig,
    /// SHA-256 of the certificate, in hex.
    pub sha256: String,
}

pub async fn self_signed(paths: &Paths, settings: &Settings) -> io::Result<SelfSigned> {
    let dir = paths.dir("tls");
    let (cert_file, key_file, names_file, hash_file) =
        (dir.join("selfsigned.crt"), dir.join("selfsigned.key"), dir.join("selfsigned.names"), dir.join("selfsigned.sha256"));

    let mut names = vec![settings.address.clone(), "localhost".to_string(), "127.0.0.1".to_string()];
    if let Some(ip) = crate::settings::lan_ip() {
        names.push(ip.to_string());
    }
    names.retain(|name| !name.is_empty());
    names.sort();
    names.dedup();
    let wanted = names.join("\n");

    let current = std::fs::read_to_string(&names_file).unwrap_or_default();
    if current != wanted || !cert_file.exists() || !key_file.exists() || !hash_file.exists() {
        let key = rcgen::KeyPair::generate().map_err(io::Error::other)?;
        let mut params = rcgen::CertificateParams::new(names).map_err(io::Error::other)?;
        params.distinguished_name.push(rcgen::DnType::CommonName, format!("Hearth {}", settings.name));
        let now = time::OffsetDateTime::now_utc();
        params.not_before = now - time::Duration::days(1);
        params.not_after = now + time::Duration::days(3650);
        let cert = params.self_signed(&key).map_err(io::Error::other)?;
        std::fs::write(&cert_file, cert.pem())?;
        write_private(&key_file, &key.serialize_pem())?;
        std::fs::write(&hash_file, hex(&Sha256::digest(cert.der())))?;
        std::fs::write(&names_file, &wanted)?;
        tracing::info!(names = %wanted.replace('\n', ", "), "made a new self-signed certificate");
    }

    let config = RustlsConfig::from_pem_file(&cert_file, &key_file).await?;
    Ok(SelfSigned { config, sha256: std::fs::read_to_string(&hash_file)?.trim().to_string() })
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The key is for the Hearth's eyes only.
fn write_private(file: &std::path::Path, contents: &str) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let mut out = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(file)?;
        out.write_all(contents.as_bytes())
    }
    #[cfg(not(unix))]
    std::fs::write(file, contents)
}

/// A Let's Encrypt certificate, ordered and renewed by the Hearth itself.
pub struct Acme {
    pub config: RustlsConfig,
    /// Orders and renews the certificate for as long as it runs.
    pub task: tokio::task::JoinHandle<()>,
}

/// Serves the Let's Encrypt certificate once there is one, and the Hearth's own certificate
/// until then, so the admin pages stay reachable while the order is under way or failing.
#[derive(Debug)]
struct WithFallback {
    acme: std::sync::Arc<rustls_acme::ResolvesServerCertAcme>,
    own: std::sync::Arc<rustls::sign::CertifiedKey>,
}

impl rustls::server::ResolvesServerCert for WithFallback {
    fn resolve(&self, hello: rustls::server::ClientHello<'_>) -> Option<std::sync::Arc<rustls::sign::CertifiedKey>> {
        // Let's Encrypt proves the domain by connecting with a special protocol name. That
        // connection must get the challenge certificate or nothing.
        let challenge = hello.alpn().is_some_and(|mut names| names.any(|name| name == rustls_acme::acme::ACME_TLS_ALPN_NAME));
        self.acme.resolve(hello).or_else(|| (!challenge).then(|| self.own.clone()))
    }
}

/// Sets up HTTPS with a Let's Encrypt certificate for the Hearth's address. The proof of
/// ownership happens on port 443 of that address, so the owner's router has to send port
/// 443 from outside to the Hearth's port.
pub async fn lets_encrypt(paths: &Paths, settings: &Settings) -> io::Result<Acme> {
    use futures_util::StreamExt;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};

    // The Hearth's own certificate is the fallback, so make sure it exists.
    self_signed(paths, settings).await?;
    let dir = paths.dir("tls");
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(dir.join("selfsigned.crt")).map_err(io::Error::other)?.collect::<Result<_, _>>().map_err(io::Error::other)?;
    let key = PrivateKeyDer::from_pem_file(dir.join("selfsigned.key")).map_err(io::Error::other)?;
    let own = rustls::sign::CertifiedKey::new(chain, rustls::crypto::ring::sign::any_supported_type(&key).map_err(io::Error::other)?);

    let mut state = rustls_acme::AcmeConfig::new([settings.address.clone()])
        .cache(rustls_acme::caches::DirCache::new(dir.join("acme")))
        .directory_lets_encrypt(true)
        .state();
    let resolver = WithFallback { acme: state.resolver(), own: std::sync::Arc::new(own) };
    let mut config = rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(io::Error::other)?
        .with_no_client_auth()
        .with_cert_resolver(std::sync::Arc::new(resolver));
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec(), rustls_acme::acme::ACME_TLS_ALPN_NAME.to_vec()];

    let domain = settings.address.clone();
    let task = tokio::spawn(async move {
        while let Some(event) = state.next().await {
            match event {
                Ok(event) => tracing::info!(%domain, ?event, "Let's Encrypt"),
                Err(err) => tracing::warn!(%domain, error = ?err, "Let's Encrypt could not issue a certificate yet. Check that the domain points here and port 443 reaches the Hearth"),
            }
        }
    });
    Ok(Acme { config: RustlsConfig::from_config(std::sync::Arc::new(config)), task })
}
