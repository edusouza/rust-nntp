//! One header field across many articles: the `HDR` and `XHDR` responses.
//!
//! `OVER` hands back eight fields for every article in a range. When only one of them is
//! wanted — `References` for threading, `Subject` for a search — `HDR` fetches that one
//! field and nothing else, which on a range of thousands is a fraction of the bytes.
//!
//! The line format is the article number, a space, and the rest of the line verbatim
//! (RFC 3977 §8.5.2). "The rest of the line" is doing some work there: a subject contains
//! spaces, so only the *first* space is a separator.

use crate::mime::decode_header_value;
use crate::response::strip_eol;

/// One article's value for the requested header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderEntry {
    /// The article number, or `0` when the article was named by message-id — which is what
    /// RFC 3977 §8.5.2 requires the server to send, rather than a number it may not have.
    pub number: u64,
    /// The field's value, with RFC 2047 encoded words expanded.
    pub value: String,
}

impl HeaderEntry {
    /// Parses one line of an `HDR` or `XHDR` response.
    ///
    /// Returns `None` for a line that does not begin with a number, which is the only part
    /// of the format a server can get wrong without the whole response being unusable.
    ///
    /// A server that has no value for the field sends the article number and nothing else;
    /// RFC 3977 §8.5 suggests a placeholder, and different servers use different ones, so
    /// an empty value is returned as an empty string rather than guessed at.
    pub fn parse(line: &[u8]) -> Option<Self> {
        let line = strip_eol(line);
        let (number, rest) = match line.iter().position(|byte| *byte == b' ') {
            Some(space) => (
                line.get(..space)?,
                line.get(space + 1..).unwrap_or_default(),
            ),
            None => (line, [].as_slice()),
        };

        let number = std::str::from_utf8(number).ok()?.parse::<u64>().ok()?;

        Some(Self {
            number,
            value: decode_header_value(rest),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_number_and_the_rest_of_the_line() {
        let entry = HeaderEntry::parse(b"4242 Re: a subject with spaces in it").expect("an entry");
        assert_eq!(entry.number, 4242);
        assert_eq!(entry.value, "Re: a subject with spaces in it");
    }

    #[test]
    fn only_the_first_space_separates() {
        // The obvious mistake is splitting on whitespace, which truncates every subject
        // that has any.
        let entry = HeaderEntry::parse(b"1 <a@x> <b@x> <c@x>").expect("an entry");
        assert_eq!(entry.value, "<a@x> <b@x> <c@x>");
    }

    #[test]
    fn an_article_named_by_message_id_is_numbered_zero() {
        // RFC 3977 §8.5.2: the server sends 0 rather than a number it may not have.
        let entry = HeaderEntry::parse(b"0 the value").expect("an entry");
        assert_eq!(entry.number, 0);
    }

    #[test]
    fn a_field_the_article_does_not_have_is_empty_not_absent() {
        assert_eq!(
            HeaderEntry::parse(b"7").map(|entry| entry.value),
            Some(String::new())
        );
        assert_eq!(
            HeaderEntry::parse(b"7 ").map(|entry| entry.value),
            Some(String::new())
        );
    }

    #[test]
    fn encoded_words_are_expanded_like_every_other_header() {
        let entry = HeaderEntry::parse(b"3 =?UTF-8?B?Y2Fmw6k=?=").expect("an entry");
        assert_eq!(entry.value, "café");
    }

    #[test]
    fn a_line_that_does_not_start_with_a_number_is_not_an_entry() {
        assert_eq!(HeaderEntry::parse(b"not a number at all"), None);
        assert_eq!(HeaderEntry::parse(b""), None);
    }

    #[test]
    fn a_trailing_terminator_is_not_part_of_the_value() {
        let entry = HeaderEntry::parse(b"1 value\r\n").expect("an entry");
        assert_eq!(entry.value, "value");
    }
}
