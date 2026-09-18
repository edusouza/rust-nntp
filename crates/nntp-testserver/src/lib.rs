//! A fake NNTP server, used to test clients without a real news server.
//!
//! The development and CI environment for this project has no outbound access on TCP 119
//! or 563, and public news servers cannot be asked to emit a malformed response on
//! demand. So the tests bring their own server. The reasoning is in [ADR-0004].
//!
//! # Using it in a test
//!
//! ```
//! use nntp_testserver::TestServer;
//!
//! let server = TestServer::start()?;
//! // Point a client at server.address(); the server stops when it is dropped.
//! assert!(server.port() > 0);
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! # Misbehaving on purpose
//!
//! Every quirk in [`Quirks`] exists because a real server does it:
//!
//! ```
//! use nntp_testserver::{CapabilityProfile, Quirks, ServerConfig, TestServer};
//!
//! // A pre-RFC-3977 server with no CAPABILITIES and no OVER, that also refuses
//! // open-ended ranges — a combination that exists in the wild.
//! let config = ServerConfig::new()
//!     .capabilities(CapabilityProfile::Legacy)
//!     .quirks(Quirks {
//!         reject_open_ended_ranges: true,
//!         ..Quirks::default()
//!     });
//! let server = TestServer::with(nntp_testserver::Corpus::sample(), config)?;
//! # let _ = server;
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! [ADR-0004]: https://github.com/edusouza/rust-nntp/blob/main/docs/adr/0004-fake-server-for-tests.md
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod config;
pub mod corpus;
pub mod server;
pub mod session;
#[cfg(feature = "tls")]
pub mod tls;

pub use config::{CapabilityProfile, Credentials, GreetingMode, Quirks, ServerConfig};
pub use corpus::{Article, Corpus, Group, Posting};
pub use server::{TestServer, TlsMode};
pub use session::{Flow, Outcome, Postbox, PostedArticle, Session};
#[cfg(feature = "tls")]
pub use tls::SelfSignedIdentity;

/// The crate version, so the server can report it in `IMPLEMENTATION`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
