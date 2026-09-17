//! Serving over TLS with a certificate generated at start-up.
//!
//! A test that cannot exercise the TLS path leaves TLS untested, and untested TLS is the
//! kind that fails on the day someone actually needs it. So the test server generates its
//! own certificate authority and server certificate in memory, and hands the CA back as
//! PEM so a client can be told to trust it.
//!
//! This is *not* a way to skip certificate verification: the client still verifies, using
//! [`SelfSignedIdentity::ca_pem`] as an extra trust anchor. The verification code path is
//! the same one used against a public server.

use std::sync::Arc;

use rustls::ServerConfig;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject as _};

/// A freshly generated certificate authority and server certificate.
#[derive(Debug, Clone)]
pub struct SelfSignedIdentity {
    /// The CA certificate, PEM encoded, for a client's trust store.
    pub ca_pem: String,
    /// The server certificate chain, PEM encoded.
    pub certificate_pem: String,
    /// The server private key, PEM encoded.
    pub key_pem: String,
    /// The DNS name the certificate is valid for.
    pub server_name: String,
}

impl SelfSignedIdentity {
    /// Generates an identity valid for `server_name`.
    ///
    /// # Errors
    ///
    /// Returns an error if key generation or signing fails.
    pub fn generate(server_name: &str) -> Result<Self, rcgen::Error> {
        // A single self-signed certificate acts as both the CA and the server
        // certificate. That is enough for a trust-anchor test and avoids having to build
        // a chain; a chain-validation test would need a real intermediate.
        let certified = rcgen::generate_simple_self_signed(vec![server_name.to_owned()])?;
        let certificate_pem = certified.cert.pem();

        Ok(Self {
            ca_pem: certificate_pem.clone(),
            certificate_pem,
            key_pem: certified.signing_key.serialize_pem(),
            server_name: server_name.to_owned(),
        })
    }

    /// Writes the CA certificate to a file, so it can be passed to a client as a path.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written.
    pub fn write_ca_pem(&self, path: &std::path::Path) -> std::io::Result<()> {
        std::fs::write(path, self.ca_pem.as_bytes())
    }

    /// Builds a rustls server configuration from this identity.
    ///
    /// # Errors
    ///
    /// Returns an error describing why the generated material was not usable, which would
    /// mean a bug here rather than anything a caller did.
    pub fn server_config(&self) -> Result<Arc<ServerConfig>, String> {
        let certificates: Vec<CertificateDer<'static>> =
            CertificateDer::pem_slice_iter(self.certificate_pem.as_bytes())
                .collect::<Result<_, _>>()
                .map_err(|error| format!("generated certificate is unusable: {error}"))?;

        let key = PrivateKeyDer::from_pem_slice(self.key_pem.as_bytes())
            .map_err(|error| format!("generated key is unusable: {error}"))?;

        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certificates, key)
            .map_err(|error| format!("rustls rejected the generated identity: {error}"))?;

        Ok(Arc::new(config))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_a_usable_identity() {
        let identity = SelfSignedIdentity::generate("localhost").expect("generate");

        assert!(identity.ca_pem.contains("BEGIN CERTIFICATE"));
        assert!(identity.key_pem.contains("PRIVATE KEY"));
        assert_eq!(identity.server_name, "localhost");
        assert!(identity.server_config().is_ok());
    }

    #[test]
    fn writes_the_ca_certificate_for_a_client_to_trust() {
        let identity = SelfSignedIdentity::generate("localhost").expect("generate");
        let path = std::env::temp_dir().join("nntp-testserver-ca-write-test.pem");
        identity.write_ca_pem(&path).expect("write");

        let written = std::fs::read_to_string(&path).expect("read");
        assert_eq!(written, identity.ca_pem);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn each_identity_is_distinct() {
        let first = SelfSignedIdentity::generate("localhost").expect("generate");
        let second = SelfSignedIdentity::generate("localhost").expect("generate");
        assert_ne!(first.certificate_pem, second.certificate_pem);
    }
}
