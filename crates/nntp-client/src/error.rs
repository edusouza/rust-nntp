//! Errors produced by the client layer.

use std::io;

use nntp_proto::ProtoError;

/// Convenience alias for results produced by this crate.
pub type Result<T> = core::result::Result<T, ClientError>;

/// Anything that can go wrong while talking to a news server.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ClientError {
    /// The transport failed.
    #[error("transport error")]
    Io(#[from] io::Error),

    /// The server's reply was well-formed at the transport level but not at the protocol
    /// level.
    #[error("protocol error")]
    Proto(#[from] ProtoError),

    /// The server replied with a valid but unexpected status code.
    #[error("unexpected response to {command}: expected {expected}, got {code} {text:?}")]
    UnexpectedResponse {
        /// The command that was sent.
        command: &'static str,
        /// A human-readable description of what was expected, e.g. `211`.
        expected: &'static str,
        /// The status code actually received.
        code: u16,
        /// The text that followed the status code.
        text: String,
    },
}
