//! The listener: accepts connections and runs a [`Session`] on each.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::config::ServerConfig;
use crate::corpus::Corpus;
use crate::session::{Outcome, Session};
#[cfg(feature = "tls")]
use crate::tls::SelfSignedIdentity;

/// How often the accept loop checks whether it has been asked to stop.
const ACCEPT_POLL: Duration = Duration::from_millis(10);

/// A running test server.
///
/// Binds to an ephemeral port on the loopback interface, so any number of tests can run
/// in parallel without coordinating port numbers. Shuts down when dropped.
#[derive(Debug)]
pub struct TestServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    accept_loop: Option<JoinHandle<()>>,
    #[cfg(feature = "tls")]
    identity: Option<SelfSignedIdentity>,
}

/// How a server protects its connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TlsMode {
    /// Plaintext only.
    #[default]
    Disabled,
    /// TLS from the first byte.
    Implicit,
    /// Plaintext until the client sends `STARTTLS`.
    StartTls,
}

impl TestServer {
    /// Starts a server with the sample corpus and default configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the loopback socket cannot be bound.
    pub fn start() -> std::io::Result<Self> {
        Self::with(Corpus::sample(), ServerConfig::new())
    }

    /// Starts a server with a specific corpus and configuration on an ephemeral port.
    ///
    /// # Errors
    ///
    /// Returns an error if the loopback socket cannot be bound or put into non-blocking
    /// mode.
    pub fn with(corpus: Corpus, config: ServerConfig) -> std::io::Result<Self> {
        Self::with_port(0, corpus, config)
    }

    /// Starts a server on a specific port; `0` asks the operating system for a free one.
    ///
    /// Tests should use [`Self::with`] so that they can run in parallel. A fixed port is
    /// for the standalone binary, where a human needs to know where to point a client.
    ///
    /// # Errors
    ///
    /// Returns an error if the port is in use, or if the socket cannot be put into
    /// non-blocking mode.
    pub fn with_port(port: u16, corpus: Corpus, config: ServerConfig) -> std::io::Result<Self> {
        #[cfg(feature = "tls")]
        return Self::build(port, corpus, config, None);
        #[cfg(not(feature = "tls"))]
        return Self::build(port, corpus, config);
    }

    /// Starts a server that serves over TLS, using a certificate generated at start-up.
    ///
    /// The certificate names `localhost`, and [`Self::ca_pem`] returns the authority a
    /// client must be told to trust. Verification on the client side stays on: this is a
    /// way to test the TLS path, not a way to skip it.
    ///
    /// # Errors
    ///
    /// Returns an error if the socket cannot be bound or the certificate cannot be
    /// generated.
    #[cfg(feature = "tls")]
    pub fn with_tls(corpus: Corpus, config: ServerConfig, mode: TlsMode) -> std::io::Result<Self> {
        let identity = SelfSignedIdentity::generate("localhost").map_err(|error| {
            std::io::Error::other(format!("could not generate a test certificate: {error}"))
        })?;

        let config = match mode {
            TlsMode::StartTls => config.starttls(true),
            TlsMode::Disabled | TlsMode::Implicit => config,
        };

        Self::build(0, corpus, config, Some((identity, mode)))
    }

    /// The PEM-encoded certificate authority a client must trust, for a TLS server.
    #[cfg(feature = "tls")]
    pub fn ca_pem(&self) -> Option<&str> {
        self.identity
            .as_ref()
            .map(|identity| identity.ca_pem.as_str())
    }

    /// Writes [`Self::ca_pem`] to a file and returns the path, for clients that take a
    /// path rather than bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if there is no TLS identity or the file cannot be written.
    #[cfg(feature = "tls")]
    pub fn write_ca_pem(&self, path: &std::path::Path) -> std::io::Result<()> {
        let identity = self
            .identity
            .as_ref()
            .ok_or_else(|| std::io::Error::other("this server is not using TLS"))?;
        identity.write_ca_pem(path)
    }

    #[cfg(not(feature = "tls"))]
    fn build(port: u16, corpus: Corpus, config: ServerConfig) -> std::io::Result<Self> {
        Self::spawn(port, corpus, config)
    }

    #[cfg(feature = "tls")]
    fn build(
        port: u16,
        corpus: Corpus,
        config: ServerConfig,
        tls: Option<(SelfSignedIdentity, TlsMode)>,
    ) -> std::io::Result<Self> {
        Self::spawn(port, corpus, config, tls)
    }

    fn spawn(
        port: u16,
        corpus: Corpus,
        config: ServerConfig,
        #[cfg(feature = "tls")] tls: Option<(SelfSignedIdentity, TlsMode)>,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let address = listener.local_addr()?;
        // Non-blocking accept so the loop can notice the stop flag; a blocking accept
        // would keep the thread alive until the next connection arrived.
        listener.set_nonblocking(true)?;

        let stop = Arc::new(AtomicBool::new(false));

        #[cfg(feature = "tls")]
        let (identity, mode, tls_config) = match tls {
            None => (None, TlsMode::Disabled, None),
            Some((identity, mode)) => {
                let config = identity.server_config().map_err(std::io::Error::other)?;
                (Some(identity), mode, Some(config))
            }
        };

        let accept_loop = {
            let stop = Arc::clone(&stop);
            let shared = Arc::new(Shared {
                corpus,
                config,
                #[cfg(feature = "tls")]
                mode,
                #[cfg(feature = "tls")]
                tls_config,
            });
            std::thread::Builder::new()
                .name("nntp-testserver".to_owned())
                .spawn(move || accept_loop(listener, &stop, &shared))?
        };

        tracing::debug!(%address, "test server listening");
        Ok(Self {
            address,
            stop,
            accept_loop: Some(accept_loop),
            #[cfg(feature = "tls")]
            identity,
        })
    }

    /// The address the server is listening on.
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// The port the server is listening on.
    pub fn port(&self) -> u16 {
        self.address.port()
    }

    /// `127.0.0.1:port`, for passing to a client.
    pub fn authority(&self) -> String {
        self.address.to_string()
    }

    /// Stops the server and waits for the accept loop to finish.
    pub fn shutdown(mut self) {
        self.stop_and_join();
    }

    fn stop_and_join(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.accept_loop.take() {
            // A panic in the accept loop is worth surfacing, but not worth turning a
            // test teardown into a second panic.
            if handle.join().is_err() {
                tracing::error!("test server accept loop panicked");
            }
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

/// What every session thread needs.
struct Shared {
    corpus: Corpus,
    config: ServerConfig,
    #[cfg(feature = "tls")]
    mode: TlsMode,
    #[cfg(feature = "tls")]
    tls_config: Option<Arc<rustls::ServerConfig>>,
}

fn accept_loop(listener: TcpListener, stop: &AtomicBool, shared: &Arc<Shared>) {
    let mut sessions: Vec<JoinHandle<()>> = Vec::new();

    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, peer)) => {
                tracing::debug!(%peer, "test server accepted a connection");
                let shared = Arc::clone(shared);
                match std::thread::Builder::new()
                    .name("nntp-testserver-session".to_owned())
                    .spawn(move || serve(stream, &shared))
                {
                    Ok(handle) => sessions.push(handle),
                    Err(error) => tracing::error!(%error, "could not spawn a session thread"),
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(ACCEPT_POLL);
            }
            Err(error) => {
                tracing::error!(%error, "test server accept failed");
                break;
            }
        }

        sessions.retain(|handle| !handle.is_finished());
    }

    // Let in-flight sessions finish so a client's last read does not race the shutdown.
    for handle in sessions {
        let _ = handle.join();
    }
}

fn serve(mut stream: TcpStream, shared: &Arc<Shared>) {
    // The listener is non-blocking so the accept loop can notice the stop flag. On Windows
    // and on the BSDs — macOS included — an accepted socket *inherits* that flag, while on
    // Linux it does not. Without this line the session's first read returns `WouldBlock`
    // immediately, the thread gives up, and the client sees the connection reset before
    // the greeting arrives. It passes on Linux and fails everywhere else.
    if let Err(error) = stream.set_nonblocking(false) {
        tracing::error!(%error, "could not put the accepted socket into blocking mode");
        return;
    }

    // A test that hangs is worse than a test that fails.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
    let _ = stream.set_nodelay(true);

    let mut session = Session::new(&shared.corpus, &shared.config);

    #[cfg(feature = "tls")]
    if shared.mode == TlsMode::Implicit {
        let Some(config) = shared.tls_config.clone() else {
            tracing::error!("implicit TLS was requested with no TLS configuration");
            return;
        };
        let mut tls = match accept_tls(stream, config) {
            Ok(tls) => tls,
            Err(error) => {
                tracing::debug!(%error, "TLS handshake failed");
                return;
            }
        };
        session.set_encrypted(true);
        if let Err(error) = session.run_duplex(&mut tls) {
            tracing::debug!(%error, "test server session ended");
        }
        return;
    }

    match session.run_duplex(&mut stream) {
        Ok(Outcome::Closed) => {}
        Ok(Outcome::UpgradeToTls) => upgrade(stream, &mut session, shared),
        Err(error) => {
            // A client that disconnects abruptly is normal in tests, not a failure.
            tracing::debug!(%error, "test server session ended");
        }
    }
}

/// Completes a `STARTTLS` upgrade and resumes the session over the encrypted stream.
#[cfg(feature = "tls")]
fn upgrade(stream: TcpStream, session: &mut Session<'_>, shared: &Arc<Shared>) {
    let Some(config) = shared.tls_config.clone() else {
        tracing::error!("STARTTLS was accepted with no TLS configuration");
        return;
    };

    let mut tls = match accept_tls(stream, config) {
        Ok(tls) => tls,
        Err(error) => {
            tracing::debug!(%error, "STARTTLS handshake failed");
            return;
        }
    };

    session.set_encrypted(true);
    // No second greeting: RFC 4642 §2.2.
    if let Err(error) = session.resume_duplex(&mut tls) {
        tracing::debug!(%error, "test server session ended after STARTTLS");
    }
}

#[cfg(not(feature = "tls"))]
fn upgrade(_stream: TcpStream, _session: &mut Session<'_>, _shared: &Arc<Shared>) {
    tracing::error!("STARTTLS was accepted in a build without TLS support");
}

/// Performs the server side of a TLS handshake, driving it to completion so that a failure
/// is reported here rather than as a confusing read error later.
#[cfg(feature = "tls")]
fn accept_tls(
    mut stream: TcpStream,
    config: Arc<rustls::ServerConfig>,
) -> std::io::Result<rustls::StreamOwned<rustls::ServerConnection, TcpStream>> {
    let mut connection = rustls::ServerConnection::new(config).map_err(std::io::Error::other)?;
    connection.complete_io(&mut stream)?;
    Ok(rustls::StreamOwned::new(connection, stream))
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};

    use super::*;

    /// A minimal client: send a line, read a line.
    struct Probe {
        reader: BufReader<TcpStream>,
        writer: TcpStream,
    }

    impl Probe {
        fn connect(server: &TestServer) -> Self {
            let stream = TcpStream::connect(server.address()).expect("connect");
            let writer = stream.try_clone().expect("clone");
            Self {
                reader: BufReader::new(stream),
                writer,
            }
        }

        fn read_line(&mut self) -> String {
            let mut line = String::new();
            self.reader.read_line(&mut line).expect("read");
            line.trim_end().to_owned()
        }

        fn send(&mut self, command: &str) {
            write!(self.writer, "{command}\r\n").expect("write");
            self.writer.flush().expect("flush");
        }
    }

    #[test]
    fn serves_a_session_over_a_real_socket() {
        let server = TestServer::start().expect("start");
        let mut probe = Probe::connect(&server);

        assert!(probe.read_line().starts_with("200 "));
        probe.send("GROUP misc.test");
        assert_eq!(probe.read_line(), "211 3 1 3 misc.test");
        probe.send("QUIT");
        assert!(probe.read_line().starts_with("205 "));
    }

    #[test]
    fn binds_an_ephemeral_port_so_tests_can_run_in_parallel() {
        let first = TestServer::start().expect("start");
        let second = TestServer::start().expect("start");
        assert_ne!(first.port(), second.port());
        assert!(first.authority().starts_with("127.0.0.1:"));
    }

    #[test]
    fn an_accepted_socket_is_blocking() {
        // The listener is non-blocking; the accepted socket must not be. Windows and the
        // BSDs inherit the flag, Linux does not, so this invariant is invisible on Linux
        // and fatal everywhere else. The delay is what makes it observable: a
        // non-blocking server would have given up long before the command arrives.
        let server = TestServer::start().expect("start");
        let mut probe = Probe::connect(&server);
        assert!(probe.read_line().starts_with("200 "));

        std::thread::sleep(Duration::from_millis(250));
        probe.send("DATE");
        assert!(
            probe.read_line().starts_with("111 "),
            "the session did not survive an idle client"
        );
    }

    #[test]
    fn accepts_more_than_one_connection() {
        let server = TestServer::start().expect("start");

        let mut first = Probe::connect(&server);
        let mut second = Probe::connect(&server);
        assert!(first.read_line().starts_with("200 "));
        assert!(second.read_line().starts_with("200 "));

        first.send("DATE");
        second.send("DATE");
        assert!(first.read_line().starts_with("111 "));
        assert!(second.read_line().starts_with("111 "));
    }

    #[test]
    fn shuts_down_without_hanging() {
        let server = TestServer::start().expect("start");
        let address = server.address();
        server.shutdown();

        // After shutdown the port is no longer accepting.
        std::thread::sleep(Duration::from_millis(50));
        assert!(TcpStream::connect(address).is_err());
    }

    #[test]
    fn dropping_the_server_stops_it() {
        let address = {
            let server = TestServer::start().expect("start");
            server.address()
        };
        std::thread::sleep(Duration::from_millis(50));
        assert!(TcpStream::connect(address).is_err());
    }
}
