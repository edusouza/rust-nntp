//! Establishing a TCP connection to a news server.
//!
//! Separated from [`crate::Client`] because everything here is about sockets — address
//! resolution, timeouts, TLS — while the client is about the conversation. Keeping them
//! apart is what lets the client be tested over an in-memory stream.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::client::{Client, ClientOptions};
use crate::error::{ClientError, Result};
use crate::limits::Limits;

/// The default NNTP port (RFC 3977 §9.3).
pub const DEFAULT_PORT: u16 = 119;

/// The default port for implicit TLS (RFC 4642).
pub const DEFAULT_TLS_PORT: u16 = 563;

/// How to reach a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectOptions {
    /// Host name or address.
    pub host: String,
    /// TCP port.
    pub port: u16,
    /// How long to wait for the TCP handshake.
    pub connect_timeout: Duration,
    /// How long to wait for data once connected.
    ///
    /// Applies per read, not per command: a `LIST` that streams for a minute is fine as
    /// long as no single gap exceeds this. It is what stops a half-open connection from
    /// hanging the caller forever.
    pub read_timeout: Duration,
    /// How long to wait for a write to complete.
    pub write_timeout: Duration,
    /// Response size limits.
    pub limits: Limits,
    /// Disable Nagle's algorithm.
    ///
    /// On by default: NNTP is a request/response protocol with small commands, and
    /// waiting to coalesce them adds latency to every single round trip.
    pub no_delay: bool,
}

impl ConnectOptions {
    /// Options for a plaintext connection to `host` on the default port.
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port: DEFAULT_PORT,
            connect_timeout: Duration::from_secs(20),
            read_timeout: Duration::from_secs(60),
            write_timeout: Duration::from_secs(30),
            limits: Limits::default(),
            no_delay: true,
        }
    }

    /// Sets the port.
    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Sets the connect timeout.
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Sets the read timeout.
    pub fn read_timeout(mut self, timeout: Duration) -> Self {
        self.read_timeout = timeout;
        self
    }

    /// Sets the write timeout.
    pub fn write_timeout(mut self, timeout: Duration) -> Self {
        self.write_timeout = timeout;
        self
    }

    /// Sets the response size limits.
    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// `host:port`, for logging and for TLS server-name verification.
    pub fn authority(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// A transport that a [`Client`] can run over.
///
/// An enum rather than a boxed trait object so that the compiler can still inline the
/// plaintext path, which is the hot one for `LIST` and `OVER`.
#[derive(Debug)]
#[non_exhaustive]
pub enum Transport {
    /// An unencrypted TCP connection.
    Plain(TcpStream),
}

impl Read for Transport {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.read(buf),
        }
    }
}

impl Write for Transport {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Plain(stream) => stream.flush(),
        }
    }
}

impl Transport {
    /// Whether this transport encrypts its traffic.
    pub fn is_encrypted(&self) -> bool {
        match self {
            Self::Plain(_) => false,
        }
    }
}

/// Opens a TCP connection and reads the server's greeting.
///
/// Does not negotiate capabilities; call [`Client::handshake`] next.
///
/// # Errors
///
/// Returns [`ClientError::Io`] if the host cannot be resolved or connected to,
/// [`ClientError::Timeout`] if the handshake takes too long, and the errors of
/// [`Client::new`] if the greeting is missing or is a refusal.
pub fn connect(options: &ConnectOptions) -> Result<Client<Transport>> {
    let stream = connect_tcp(options)?;
    let encrypted = stream.is_encrypted();

    Client::with_options(
        stream,
        ClientOptions {
            limits: options.limits,
            encrypted,
        },
    )
}

/// Opens the TCP socket and applies the socket options.
fn connect_tcp(options: &ConnectOptions) -> Result<Transport> {
    let authority = options.authority();
    tracing::debug!(server = %authority, "connecting");

    // Resolution can return several addresses (IPv6 and IPv4, or a round-robin set);
    // each is tried in turn so that one dead address does not fail the connection.
    let addresses: Vec<_> = authority
        .to_socket_addrs()
        .map_err(ClientError::from_io)?
        .collect();

    if addresses.is_empty() {
        return Err(ClientError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{authority} resolved to no addresses"),
        )));
    }

    let mut last_error = None;
    for address in addresses {
        match TcpStream::connect_timeout(&address, options.connect_timeout) {
            Ok(stream) => {
                stream
                    .set_read_timeout(Some(options.read_timeout))
                    .map_err(ClientError::from_io)?;
                stream
                    .set_write_timeout(Some(options.write_timeout))
                    .map_err(ClientError::from_io)?;
                // Failing to disable Nagle costs latency, not correctness.
                if options.no_delay {
                    if let Err(error) = stream.set_nodelay(true) {
                        tracing::debug!(%error, "could not disable Nagle's algorithm");
                    }
                }
                tracing::debug!(%address, "connected");
                return Ok(Transport::Plain(stream));
            }
            Err(error) => {
                tracing::debug!(%address, %error, "connection attempt failed");
                last_error = Some(error);
            }
        }
    }

    Err(ClientError::from_io(last_error.unwrap_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "no addresses to try")
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_options_with_sane_defaults() {
        let options = ConnectOptions::new("news.example.org");
        assert_eq!(options.port, DEFAULT_PORT);
        assert_eq!(options.authority(), "news.example.org:119");
        assert!(options.no_delay);
        assert!(options.connect_timeout.as_secs() > 0);
        assert!(options.read_timeout > options.connect_timeout);
    }

    #[test]
    fn options_are_chainable() {
        let options = ConnectOptions::new("example.org")
            .port(DEFAULT_TLS_PORT)
            .connect_timeout(Duration::from_secs(1))
            .read_timeout(Duration::from_secs(2))
            .write_timeout(Duration::from_secs(3))
            .limits(Limits::SMALL);

        assert_eq!(options.authority(), "example.org:563");
        assert_eq!(options.connect_timeout, Duration::from_secs(1));
        assert_eq!(options.read_timeout, Duration::from_secs(2));
        assert_eq!(options.write_timeout, Duration::from_secs(3));
        assert_eq!(options.limits, Limits::SMALL);
    }

    #[test]
    fn reports_a_refused_connection_rather_than_hanging() {
        // Port 1 on the loopback interface is not listening.
        let options = ConnectOptions::new("127.0.0.1")
            .port(1)
            .connect_timeout(Duration::from_millis(500));
        let error = connect(&options).unwrap_err();
        assert!(error.is_connection_fatal());
    }

    #[test]
    fn reports_an_unresolvable_host() {
        let options = ConnectOptions::new("invalid.invalid").port(119);
        assert!(connect(&options).is_err());
    }

    #[test]
    fn a_plain_transport_is_not_encrypted() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stream = TcpStream::connect(address).unwrap();
        assert!(!Transport::Plain(stream).is_encrypted());
    }
}
