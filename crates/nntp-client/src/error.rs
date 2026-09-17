//! Errors produced by the client layer.

use std::io;

use nntp_proto::{ProtoError, ResponseCode, StatusLine};

/// Convenience alias for results produced by this crate.
pub type Result<T> = core::result::Result<T, ClientError>;

/// Anything that can go wrong while talking to a news server.
///
/// The variants are split by what a caller can *do* about them: retry the command, ask for
/// credentials, fall back to an older command, pick a different article, or give up on the
/// connection. [`ClientError::is_connection_fatal`] answers the last of those.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ClientError {
    /// The transport failed.
    #[error("transport error: {0}")]
    Io(#[from] io::Error),

    /// The peer closed the connection, or it was closed mid-response.
    #[error("connection closed by the server{}", context_suffix(.0))]
    ConnectionClosed(Option<&'static str>),

    /// A read or write exceeded its timeout.
    #[error("timed out waiting for the server")]
    Timeout,

    /// The server's reply could not be parsed.
    #[error("protocol error: {0}")]
    Proto(#[from] ProtoError),

    /// A response line exceeded the configured limit.
    #[error("response line exceeded the {limit}-octet limit")]
    LineTooLong {
        /// The limit that was exceeded.
        limit: usize,
    },

    /// A multi-line block exceeded the configured limit.
    #[error("{what} exceeded the configured limit of {limit}")]
    BlockTooLarge {
        /// Which limit was hit: `block size in octets` or `block line count`.
        what: &'static str,
        /// The limit that was exceeded.
        limit: usize,
    },

    /// The server replied with a valid but unexpected status code.
    #[error("unexpected response to {command}: expected {expected}, got {code} {text:?}")]
    UnexpectedResponse {
        /// The command that was sent.
        command: &'static str,
        /// What was expected, for example `211`.
        expected: &'static str,
        /// The status code actually received.
        code: ResponseCode,
        /// The text that followed the status code.
        text: String,
    },

    /// The server requires authentication before this command (code 480).
    #[error("the server requires authentication: {text:?}")]
    AuthenticationRequired {
        /// The server's message.
        text: String,
    },

    /// Authentication was attempted and rejected (codes 481, 482, 502 during login).
    #[error("authentication rejected: {text:?}")]
    AuthenticationRejected {
        /// The server's message.
        text: String,
    },

    /// Credentials would have been sent in the clear and the caller did not allow that.
    #[error(
        "refusing to send credentials over an unencrypted connection; \
         use TLS or set allow_plaintext_auth"
    )]
    PlaintextAuthenticationRefused,

    /// The group does not exist on this server (code 411).
    #[error("no such newsgroup: {group}")]
    NoSuchGroup {
        /// The group that was requested.
        group: String,
    },

    /// A command needed a selected group and none was selected (code 412).
    #[error("{command} needs a newsgroup to be selected first")]
    NoGroupSelected {
        /// The command that was refused.
        command: &'static str,
    },

    /// The article does not exist, or no article is selected (codes 420, 423, 430).
    #[error("no such article: {what}")]
    NoSuchArticle {
        /// What was requested, for display.
        what: String,
    },

    /// The server does not implement the command (codes 500, 501).
    ///
    /// Not necessarily an error: it is the signal to fall back from `OVER` to `XOVER`, or
    /// to stop offering a feature.
    #[error("the server does not support {command}")]
    CommandNotSupported {
        /// The command that was rejected.
        command: &'static str,
    },

    /// The server declined to act, permanently or for now (other 4xx and 5xx codes).
    #[error("server refused {command}: {code} {text:?}")]
    Server {
        /// The command that was refused.
        command: &'static str,
        /// The status code.
        code: ResponseCode,
        /// The server's message.
        text: String,
    },

    /// TLS negotiation failed.
    #[error("TLS error: {0}")]
    Tls(String),

    /// The requested transport is not available in this build.
    #[error("{0} support was not compiled in; rebuild with the \"tls\" feature")]
    FeatureNotCompiled(&'static str),
}

impl ClientError {
    /// Builds the right error for a failure status line.
    ///
    /// Mapping codes to meanings in one place keeps every command's error handling
    /// consistent, and keeps the "which code means retry?" question answerable.
    pub(crate) fn from_status(command: &'static str, line: &StatusLine) -> Self {
        use nntp_proto::response::codes;

        let text = line.text.clone();
        match line.code {
            codes::AUTH_REQUIRED => Self::AuthenticationRequired { text },
            codes::AUTH_REJECTED | codes::AUTH_OUT_OF_SEQUENCE => {
                Self::AuthenticationRejected { text }
            }
            codes::NO_SUCH_GROUP => Self::NoSuchGroup {
                group: line.args().next().unwrap_or_default().to_owned(),
            },
            codes::NO_GROUP_SELECTED => Self::NoGroupSelected { command },
            codes::NO_ARTICLE_SELECTED
            | codes::NO_SUCH_ARTICLE_NUMBER
            | codes::NO_SUCH_ARTICLE_ID => Self::NoSuchArticle {
                what: if text.is_empty() {
                    command.to_owned()
                } else {
                    text
                },
            },
            // Only 500 and 503 say anything about the command itself. A 501 is a
            // complaint about *these arguments* — a server that rejects an open-ended
            // OVER range answers 501 — so it must not be read as "this server has no
            // OVER", or one bad request would disable the feature for the session.
            codes::UNKNOWN_COMMAND | codes::FEATURE_NOT_SUPPORTED => {
                Self::CommandNotSupported { command }
            }
            code => Self::Server {
                command,
                code,
                text,
            },
        }
    }

    /// Whether the connection is unusable and must be re-established.
    ///
    /// A protocol or framing error means the client has lost track of where it is in the
    /// stream; continuing would read one response as another. A `NoSuchGroup` does not.
    pub fn is_connection_fatal(&self) -> bool {
        match self {
            Self::Io(_)
            | Self::ConnectionClosed(_)
            | Self::Timeout
            | Self::Proto(_)
            | Self::LineTooLong { .. }
            | Self::BlockTooLarge { .. }
            | Self::Tls(_) => true,

            Self::UnexpectedResponse { .. }
            | Self::AuthenticationRequired { .. }
            | Self::AuthenticationRejected { .. }
            | Self::PlaintextAuthenticationRefused
            | Self::NoSuchGroup { .. }
            | Self::NoGroupSelected { .. }
            | Self::NoSuchArticle { .. }
            | Self::CommandNotSupported { .. }
            | Self::Server { .. }
            | Self::FeatureNotCompiled(_) => false,
        }
    }

    /// Whether retrying the same command later might succeed.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Io(_) | Self::ConnectionClosed(_) | Self::Timeout => true,
            Self::Server { code, .. } => code.as_u16() < 500,
            _ => false,
        }
    }

    /// Whether the caller should obtain credentials and try again.
    pub fn needs_authentication(&self) -> bool {
        matches!(self, Self::AuthenticationRequired { .. })
    }

    /// Wraps an IO error, distinguishing a timeout from a real failure.
    ///
    /// A socket with a read timeout reports `WouldBlock` on Unix and `TimedOut` on
    /// Windows, so both have to be treated as a timeout.
    pub(crate) fn from_io(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => Self::Timeout,
            io::ErrorKind::UnexpectedEof => Self::ConnectionClosed(None),
            _ => Self::Io(error),
        }
    }
}

fn context_suffix(context: &Option<&'static str>) -> String {
    match context {
        Some(context) => format!(" while reading {context}"),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(line: &str) -> StatusLine {
        StatusLine::parse(line.as_bytes()).unwrap()
    }

    #[test]
    fn maps_authentication_codes() {
        assert!(matches!(
            ClientError::from_status("GROUP", &status("480 authentication required")),
            ClientError::AuthenticationRequired { .. }
        ));
        assert!(matches!(
            ClientError::from_status("AUTHINFO PASS", &status("481 bad password")),
            ClientError::AuthenticationRejected { .. }
        ));
        assert!(matches!(
            ClientError::from_status("AUTHINFO PASS", &status("482 out of sequence")),
            ClientError::AuthenticationRejected { .. }
        ));
    }

    #[test]
    fn maps_group_and_article_codes() {
        let error = ClientError::from_status("GROUP", &status("411 no.such.group"));
        assert!(matches!(&error, ClientError::NoSuchGroup { group } if group == "no.such.group"));

        assert!(matches!(
            ClientError::from_status("OVER", &status("412 no newsgroup selected")),
            ClientError::NoGroupSelected { command: "OVER" }
        ));

        for line in [
            "420 no article selected",
            "423 no such number",
            "430 no such id",
        ] {
            assert!(
                matches!(
                    ClientError::from_status("ARTICLE", &status(line)),
                    ClientError::NoSuchArticle { .. }
                ),
                "{line}"
            );
        }
    }

    #[test]
    fn maps_unimplemented_commands_so_callers_can_fall_back() {
        // 500 and 503 are the signal to retry with XOVER instead of OVER.
        assert!(matches!(
            ClientError::from_status("OVER", &status("500 unknown command")),
            ClientError::CommandNotSupported { command: "OVER" }
        ));
        assert!(matches!(
            ClientError::from_status("OVER", &status("503 not supported")),
            ClientError::CommandNotSupported { command: "OVER" }
        ));
    }

    #[test]
    fn a_syntax_error_is_about_the_arguments_not_the_command() {
        // Servers answer 501 to an OVER range with an open upper bound. Treating that as
        // "this server has no OVER" would disable overview for the whole session over one
        // bad request.
        let error = ClientError::from_status("OVER", &status("501 open ranges unsupported"));
        assert!(
            matches!(&error, ClientError::Server { code, .. } if code.as_u16() == 501),
            "got {error:?}"
        );
        assert!(!error.is_connection_fatal());
    }

    #[test]
    fn keeps_unrecognised_codes_intact() {
        let error =
            ClientError::from_status("HELP", &status("403 archive server temporarily offline"));
        match error {
            ClientError::Server { code, text, .. } => {
                assert_eq!(code.as_u16(), 403);
                assert_eq!(text, "archive server temporarily offline");
            }
            other => panic!("expected Server, got {other:?}"),
        }
    }

    #[test]
    fn classifies_what_kills_a_connection() {
        assert!(ClientError::Timeout.is_connection_fatal());
        assert!(ClientError::ConnectionClosed(None).is_connection_fatal());
        assert!(ClientError::LineTooLong { limit: 10 }.is_connection_fatal());
        assert!(!ClientError::from_status("OVER", &status("412 x")).is_connection_fatal());
        assert!(!ClientError::from_status("GROUP", &status("411 x")).is_connection_fatal());
    }

    #[test]
    fn classifies_transient_failures() {
        assert!(ClientError::Timeout.is_transient());
        // 4xx may succeed later; 5xx will not.
        assert!(ClientError::from_status("HELP", &status("400 back later")).is_transient());
        assert!(!ClientError::from_status("HELP", &status("502 go away")).is_transient());
        assert!(!ClientError::PlaintextAuthenticationRefused.is_transient());
    }

    #[test]
    fn reports_when_credentials_are_needed() {
        assert!(ClientError::from_status("GROUP", &status("480 x")).needs_authentication());
        assert!(!ClientError::from_status("GROUP", &status("481 x")).needs_authentication());
    }

    #[test]
    fn a_read_timeout_is_not_reported_as_an_io_failure() {
        let would_block = io::Error::new(io::ErrorKind::WouldBlock, "timed out");
        assert!(matches!(
            ClientError::from_io(would_block),
            ClientError::Timeout
        ));
        let timed_out = io::Error::new(io::ErrorKind::TimedOut, "timed out");
        assert!(matches!(
            ClientError::from_io(timed_out),
            ClientError::Timeout
        ));
        let refused = io::Error::new(io::ErrorKind::ConnectionRefused, "nope");
        assert!(matches!(ClientError::from_io(refused), ClientError::Io(_)));
    }

    #[test]
    fn messages_are_readable() {
        assert_eq!(
            ClientError::ConnectionClosed(Some("a LIST block")).to_string(),
            "connection closed by the server while reading a LIST block"
        );
        assert_eq!(
            ClientError::ConnectionClosed(None).to_string(),
            "connection closed by the server"
        );
        assert!(
            ClientError::PlaintextAuthenticationRefused
                .to_string()
                .contains("allow_plaintext_auth")
        );
    }
}
