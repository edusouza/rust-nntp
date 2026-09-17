//! The listener: accepts connections and runs a [`Session`] on each.

use std::io::BufReader;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::config::ServerConfig;
use crate::corpus::Corpus;
use crate::session::Session;

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
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let address = listener.local_addr()?;
        // Non-blocking accept so the loop can notice the stop flag; a blocking accept
        // would keep the thread alive until the next connection arrived.
        listener.set_nonblocking(true)?;

        let stop = Arc::new(AtomicBool::new(false));
        let accept_loop = {
            let stop = Arc::clone(&stop);
            let shared = Arc::new((corpus, config));
            std::thread::Builder::new()
                .name("nntp-testserver".to_owned())
                .spawn(move || accept_loop(listener, &stop, &shared))?
        };

        tracing::debug!(%address, "test server listening");
        Ok(Self {
            address,
            stop,
            accept_loop: Some(accept_loop),
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

fn accept_loop(listener: TcpListener, stop: &AtomicBool, shared: &Arc<(Corpus, ServerConfig)>) {
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

fn serve(stream: TcpStream, shared: &Arc<(Corpus, ServerConfig)>) {
    let (corpus, config) = (&shared.0, &shared.1);

    // A test that hangs is worse than a test that fails.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
    let _ = stream.set_nodelay(true);

    let Ok(write_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let mut writer = write_half;

    let mut session = Session::new(corpus, config);
    if let Err(error) = session.run(&mut reader, &mut writer) {
        // A client that disconnects abruptly is normal in tests, not a failure.
        tracing::debug!(%error, "test server session ended");
    }
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
