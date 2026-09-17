//! Blocking NNTP client.
//!
//! The client is synchronous and generic over its transport (`Read + Write`), so the same
//! code path is exercised by an in-memory cursor in unit tests, a TCP socket, and a TLS
//! stream. Callers that need concurrency run it on a thread; the reasoning is in
//! [ADR-0003].
//!
//! # Layers
//!
//! - [`connector`] — sockets: address resolution, timeouts, transports, `STARTTLS`.
//! - [`tls`] — rustls configuration and the handshake (feature `tls`, on by default).
//! - [`connection`] — framing: status lines and multi-line blocks, with size limits.
//! - [`client`] — the conversation: one method per command, plus the session state they
//!   depend on (capabilities, selected group, authentication, which overview command
//!   works on this server).
//!
//! # Example
//!
//! ```no_run
//! use nntp_client::{ConnectOptions, connector};
//! use nntp_proto::{GroupName, Range};
//!
//! let mut client = connector::connect(&ConnectOptions::new("news.example.org"))?;
//! client.handshake()?;
//!
//! let group = client.select_group(&GroupName::parse("comp.lang.rust")?)?;
//! if let Some((low, high)) = group.range() {
//!     let newest = Range::between(high.saturating_sub(49).max(low), high);
//!     for record in client.overview(newest)?.entries {
//!         println!("{:>8}  {}", record.number, record.subject);
//!     }
//! }
//! client.quit()?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # What it does not do
//!
//! No reconnection, no connection pooling and no retries. Those are policy, and policy
//! belongs to the application: a UI wants to tell the user that the connection dropped,
//! a batch job wants to retry silently, and neither is served by the library guessing.
//! [`ClientError::is_connection_fatal`] and [`ClientError::is_transient`] are there to
//! make that policy easy to write.
//!
//! [ADR-0003]: https://github.com/edusouza/rust-nntp/blob/main/docs/adr/0003-blocking-io-on-a-worker-thread.md
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod cancel;
pub mod client;
pub mod connection;
pub mod connector;
pub mod error;
pub mod limits;
#[cfg(feature = "tls")]
pub mod tls;

pub use cancel::Cancel;
pub use client::{ArticleId, Client, ClientOptions, Greeting};
pub use connection::Connection;
pub use connector::{ConnectOptions, DEFAULT_PORT, DEFAULT_TLS_PORT, Security, Transport};
pub use error::{ClientError, Result};
pub use limits::Limits;
#[cfg(feature = "tls")]
pub use tls::TlsOptions;

/// Everything this crate needs from `nntp-proto`, re-exported so callers do not have to
/// depend on it directly.
pub use nntp_proto as proto;
