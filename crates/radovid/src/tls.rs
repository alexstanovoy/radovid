//! TLS 1.3 server configuration, and the self-signed certificates of
//! development and tests.

use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConfig;

/// A PEM certificate and its private key.
pub struct Identity {
    pub cert_pem: String,
    pub key_pem: String,
}

/// A fresh self-signed certificate for the given names.
pub fn self_signed(names: Vec<String>) -> Result<Identity, String> {
    let key = rcgen::KeyPair::generate().map_err(|e| e.to_string())?;
    let params = rcgen::CertificateParams::new(names).map_err(|e| e.to_string())?;
    let cert = params.self_signed(&key).map_err(|e| e.to_string())?;
    Ok(Identity {
        cert_pem: cert.pem(),
        key_pem: key.serialize_pem(),
    })
}

pub fn load(cert: &Path, key: &Path) -> Result<Identity, String> {
    let read = |path: &Path| {
        std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
    };
    Ok(Identity {
        cert_pem: read(cert)?,
        key_pem: read(key)?,
    })
}

/// The server side: TLS 1.3 only, no client certificates.
pub fn server_config(identity: &Identity) -> Result<Arc<ServerConfig>, String> {
    let certs: Vec<CertificateDer<'static>> =
        pem_certs(&identity.cert_pem).map_err(|e| format!("certificate: {e}"))?;
    let key = pem_key(&identity.key_pem).map_err(|e| format!("private key: {e}"))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| e.to_string())?;
    Ok(Arc::new(config))
}

fn pem_certs(pem: &str) -> Result<Vec<CertificateDer<'static>>, String> {
    use rustls::pki_types::pem::PemObject;
    let certs: Result<Vec<_>, _> = CertificateDer::pem_reader_iter(&mut BufReader::new(pem.as_bytes())).collect();
    let certs = certs.map_err(|e| e.to_string())?;
    if certs.is_empty() {
        return Err("no certificate in the file".to_owned());
    }
    Ok(certs)
}

fn pem_key(pem: &str) -> Result<PrivateKeyDer<'static>, String> {
    use rustls::pki_types::pem::PemObject;
    PrivateKeyDer::from_pem_reader(&mut BufReader::new(pem.as_bytes())).map_err(|e| e.to_string())
}
