//! IO-free implementation of the NNTP wire grammar.
//!
//! This crate contains no sockets, no clock and no global state: every entry point is a
//! function over bytes or strings. That makes the grammar testable against input a real
//! server would never send, which is exactly the input that breaks news readers.
//!
//! The layering is described in
//! [ADR-0002](https://github.com/edusouza/rust-nntp/blob/main/docs/adr/0002-layered-workspace.md).
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

/// Placeholder module list; filled in during milestone M1.
pub mod error;

pub use error::{ProtoError, Result};
