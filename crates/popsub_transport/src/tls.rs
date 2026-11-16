//! TLS for the server, so clients can connect with `wss://` without a
//! TLS-terminating proxy in front.
//!
//! The certificate chain and private key are PEM files, read once when the
//! server starts. A renewed certificate takes a restart to be picked up.
//! Crypto comes from `ring` through rustls, with its safe default protocol
//! versions (TLS 1.2 and 1.3). Client certificates aren't asked for.

use std::path::Path;
use std::sync::Arc;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::TlsAcceptor;

/// The server's TLS certificate and key, ready to accept connections with.
#[derive(Clone)]
pub struct ServerTls {
    /// Performs the TLS handshake on accepted sockets.
    acceptor: TlsAcceptor,
}

impl std::fmt::Debug for ServerTls {
    /// Writes the type name only, never key material.
    ///
    /// Args:
    ///     f: The formatter.
    ///
    /// Returns:
    ///     The formatting result.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ServerTls")
    }
}

impl ServerTls {
    /// Loads the certificate chain and private key from PEM files.
    ///
    /// Args:
    ///     cert: The certificate chain, leaf first.
    ///     key: The private key (PKCS#8, PKCS#1 or SEC1).
    ///
    /// Returns:
    ///     The TLS setup.
    ///
    /// Raises:
    ///     String: When a file can't be read, holds no certificate or key,
    ///         or the key doesn't match the certificate.
    pub fn from_pem_files(cert: &Path, key: &Path) -> Result<Self, String> {
        let read = |path: &Path| {
            std::fs::read(path).map_err(|e| format!("can't read {}: {e}", path.display()))
        };
        Self::from_pem(&read(cert)?, &read(key)?)
            .map_err(|e| format!("{} and {}: {e}", cert.display(), key.display()))
    }

    /// Builds the TLS setup from PEM text.
    ///
    /// Args:
    ///     cert: The certificate chain, leaf first.
    ///     key: The private key.
    ///
    /// Returns:
    ///     The TLS setup.
    ///
    /// Raises:
    ///     String: When there is no certificate or key, the key's type isn't
    ///         supported (such as an EC key with explicit curve parameters),
    ///         or they don't match.
    pub fn from_pem(cert: &[u8], key: &[u8]) -> Result<Self, String> {
        let chain = CertificateDer::pem_slice_iter(cert)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("the certificate isn't valid PEM: {e}"))?;
        if chain.is_empty() {
            return Err("no certificate found".to_string());
        }
        let key = PrivateKeyDer::from_pem_slice(key)
            .map_err(|e| format!("no usable private key found: {e}"))?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        provider
            .key_provider
            .load_private_key(key.clone_key())
            .map_err(|e| {
                format!(
                    "the private key isn't usable; it must be RSA, Ed25519, or ECDSA on P-256 or P-384 with a named curve: {e}"
                )
            })?;
        let mut config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .map_err(|e| format!("the private key doesn't match the certificate: {e}"))?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Self {
            acceptor: TlsAcceptor::from(Arc::new(config)),
        })
    }

    /// The acceptor that performs the handshake.
    ///
    /// Returns:
    ///     A handle sharing this setup.
    pub(crate) fn acceptor(&self) -> TlsAcceptor {
        self.acceptor.clone()
    }
}
