//! Which certificates a client trusts.

use std::io::BufReader;
use std::sync::Arc;

use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;

use crate::ClientError;

/// The trust of a TLS client: the public web roots, or the certificate of a
/// development server.
#[derive(Clone)]
pub struct TlsConfig {
    pub(crate) config: Arc<rustls::ClientConfig>,
}

impl TlsConfig {
    /// Trusts the certificates the public web does.
    pub fn webpki() -> TlsConfig {
        let roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        Self::with_roots(roots)
    }

    /// Trusts exactly the certificates in `pem`: a development server's own.
    pub fn trust_pem(pem: &str) -> Result<TlsConfig, ClientError> {
        let mut roots = rustls::RootCertStore::empty();
        for cert in CertificateDer::pem_reader_iter(&mut BufReader::new(pem.as_bytes())) {
            let cert = cert.map_err(|e| ClientError::Io(e.to_string()))?;
            roots.add(cert).map_err(|e| ClientError::Io(e.to_string()))?;
        }
        if roots.is_empty() {
            return Err(ClientError::Io("no certificate in the pem".to_owned()));
        }
        Ok(Self::with_roots(roots))
    }

    fn with_roots(roots: rustls::RootCertStore) -> TlsConfig {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .expect("TLS 1.3 is supported by the ring provider")
            .with_root_certificates(roots)
            .with_no_client_auth();
        TlsConfig { config: Arc::new(config) }
    }
}
