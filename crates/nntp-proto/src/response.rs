//! Status lines: the single-line reply that begins every NNTP response.
//!
//! RFC 3977 §3.2 defines a response as a three-digit code, optionally followed by a space
//! and human-readable text, terminated by CRLF. Commands that return a multi-line data
//! block send the status line first; the block follows and is handled by
//! [`crate::block`].

use crate::{ProtoError, Result};

/// A parsed status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusLine {
    /// The three-digit response code.
    pub code: ResponseCode,
    /// The text following the code, with the separating space removed.
    ///
    /// Decoded lossily: the text is meant to be human-readable and is never used for
    /// control decisions, so invalid UTF-8 is replaced rather than rejected.
    pub text: String,
}

impl StatusLine {
    /// Parses a status line.
    ///
    /// A trailing CRLF or LF is tolerated but not required; callers that read
    /// line-by-line will normally have stripped it already.
    ///
    /// # Errors
    ///
    /// Returns [`ProtoError::MalformedStatusLine`] if the line does not start with three
    /// ASCII digits followed by either end-of-line or a space or tab.
    pub fn parse(line: &[u8]) -> Result<Self> {
        let line = strip_eol(line);

        let (digits, rest) = line
            .split_at_checked(3)
            .ok_or_else(|| ProtoError::MalformedStatusLine(lossy(line)))?;

        if !digits.iter().all(u8::is_ascii_digit) {
            return Err(ProtoError::MalformedStatusLine(lossy(line)));
        }

        // Three ASCII digits always parse as a u16 below 1000.
        let value = digits
            .iter()
            .fold(0u16, |acc, d| acc * 10 + u16::from(d - b'0'));

        let text = match rest.split_first() {
            None => String::new(),
            // Servers in the wild use a tab here often enough to be worth accepting.
            Some((b' ' | b'\t', tail)) => lossy(tail),
            Some(_) => return Err(ProtoError::MalformedStatusLine(lossy(line))),
        };

        Ok(Self {
            code: ResponseCode(value),
            text,
        })
    }

    /// The whitespace-separated tokens of [`Self::text`].
    ///
    /// Several responses carry machine-readable arguments in the text, for example
    /// `211 1234 3000234 3002322 misc.test`.
    pub fn args(&self) -> impl Iterator<Item = &str> {
        self.text.split_ascii_whitespace()
    }

    /// The `n`th whitespace-separated token of [`Self::text`], zero-indexed.
    ///
    /// # Errors
    ///
    /// Returns [`ProtoError::MissingField`] if there are fewer than `n + 1` tokens.
    pub fn arg(&self, n: usize, context: &'static str, field: &'static str) -> Result<&str> {
        self.args()
            .nth(n)
            .ok_or(ProtoError::MissingField { context, field })
    }

    /// Parses the `n`th token as a `u64`.
    ///
    /// # Errors
    ///
    /// Returns [`ProtoError::MissingField`] if the token is absent and
    /// [`ProtoError::InvalidNumber`] if it is not a decimal number.
    pub fn arg_u64(&self, n: usize, context: &'static str, field: &'static str) -> Result<u64> {
        let raw = self.arg(n, context, field)?;
        raw.parse().map_err(|_| ProtoError::InvalidNumber {
            context,
            value: raw.to_owned(),
        })
    }
}

/// A three-digit NNTP response code.
///
/// The first digit gives the outcome and the second the category of the command it
/// applies to (RFC 3977 §3.2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResponseCode(u16);

impl ResponseCode {
    /// Wraps a raw code without checking that it is three digits.
    ///
    /// Codes outside 100..=599 classify as [`ResponseKind::Unknown`], which callers treat
    /// as a protocol violation.
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// The numeric value.
    pub const fn as_u16(self) -> u16 {
        self.0
    }

    /// The outcome indicated by the first digit.
    pub const fn kind(self) -> ResponseKind {
        match self.0 {
            100..=199 => ResponseKind::Informative,
            200..=299 => ResponseKind::Success,
            300..=399 => ResponseKind::Continue,
            400..=499 => ResponseKind::TransientError,
            500..=599 => ResponseKind::PermanentError,
            _ => ResponseKind::Unknown,
        }
    }

    /// Whether the command succeeded (a 1xx or 2xx code).
    pub const fn is_ok(self) -> bool {
        matches!(
            self.kind(),
            ResponseKind::Informative | ResponseKind::Success
        )
    }

    /// Whether the code reports a failure (4xx or 5xx).
    pub const fn is_error(self) -> bool {
        matches!(
            self.kind(),
            ResponseKind::TransientError | ResponseKind::PermanentError
        )
    }

    /// Whether this response is followed by a multi-line data block.
    ///
    /// This cannot be derived from the code alone — `215` introduces a block after `LIST`
    /// but `211` does so only after `LISTGROUP`, not after `GROUP` — so the decision is
    /// made per command by [`crate::command::Command::expects_data_block`] and this method
    /// only covers the codes that unambiguously carry one.
    pub const fn always_has_data_block(self) -> bool {
        matches!(
            self.0,
            100 | 101 | 215 | 220 | 221 | 222 | 224 | 225 | 230 | 231
        )
    }
}

impl core::fmt::Display for ResponseCode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<u16> for ResponseCode {
    fn from(value: u16) -> Self {
        Self(value)
    }
}

/// The class of a response, taken from the first digit of the code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseKind {
    /// 1xx — informative message.
    Informative,
    /// 2xx — command completed.
    Success,
    /// 3xx — command accepted, send the rest of it.
    Continue,
    /// 4xx — command correct but could not be performed; may succeed later.
    TransientError,
    /// 5xx — command unimplemented, unsupported or syntactically wrong.
    PermanentError,
    /// Not a valid NNTP class.
    Unknown,
}

/// Named response codes used by this crate and its callers.
///
/// Only the codes we act on are listed; servers send others and callers must handle an
/// unrecognised code by class rather than by value.
pub mod codes {
    use super::ResponseCode;

    /// Help text follows.
    pub const HELP_TEXT_FOLLOWS: ResponseCode = ResponseCode::new(100);
    /// Capability list follows.
    pub const CAPABILITIES_FOLLOW: ResponseCode = ResponseCode::new(101);
    /// Server date and time follows.
    pub const SERVER_DATE: ResponseCode = ResponseCode::new(111);
    /// Service available, posting allowed.
    pub const GREETING_POSTING_ALLOWED: ResponseCode = ResponseCode::new(200);
    /// Service available, posting prohibited.
    pub const GREETING_NO_POSTING: ResponseCode = ResponseCode::new(201);
    /// `STARTTLS` accepted, begin the TLS handshake.
    pub const TLS_CONTINUE: ResponseCode = ResponseCode::new(382);
    /// Connection closing.
    pub const CLOSING: ResponseCode = ResponseCode::new(205);
    /// Group selected.
    pub const GROUP_SELECTED: ResponseCode = ResponseCode::new(211);
    /// Information follows (a `LIST` block).
    pub const INFORMATION_FOLLOWS: ResponseCode = ResponseCode::new(215);
    /// Article follows (head and body).
    pub const ARTICLE_FOLLOWS: ResponseCode = ResponseCode::new(220);
    /// Article headers follow.
    pub const HEAD_FOLLOWS: ResponseCode = ResponseCode::new(221);
    /// Article body follows.
    pub const BODY_FOLLOWS: ResponseCode = ResponseCode::new(222);
    /// Article exists (`STAT`), no data block.
    pub const ARTICLE_EXISTS: ResponseCode = ResponseCode::new(223);
    /// Overview information follows.
    pub const OVERVIEW_FOLLOWS: ResponseCode = ResponseCode::new(224);
    /// Header values follow (`HDR`).
    pub const HEADERS_FOLLOW: ResponseCode = ResponseCode::new(225);
    /// List of new articles follows (`NEWNEWS`).
    pub const NEW_ARTICLES_FOLLOW: ResponseCode = ResponseCode::new(230);
    /// List of new newsgroups follows.
    pub const NEW_GROUPS_FOLLOW: ResponseCode = ResponseCode::new(231);
    /// Authentication accepted.
    pub const AUTH_ACCEPTED: ResponseCode = ResponseCode::new(281);
    /// Send the article to be posted.
    pub const SEND_ARTICLE: ResponseCode = ResponseCode::new(340);
    /// Password required (continue with `AUTHINFO PASS`).
    pub const AUTH_PASSWORD_REQUIRED: ResponseCode = ResponseCode::new(381);
    /// Service temporarily unavailable (greeting).
    pub const SERVICE_UNAVAILABLE_TEMPORARY: ResponseCode = ResponseCode::new(400);
    /// The server wants a different `MODE`.
    pub const WRONG_MODE: ResponseCode = ResponseCode::new(401);
    /// No such newsgroup.
    pub const NO_SUCH_GROUP: ResponseCode = ResponseCode::new(411);
    /// No newsgroup has been selected.
    pub const NO_GROUP_SELECTED: ResponseCode = ResponseCode::new(412);
    /// No article has been selected.
    pub const NO_ARTICLE_SELECTED: ResponseCode = ResponseCode::new(420);
    /// No such article number in this group.
    pub const NO_SUCH_ARTICLE_NUMBER: ResponseCode = ResponseCode::new(423);
    /// No article with that message-id.
    pub const NO_SUCH_ARTICLE_ID: ResponseCode = ResponseCode::new(430);
    /// Authentication required.
    pub const AUTH_REQUIRED: ResponseCode = ResponseCode::new(480);
    /// Authentication failed or rejected.
    pub const AUTH_REJECTED: ResponseCode = ResponseCode::new(481);
    /// Authentication commands issued out of sequence.
    pub const AUTH_OUT_OF_SEQUENCE: ResponseCode = ResponseCode::new(482);
    /// Command not recognised.
    pub const UNKNOWN_COMMAND: ResponseCode = ResponseCode::new(500);
    /// Command syntax error.
    pub const SYNTAX_ERROR: ResponseCode = ResponseCode::new(501);
    /// Service permanently unavailable, or access denied.
    pub const SERVICE_UNAVAILABLE_PERMANENT: ResponseCode = ResponseCode::new(502);
    /// Feature not supported.
    pub const FEATURE_NOT_SUPPORTED: ResponseCode = ResponseCode::new(503);
}

/// Removes a single trailing CRLF or LF.
pub(crate) fn strip_eol(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// Lossy UTF-8 conversion, used for human-readable text only.
pub(crate) fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_code_and_text() {
        let line = StatusLine::parse(b"200 news.example.org InterNetNews ready\r\n").unwrap();
        assert_eq!(line.code, codes::GREETING_POSTING_ALLOWED);
        assert_eq!(line.text, "news.example.org InterNetNews ready");
    }

    #[test]
    fn parses_code_with_no_text() {
        let line = StatusLine::parse(b"205").unwrap();
        assert_eq!(line.code.as_u16(), 205);
        assert!(line.text.is_empty());
    }

    #[test]
    fn tolerates_missing_crlf_and_bare_lf() {
        assert_eq!(StatusLine::parse(b"211 0 0 0 a").unwrap().text, "0 0 0 a");
        assert_eq!(StatusLine::parse(b"211 x\n").unwrap().text, "x");
    }

    #[test]
    fn accepts_tab_after_the_code() {
        // Not permitted by the grammar, but observed in the wild.
        assert_eq!(StatusLine::parse(b"500\tnope").unwrap().text, "nope");
    }

    #[test]
    fn rejects_malformed_lines() {
        for bad in [
            &b""[..],
            b"20",
            b"2000 text",
            b"abc text",
            b"20x text",
            b"200text",
            b"-200 text",
        ] {
            assert!(
                matches!(
                    StatusLine::parse(bad),
                    Err(ProtoError::MalformedStatusLine(_))
                ),
                "expected {:?} to be rejected",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn four_digit_code_is_rejected_not_truncated() {
        // "2000 text" must not be read as code 200 with text "0 text".
        assert!(StatusLine::parse(b"2000 text").is_err());
    }

    #[test]
    fn text_is_decoded_lossily() {
        let line = StatusLine::parse(b"200 caf\xe9").unwrap();
        assert_eq!(line.text, "caf\u{fffd}");
    }

    #[test]
    fn classifies_codes() {
        assert_eq!(ResponseCode::new(100).kind(), ResponseKind::Informative);
        assert_eq!(ResponseCode::new(200).kind(), ResponseKind::Success);
        assert_eq!(ResponseCode::new(340).kind(), ResponseKind::Continue);
        assert_eq!(ResponseCode::new(480).kind(), ResponseKind::TransientError);
        assert_eq!(ResponseCode::new(500).kind(), ResponseKind::PermanentError);
        assert_eq!(ResponseCode::new(999).kind(), ResponseKind::Unknown);
        assert!(ResponseCode::new(215).is_ok());
        assert!(!ResponseCode::new(215).is_error());
        assert!(ResponseCode::new(411).is_error());
    }

    #[test]
    fn extracts_numeric_args() {
        let line = StatusLine::parse(b"211 1234 3000234 3002322 misc.test").unwrap();
        assert_eq!(line.arg_u64(0, "GROUP", "count").unwrap(), 1234);
        assert_eq!(line.arg_u64(2, "GROUP", "high").unwrap(), 3_002_322);
        assert_eq!(line.arg(3, "GROUP", "group").unwrap(), "misc.test");
        assert!(matches!(
            line.arg(4, "GROUP", "extra"),
            Err(ProtoError::MissingField { .. })
        ));
        assert!(matches!(
            line.arg_u64(3, "GROUP", "group"),
            Err(ProtoError::InvalidNumber { .. })
        ));
    }

    #[test]
    fn collapses_runs_of_whitespace_between_args() {
        // Some servers pad the numeric fields of a GROUP reply.
        let line = StatusLine::parse(b"211   4   1   4   misc.test").unwrap();
        let args: Vec<_> = line.args().collect();
        assert_eq!(args, ["4", "1", "4", "misc.test"]);
    }
}
