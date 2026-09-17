//! TLS, via rustls.
//!
//! Certificate verification is always on and cannot be switched off from anywhere in this
//! crate — there is no `accept_invalid_certs` flag to find. A private certificate
//! authority is supported instead, through [`TlsOptions::extra_ca_file`]. The reasoning,
//! including what that costs users behind a corporate CA, is in [ADR-0007].
//!
//! [ADR-0007]: https://github.com/edusouza/rust-nntp/blob/main/docs/adr/0007-rustls-for-tls.md

use std::io::{BufReader, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use rustls_pki_types::ServerName;

use crate::error::{ClientError, Result};

/// A TLS stream over a TCP socket.
pub type TlsStream = StreamOwned<ClientConnection, TcpStream>;

/// How to verify the server.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TlsOptions {
    /// A PEM bundle of extra trust anchors, for a server signed by a private CA.
    ///
    /// Added to the built-in Mozilla root set rather than replacing it.
    pub extra_ca_file: Option<PathBuf>,

    /// The name to verify the certificate against, if it differs from the host connected
    /// to.
    ///
    /// Needed when connecting by IP address to a server whose certificate names a host,
    /// which is common for a news server behind a load balancer.
    pub server_name: Option<String>,
}

impl TlsOptions {
    /// Default options: the built-in root set, verified against the connected host.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a PEM bundle of extra trust anchors.
    #[must_use]
    pub fn extra_ca_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.extra_ca_file = Some(path.into());
        self
    }

    /// Verifies the certificate against this name instead of the connected host.
    #[must_use]
    pub fn server_name(mut self, name: impl Into<String>) -> Self {
        self.server_name = Some(name.into());
        self
    }
}

/// Builds a rustls client configuration.
///
/// # Errors
///
/// Returns [`ClientError::Tls`] if the extra CA file cannot be read or contains no
/// usable certificate. A CA file that was asked for and cannot be used is an error, not a
/// warning: silently falling back to the public roots would connect to the wrong server
/// without saying so.
pub fn client_config(options: &TlsOptions) -> Result<Arc<ClientConfig>> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    if let Some(path) = &options.extra_ca_file {
        let added = add_pem_anchors(&mut roots, path)?;
        if added == 0 {
            return Err(ClientError::Tls(format!(
                "{} contains no certificates",
                path.display()
            )));
        }
        tracing::debug!(path = %path.display(), added, "added extra trust anchors");
    }

    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    Ok(Arc::new(config))
}

/// Reads PEM certificates from `path` into `roots`, returning how many were added.
fn add_pem_anchors(roots: &mut RootCertStore, path: &Path) -> Result<usize> {
    let file = std::fs::File::open(path)
        .map_err(|error| ClientError::Tls(format!("cannot read {}: {error}", path.display())))?;
    let mut reader = BufReader::new(file);

    let mut added = 0usize;
    for certificate in rustls_pemfile::certs(&mut reader) {
        let certificate = certificate.map_err(|error| {
            ClientError::Tls(format!("bad certificate in {}: {error}", path.display()))
        })?;
        roots.add(certificate).map_err(|error| {
            ClientError::Tls(format!(
                "rejected certificate in {}: {error}",
                path.display()
            ))
        })?;
        added += 1;
    }

    Ok(added)
}

/// Performs a TLS handshake over an existing socket.
///
/// The handshake is driven to completion here rather than left to the first read, so that
/// a certificate problem is reported by the connection attempt instead of surfacing later
/// as a mysterious failure of whatever command happened to be first.
///
/// # Errors
///
/// Returns [`ClientError::Tls`] if the server name is not a valid DNS name or IP address,
/// or if the handshake fails — including because the certificate could not be verified.
pub fn handshake(
    socket: TcpStream,
    host: &str,
    config: Arc<ClientConfig>,
    options: &TlsOptions,
) -> Result<TlsStream> {
    let name = options.server_name.as_deref().unwrap_or(host);
    let server_name = ServerName::try_from(name.to_owned())
        .map_err(|error| ClientError::Tls(format!("invalid server name {name:?}: {error}")))?;

    let connection = ClientConnection::new(config, server_name)
        .map_err(|error| ClientError::Tls(error.to_string()))?;
    let mut stream = StreamOwned::new(connection, socket);

    while stream.conn.is_handshaking() {
        match stream.conn.complete_io(&mut stream.sock) {
            Ok(_) => {}
            Err(error) => return Err(handshake_error(error)),
        }
    }
    stream.flush().map_err(ClientError::from_io)?;

    if let Some(protocol) = stream.conn.protocol_version() {
        tracing::debug!(?protocol, server_name = %name, "TLS handshake complete");
    }

    Ok(stream)
}

/// Maps a handshake failure, keeping a certificate rejection distinguishable from a
/// network failure.
fn handshake_error(error: std::io::Error) -> ClientError {
    // rustls reports an alert as an io::Error wrapping rustls::Error.
    match error.get_ref().map(ToString::to_string) {
        Some(detail) => ClientError::Tls(detail),
        None => match error.kind() {
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => ClientError::Timeout,
            std::io::ErrorKind::UnexpectedEof => ClientError::Tls(
                "the server closed the connection during the TLS handshake; \
                 is this port really using TLS?"
                    .to_owned(),
            ),
            _ => ClientError::Io(error),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_configuration_from_the_built_in_roots() {
        assert!(client_config(&TlsOptions::new()).is_ok());
    }

    #[test]
    fn reports_a_missing_ca_file_instead_of_ignoring_it() {
        let options = TlsOptions::new().extra_ca_file("/nonexistent/ca.pem");
        let error = client_config(&options).unwrap_err();
        assert!(
            matches!(&error, ClientError::Tls(message) if message.contains("cannot read")),
            "got {error}"
        );
    }

    #[test]
    fn reports_a_ca_file_with_no_certificates() {
        let path = std::env::temp_dir().join("nntp-client-empty-ca.pem");
        std::fs::write(&path, b"this is not a certificate\n").unwrap();

        let options = TlsOptions::new().extra_ca_file(&path);
        let error = client_config(&options).unwrap_err();
        assert!(
            matches!(&error, ClientError::Tls(message) if message.contains("no certificates")),
            "got {error}"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn options_are_chainable() {
        let options = TlsOptions::new()
            .extra_ca_file("/tmp/ca.pem")
            .server_name("news.example.org");
        assert_eq!(options.server_name.as_deref(), Some("news.example.org"));
        assert!(options.extra_ca_file.is_some());
    }

    #[test]
    fn rejects_an_unusable_server_name() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let socket = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let config = client_config(&TlsOptions::new()).unwrap();

        let error = handshake(socket, "not a host name", config, &TlsOptions::new()).unwrap_err();
        assert!(
            matches!(&error, ClientError::Tls(message) if message.contains("invalid server name")),
            "got {error}"
        );
    }
}
