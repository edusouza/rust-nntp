//! Message identifiers.

use crate::{ProtoError, Result};

/// The maximum length of a message-id, in octets, including the angle brackets
/// (RFC 3977 §3.6).
pub const MAX_MESSAGE_ID_LEN: usize = 250;

/// A message identifier, including its angle brackets: `<abc123@example.org>`.
///
/// Construction is validated so that a `MessageId` is always safe to write into a command
/// line: it cannot contain whitespace or control characters, and it cannot be long enough
/// to overflow the 512-octet command limit.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MessageId(String);

impl MessageId {
    /// Validates and wraps a message-id.
    ///
    /// Surrounding whitespace is trimmed. The RFC also requires an `@` separating
    /// `id-left` from `id-right`, which is *not* enforced here: ids without one exist in
    /// old archives, and a reader that refuses to fetch them is less useful than one that
    /// passes them through.
    ///
    /// # Errors
    ///
    /// Returns [`ProtoError::InvalidMessageId`] if the value is not enclosed in angle
    /// brackets, is empty, contains whitespace, control characters or a bare `<` or `>`,
    /// or exceeds [`MAX_MESSAGE_ID_LEN`] octets.
    pub fn parse(value: &str) -> Result<Self> {
        let trimmed = value.trim();
        let invalid = || ProtoError::InvalidMessageId(trimmed.to_owned());

        if trimmed.len() > MAX_MESSAGE_ID_LEN {
            return Err(invalid());
        }

        let inner = trimmed
            .strip_prefix('<')
            .and_then(|rest| rest.strip_suffix('>'))
            .ok_or_else(invalid)?;

        if inner.is_empty() {
            return Err(invalid());
        }
        if inner
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '<' || c == '>')
        {
            return Err(invalid());
        }

        Ok(Self(trimmed.to_owned()))
    }

    /// The message-id including its angle brackets.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The message-id without its angle brackets.
    pub fn inner(&self) -> &str {
        self.0
            .strip_prefix('<')
            .and_then(|rest| rest.strip_suffix('>'))
            .unwrap_or(&self.0)
    }

    /// Extracts every message-id found in a `References`-style header value.
    ///
    /// Unparseable fragments are skipped rather than failing the whole value: a broken
    /// `References` header is common and must not stop an article from being displayed.
    pub fn parse_list(value: &str) -> Vec<Self> {
        let mut out = Vec::new();
        let mut rest = value;
        while let Some(open) = rest.find('<') {
            let after_open = rest.get(open..).unwrap_or_default();
            let Some(close) = after_open.find('>') else {
                break;
            };
            if let Some(candidate) = after_open.get(..=close) {
                if let Ok(id) = Self::parse(candidate) {
                    out.push(id);
                }
            }
            rest = after_open.get(close + 1..).unwrap_or_default();
        }
        out
    }
}

impl core::fmt::Display for MessageId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl core::str::FromStr for MessageId {
    type Err = ProtoError;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_plain_id() {
        let id = MessageId::parse("<abc123@example.org>").unwrap();
        assert_eq!(id.as_str(), "<abc123@example.org>");
        assert_eq!(id.inner(), "abc123@example.org");
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(MessageId::parse("  <a@b>\t").unwrap().as_str(), "<a@b>");
    }

    #[test]
    fn accepts_an_id_without_an_at_sign() {
        assert!(MessageId::parse("<legacy-id>").is_ok());
    }

    #[test]
    fn rejects_unbracketed_empty_and_oversized_ids() {
        for bad in [
            "abc@example.org",
            "<abc@example.org",
            "abc@example.org>",
            "<>",
            "",
            "<a b@c>",
            "<a\r\nb>",
            "<a<b>",
            "<a>b>",
        ] {
            assert!(
                MessageId::parse(bad).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
        let oversized = format!("<{}@example.org>", "x".repeat(MAX_MESSAGE_ID_LEN));
        assert!(MessageId::parse(&oversized).is_err());
    }

    #[test]
    fn injection_attempt_is_refused() {
        // If this were accepted, sending `ARTICLE <id>` would also send `QUIT`.
        assert!(MessageId::parse("<a@b>\r\nQUIT\r\n").is_err());
    }

    #[test]
    fn parses_a_references_chain() {
        let ids = MessageId::parse_list("<a@x> <b@y>\t<c@z>");
        assert_eq!(
            ids.iter().map(MessageId::as_str).collect::<Vec<_>>(),
            ["<a@x>", "<b@y>", "<c@z>"]
        );
    }

    #[test]
    fn skips_junk_in_a_references_chain() {
        let ids = MessageId::parse_list("<good@x> not-an-id <> <also good@y> <fine@z>");
        assert_eq!(
            ids.iter().map(MessageId::as_str).collect::<Vec<_>>(),
            ["<good@x>", "<fine@z>"]
        );
    }

    #[test]
    fn unterminated_reference_does_not_loop() {
        assert!(MessageId::parse_list("<unterminated").is_empty());
        assert_eq!(MessageId::parse_list("<a@b> <unterminated").len(), 1);
    }
}
