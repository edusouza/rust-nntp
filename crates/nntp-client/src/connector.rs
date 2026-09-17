//! Establishing a TCP connection to a news server.
//!
//! Separated from [`crate::Client`] because everything here is about sockets — address
//! resolution, timeouts, TLS — while the client is about the conversation. Keeping them
//! apart is what lets the client be tested over an in-memory stream.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::client::{Client, ClientOptions};
#[cfg(feature = "tls")]
use crate::connection::Connection;
use crate::error::{ClientError, Result};
use crate::limits::Limits;
#[cfg(feature = "tls")]
use crate::tls::{self, TlsOptions};

/// The default NNTP port (RFC 3977 §9.3).
pub const DEFAULT_PORT: u16 = 119;

/// The default port for implicit TLS (RFC 4642).
pub const DEFAULT_TLS_PORT: u16 = 563;

/// How the connection is protected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Security {
    /// No encryption. The default port is 119.
    #[default]
    Plain,
    /// TLS from the first byte. The default port is 563 (RFC 4642).
    ImplicitTls,
    /// Start in the clear, then upgrade with `STARTTLS` (RFC 4642 §2).
    ///
    /// Prefer [`Self::ImplicitTls`] where the server offers it: `STARTTLS` exposes the
    /// greeting and the capability list to anyone on the path, and a downgrade attack has
    /// to be detected rather than prevented.
    StartTls,
}

impl Security {
    /// The conventional port for this mode.
    pub const fn default_port(self) -> u16 {
        match self {
            Self::Plain | Self::StartTls => DEFAULT_PORT,
            Self::ImplicitTls => DEFAULT_TLS_PORT,
        }
    }

    /// Whether traffic is encrypted once the connection is established.
    pub const fn is_encrypted(self) -> bool {
        matches!(self, Self::ImplicitTls | Self::StartTls)
    }
}

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
    /// Whether and how to encrypt the connection.
    pub security: Security,
    /// How to verify the server's certificate.
    #[cfg(feature = "tls")]
    pub tls: TlsOptions,
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
            security: Security::Plain,
            #[cfg(feature = "tls")]
            tls: TlsOptions::new(),
            no_delay: true,
        }
    }

    /// Sets the security mode, and the port to that mode's default.
    ///
    /// Call this before [`Self::port`] if both are being set: changing the mode resets the
    /// port, on the grounds that asking for TLS and silently keeping port 119 is a worse
    /// surprise than the reverse.
    #[must_use]
    pub fn security(mut self, security: Security) -> Self {
        self.security = security;
        self.port = security.default_port();
        self
    }

    /// Sets the TLS verification options.
    #[cfg(feature = "tls")]
    #[must_use]
    pub fn tls_options(mut self, options: TlsOptions) -> Self {
        self.tls = options;
        self
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
    /// A TLS connection.
    ///
    /// Boxed because a rustls session is around 1.5 KiB and would otherwise make every
    /// `Transport` — including the plaintext one — that large.
    #[cfg(feature = "tls")]
    Tls(Box<tls::TlsStream>),
}

impl Read for Transport {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.read(buf),
            #[cfg(feature = "tls")]
            Self::Tls(stream) => stream.read(buf),
        }
    }
}

impl Write for Transport {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.write(buf),
            #[cfg(feature = "tls")]
            Self::Tls(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Plain(stream) => stream.flush(),
            #[cfg(feature = "tls")]
            Self::Tls(stream) => stream.flush(),
        }
    }
}

impl Transport {
    /// Whether this transport encrypts its traffic.
    pub fn is_encrypted(&self) -> bool {
        match self {
            Self::Plain(_) => false,
            #[cfg(feature = "tls")]
            Self::Tls(_) => true,
        }
    }

    /// The underlying TCP socket, for setting socket options.
    pub fn socket(&self) -> &TcpStream {
        match self {
            Self::Plain(stream) => stream,
            #[cfg(feature = "tls")]
            Self::Tls(stream) => &stream.sock,
        }
    }
}

/// Opens a connection and reads the server's greeting.
///
/// For [`Security::StartTls`] this also performs the upgrade, so the returned client is
/// already encrypted and its capability list has been re-read.
///
/// Does not negotiate reader mode; call [`Client::handshake`] next.
///
/// # Errors
///
/// Returns [`ClientError::Io`] if the host cannot be resolved or connected to,
/// [`ClientError::Timeout`] if the handshake takes too long, [`ClientError::Tls`] if TLS
/// negotiation or certificate verification fails, and the errors of [`Client::new`] if the
/// greeting is missing or is a refusal.
pub fn connect(options: &ConnectOptions) -> Result<Client<Transport>> {
    match options.security {
        Security::Plain => {
            let transport = Transport::Plain(connect_tcp(options)?);
            build(transport, options, false)
        }
        #[cfg(feature = "tls")]
        Security::ImplicitTls => {
            let socket = connect_tcp(options)?;
            let config = tls::client_config(&options.tls)?;
            let stream = tls::handshake(socket, &options.host, config, &options.tls)?;
            build(Transport::Tls(Box::new(stream)), options, true)
        }
        #[cfg(feature = "tls")]
        Security::StartTls => {
            let transport = Transport::Plain(connect_tcp(options)?);
            let client = build(transport, options, false)?;
            upgrade_with_starttls(client, options)
        }
        #[cfg(not(feature = "tls"))]
        Security::ImplicitTls | Security::StartTls => Err(ClientError::FeatureNotCompiled("TLS")),
    }
}

/// Wraps a transport in a client, reading the greeting.
fn build(
    transport: Transport,
    options: &ConnectOptions,
    encrypted: bool,
) -> Result<Client<Transport>> {
    Client::with_options(
        transport,
        ClientOptions {
            limits: options.limits,
            encrypted,
        },
    )
}

/// Performs the `STARTTLS` exchange and hands back an encrypted client.
///
/// # Errors
///
/// Returns [`ClientError::Tls`] if the server does not offer `STARTTLS`, refuses it, or
/// sends anything after the `382` response, and the errors of [`tls::handshake`] if the
/// handshake itself fails.
#[cfg(feature = "tls")]
pub fn upgrade_with_starttls(
    mut client: Client<Transport>,
    options: &ConnectOptions,
) -> Result<Client<Transport>> {
    // Ask first. A server that does not offer STARTTLS will reject the command, and
    // knowing that before sending it keeps the error message honest.
    let capabilities = client.refresh_capabilities()?;
    if !capabilities.is_empty() && !capabilities.has_starttls() {
        return Err(ClientError::Tls(
            "the server does not offer STARTTLS; use implicit TLS on port 563,              or connect in the clear if that is really what you want"
                .to_owned(),
        ));
    }

    let line = client
        .connection_mut()
        .command(&nntp_proto::Command::StartTls)?;
    if line.code != nntp_proto::response::codes::TLS_CONTINUE {
        return Err(ClientError::Tls(format!(
            "the server refused STARTTLS: {} {}",
            line.code, line.text
        )));
    }

    // RFC 4642 §2.2: the server must send nothing between the 382 and the handshake.
    // Anything buffered here was either injected by someone on the path or sent by a
    // broken server; either way it must not be fed to the TLS layer.
    if client.connection_mut().has_buffered_data() {
        return Err(ClientError::Tls(
            "the server sent data after its STARTTLS response, which RFC 4642 forbids;              refusing to continue"
                .to_owned(),
        ));
    }

    let greeting = client.greeting().clone();
    let transport = client.into_connection().into_inner();

    let socket = match transport {
        Transport::Plain(socket) => socket,
        // Upgrading an already-encrypted connection is a caller bug, and RFC 4642 §2.2
        // forbids a second STARTTLS in any case.
        Transport::Tls(_) => {
            return Err(ClientError::Tls(
                "this connection is already using TLS".to_owned(),
            ));
        }
    };

    let config = tls::client_config(&options.tls)?;
    let stream = tls::handshake(socket, &options.host, config, &options.tls)?;
    let connection = Connection::with_limits(Transport::Tls(Box::new(stream)), options.limits);

    // from_parts discards the capability list read in the clear, as §2.2 requires.
    let mut client = Client::from_parts(connection, greeting, true);
    client.refresh_capabilities()?;
    tracing::debug!("connection upgraded to TLS with STARTTLS");

    Ok(client)
}

/// Opens the TCP socket and applies the socket options.
fn connect_tcp(options: &ConnectOptions) -> Result<TcpStream> {
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
                return Ok(stream);
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
