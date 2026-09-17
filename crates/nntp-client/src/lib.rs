//! Blocking NNTP client.
//!
//! The client is synchronous and generic over its transport (`Read + Write`), so the same
//! code path is exercised by an in-memory cursor in unit tests, a TCP socket, and a TLS
//! stream. Callers that need concurrency run it on a thread; see
//! [ADR-0003](https://github.com/edusouza/rust-nntp/blob/main/docs/adr/0003-blocking-io-on-a-worker-thread.md).
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod error;

pub use error::{ClientError, Result};
