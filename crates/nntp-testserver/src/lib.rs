//! A fake NNTP server, used to test clients without a real news server.
//!
//! Rationale and scope are in
//! [ADR-0004](https://github.com/edusouza/rust-nntp/blob/main/docs/adr/0004-fake-server-for-tests.md).
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

/// The crate version, exposed so the binary and tests report the same string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
