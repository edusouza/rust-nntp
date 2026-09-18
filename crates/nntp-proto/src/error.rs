//! Errors produced by the protocol layer.

/// Convenience alias for results produced by this crate.
pub type Result<T> = core::result::Result<T, ProtoError>;

/// A byte sequence could not be interpreted, or a value could not be encoded.
///
/// Every variant is recoverable at the protocol level: the caller may abandon the
/// response, log it, and either resynchronise or close the connection. Nothing in this
/// crate panics on malformed input, and nothing here indicates a bug in the caller — the
/// data came off a socket.
#[derive(Debug, thiserror::Error, PartialEq, Eq, Clone)]
#[non_exhaustive]
pub enum ProtoError {
    /// A status line did not match `3DIGIT [(SP / HTAB) text]`.
    #[error("malformed status line: {0:?}")]
    MalformedStatusLine(String),

    /// A line was expected to contain a field that was absent.
    #[error("{context}: missing field {field}")]
    MissingField {
        /// What was being parsed, for example `GROUP response`.
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

    /// A command argument contained a control character and was refused rather than
    /// written to the socket, where CR or LF would have injected a further command.
    #[error("argument for {command} contains a control character")]
    IllegalCommandArgument {
        /// The command whose argument was refused.
        command: &'static str,
    },

    /// A draft was refused before it reached the network.
    ///
    /// Everything wrong with it at once, because somebody who has just written an article
    /// wants one trip back to the editor rather than one per mistake.
    #[error("the article cannot be posted: {problems}")]
    UnpostableDraft {
        /// The problems, already formatted and joined.
        problems: String,
    },

    /// The encoded command would exceed the 512-octet limit of RFC 3977 §3.1.
    #[error("{command} command line is {len} octets, over the 512-octet limit")]
    CommandTooLong {
        /// The command that was too long.
        command: &'static str,
        /// The length the encoded line would have had, including CRLF.
        len: usize,
    },

    /// A message-id was not of the form `<id-left@id-right>`, or was too long.
    #[error("invalid message-id: {0:?}")]
    InvalidMessageId(String),

    /// A newsgroup name was empty, too long, or contained a character that is not allowed
    /// in one.
    #[error("invalid newsgroup name: {0:?}")]
    InvalidGroupName(String),

    /// A header field name was empty or contained a character other than printable
    /// US-ASCII excluding colon.
    #[error("invalid header field name: {0:?}")]
    InvalidHeaderName(String),

    /// A wildmat pattern was empty, too long, or contained a non-printable character.
    #[error("invalid wildmat pattern: {0:?}")]
    InvalidWildmat(String),

    /// A date could not be interpreted, even allowing for the obsolete and malformed
    /// forms that appear in practice.
    #[error("invalid date: {0:?}")]
    InvalidDate(String),
}

impl ProtoError {
    /// Whether this error indicates that the *peer* sent something invalid, as opposed to
    /// the caller asking for something that cannot be encoded.
    ///
    /// Useful for deciding what to log and at what level: a bad response from a server is
    /// worth a warning about that server, while an unencodable command is a bug here or a
    /// bad configuration value.
    pub const fn is_peer_fault(&self) -> bool {
        match self {
            Self::MalformedStatusLine(_)
            | Self::MissingField { .. }
            | Self::InvalidNumber { .. }
            | Self::InvalidMessageId(_)
            | Self::InvalidGroupName(_)
            | Self::InvalidHeaderName(_)
            | Self::InvalidDate(_) => true,

            Self::IllegalCommandArgument { .. }
            | Self::CommandTooLong { .. }
            | Self::UnpostableDraft { .. }
            | Self::InvalidWildmat(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_name_the_offending_value() {
        let error = ProtoError::MalformedStatusLine("2xx nope".to_owned());
        assert_eq!(error.to_string(), r#"malformed status line: "2xx nope""#);

        let error = ProtoError::CommandTooLong {
            command: "AUTHINFO PASS",
            len: 600,
        };
        assert_eq!(
            error.to_string(),
            "AUTHINFO PASS command line is 600 octets, over the 512-octet limit"
        );
    }

    #[test]
    fn distinguishes_peer_faults_from_caller_faults() {
        assert!(ProtoError::InvalidDate("x".to_owned()).is_peer_fault());
        assert!(
            !ProtoError::CommandTooLong {
                command: "GROUP",
                len: 513
            }
            .is_peer_fault()
        );
        assert!(
            !ProtoError::IllegalCommandArgument {
                command: "AUTHINFO USER"
            }
            .is_peer_fault()
        );
    }
}
