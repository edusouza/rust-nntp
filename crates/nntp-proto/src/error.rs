//! Errors produced by the protocol layer.

/// Convenience alias for results produced by this crate.
pub type Result<T> = core::result::Result<T, ProtoError>;

/// A byte sequence received from a peer could not be interpreted.
///
/// Every variant is recoverable at the protocol level: the caller may abandon the
/// response, log it, and either resynchronise or close the connection. Nothing in this
/// crate panics on malformed input.
#[derive(Debug, thiserror::Error, PartialEq, Eq, Clone)]
#[non_exhaustive]
pub enum ProtoError {
    /// A status line did not match `3DIGIT [SP text]`.
    #[error("malformed status line: {0}")]
    MalformedStatusLine(String),

    /// A line was expected to contain a field that was absent.
    #[error("{context}: missing field {field}")]
    MissingField {
        /// What was being parsed, e.g. `GROUP response`.
        context: &'static str,
        /// The name of the field that was absent.
        field: &'static str,
    },

    /// A numeric field could not be parsed.
    #[error("{context}: invalid number {value:?}")]
    InvalidNumber {
        /// What was being parsed.
        context: &'static str,
        /// The offending text.
        value: String,
    },

    /// A command argument contained CR or LF and was refused rather than written to the
    /// socket, where it would have injected an additional command.
    #[error("command argument for {command} contains a control character")]
    IllegalCommandArgument {
        /// The command whose argument was refused.
        command: &'static str,
    },
}
