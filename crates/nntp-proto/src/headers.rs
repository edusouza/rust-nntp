//! Article headers: names, values, folding and lookup.
//!
//! Header *values* are kept as raw bytes rather than `String`. Articles carry unlabelled
//! 8-bit text often enough that converting eagerly would either lose information (lossy
//! UTF-8) or reject readable articles (strict UTF-8). Decoding is a separate, explicit
//! step: [`HeaderValue::decoded`] applies RFC 2047 and the 8-bit fallback.

use crate::mime;
use crate::{ProtoError, Result};

/// A header field name, without the colon.
///
/// RFC 5322 §3.6.8 allows printable US-ASCII except colon. Comparison is
/// case-insensitive, as required by the specification.
#[derive(Debug, Clone)]
pub struct HeaderName(String);

impl HeaderName {
    /// Validates and wraps a field name.
    ///
    /// # Errors
    ///
    /// Returns [`ProtoError::InvalidHeaderName`] if the name is empty or contains a byte
    /// that is not printable US-ASCII, or contains a colon.
    pub fn parse(value: &str) -> Result<Self> {
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_graphic() && b != b':') {
            return Err(ProtoError::InvalidHeaderName(value.to_owned()));
        }
        Ok(Self(value.to_owned()))
    }

    /// The name as written by the sender, preserving its original case.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl PartialEq for HeaderName {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq_ignore_ascii_case(&other.0)
    }
}

impl Eq for HeaderName {}

impl core::hash::Hash for HeaderName {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        for byte in self.0.bytes() {
            state.write_u8(byte.to_ascii_lowercase());
        }
        state.write_u8(0xff);
    }
}

impl core::fmt::Display for HeaderName {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl core::str::FromStr for HeaderName {
    type Err = ProtoError;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

/// Well-known header names, as `&str` for use with [`Headers::get`].
pub mod names {
    /// `From`
    pub const FROM: &str = "From";
    /// `Subject`
    pub const SUBJECT: &str = "Subject";
    /// `Date`
    pub const DATE: &str = "Date";
    /// `Message-ID`
    pub const MESSAGE_ID: &str = "Message-ID";
    /// `References`
    pub const REFERENCES: &str = "References";
    /// `In-Reply-To`
    pub const IN_REPLY_TO: &str = "In-Reply-To";
    /// `Newsgroups`
    pub const NEWSGROUPS: &str = "Newsgroups";
    /// `Followup-To`
    pub const FOLLOWUP_TO: &str = "Followup-To";
    /// `Content-Type`
    pub const CONTENT_TYPE: &str = "Content-Type";
    /// `Content-Transfer-Encoding`
    pub const CONTENT_TRANSFER_ENCODING: &str = "Content-Transfer-Encoding";
    /// `Lines`
    pub const LINES: &str = "Lines";
    /// `Xref`
    pub const XREF: &str = "Xref";
    /// `Organization`
    pub const ORGANIZATION: &str = "Organization";
    /// `User-Agent`
    pub const USER_AGENT: &str = "User-Agent";
}

/// A header field value, as received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderValue(Vec<u8>);

impl HeaderValue {
    /// Wraps raw value bytes.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    /// The raw bytes, with folding already removed and surrounding space trimmed.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// The value decoded for display: RFC 2047 encoded words expanded, unlabelled 8-bit
    /// bytes read as Windows-1252, and runs of folding whitespace collapsed to one space.
    pub fn decoded(&self) -> String {
        let decoded = mime::decode_header_value(&self.0);
        collapse_whitespace(&decoded)
    }

    /// The value decoded without collapsing internal whitespace.
    ///
    /// Use this where layout matters, for example an `Xref` or a `Path` that a human will
    /// compare byte for byte.
    pub fn decoded_verbatim(&self) -> String {
        mime::decode_header_value(&self.0)
    }
}

impl core::fmt::Display for HeaderValue {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.decoded())
    }
}

/// The header block of an article, in the order the fields arrived.
///
/// Order is preserved because it is meaningful: `Path` and `Received`-style trace fields
/// are read top to bottom, and a reader that displays raw headers should show what the
/// server sent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Headers {
    entries: Vec<(HeaderName, HeaderValue)>,
    malformed: Vec<Vec<u8>>,
}

impl Headers {
    /// An empty header block.
    pub fn new() -> Self {
        Self::default()
    }

    /// Parses a header block from lines with their line terminators already removed.
    ///
    /// Folding is undone per RFC 5322 §2.2.3: a line beginning with space or tab
    /// continues the previous field, and the CRLF is removed while the leading whitespace
    /// is kept.
    ///
    /// Lines that are neither a `name: value` pair nor a continuation are collected by
    /// [`Self::malformed`] instead of failing the parse. Broken headers are common in old
    /// archives and must not prevent an article from being displayed.
    pub fn parse_lines<'a>(lines: impl IntoIterator<Item = &'a [u8]>) -> Self {
        let mut headers = Self::new();

        for line in lines {
            // An empty line ends the header block; callers normally split it off first.
            if line.is_empty() {
                break;
            }

            if matches!(line.first(), Some(b' ' | b'\t')) {
                match headers.entries.last_mut() {
                    Some((_, value)) => {
                        value.0.extend_from_slice(line);
                        continue;
                    }
                    // A continuation with nothing to continue.
                    None => {
                        headers.malformed.push(line.to_vec());
                        continue;
                    }
                }
            }

            match split_field(line) {
                Some((name, value)) => headers.entries.push((name, HeaderValue::new(value))),
                None => headers.malformed.push(line.to_vec()),
            }
        }

        // Trailing whitespace from folding is not part of any value.
        for (_, value) in &mut headers.entries {
            while matches!(value.0.last(), Some(b' ' | b'\t')) {
                value.0.pop();
            }
        }

        headers
    }

    /// Parses a header block from a byte buffer, splitting on CRLF or bare LF.
    pub fn parse_block(block: &[u8]) -> Self {
        let lines: Vec<&[u8]> = split_lines(block);
        Self::parse_lines(lines)
    }

    /// The first value for `name`, compared case-insensitively.
    pub fn get(&self, name: &str) -> Option<&HeaderValue> {
        self.entries
            .iter()
            .find(|(field, _)| field.0.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    }

    /// Every value for `name`, in order.
    ///
    /// Repeated fields are legal for some names and a sign of a forged article for others,
    /// such as a second `Message-ID`, so the duplicates are visible rather than merged.
    pub fn get_all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a HeaderValue> + 'a {
        self.entries
            .iter()
            .filter(move |(field, _)| field.0.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    }

    /// The first value for `name`, decoded for display.
    pub fn get_decoded(&self, name: &str) -> Option<String> {
        self.get(name).map(HeaderValue::decoded)
    }

    /// Whether a field is present.
    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Appends a field.
    ///
    /// # Errors
    ///
    /// Returns [`ProtoError::InvalidHeaderName`] if `name` is not a valid field name.
    pub fn insert(&mut self, name: &str, value: impl Into<Vec<u8>>) -> Result<()> {
        self.entries
            .push((HeaderName::parse(name)?, HeaderValue::new(value)));
        Ok(())
    }

    /// Iterates over the fields in the order they arrived.
    pub fn iter(&self) -> impl Iterator<Item = (&HeaderName, &HeaderValue)> {
        self.entries.iter().map(|(name, value)| (name, value))
    }

    /// The number of fields, counting repeats separately.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the block has no well-formed fields.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Lines that could not be parsed as a field or a continuation.
    ///
    /// Non-empty output here is worth logging: it usually means the article is malformed,
    /// but it can also mean this parser is wrong.
    pub fn malformed(&self) -> &[Vec<u8>] {
        &self.malformed
    }
}

/// Splits `name: value`, rejecting a name that is not valid.
fn split_field(line: &[u8]) -> Option<(HeaderName, Vec<u8>)> {
    let colon = memchr::memchr(b':', line)?;
    let raw_name = line.get(..colon)?;
    // RFC 5322 forbids whitespace between the name and the colon. Some articles have it
    // anyway, so it is trimmed rather than treated as malformed.
    let name = HeaderName::parse(core::str::from_utf8(raw_name).ok()?.trim()).ok()?;
    let value = line.get(colon + 1..).unwrap_or_default();
    let value = trim_ascii_start(value);
    Some((name, value.to_vec()))
}

fn trim_ascii_start(mut bytes: &[u8]) -> &[u8] {
    while let Some((b' ' | b'\t', rest)) = bytes.split_first() {
        bytes = rest;
    }
    bytes
}

/// Splits a buffer into lines on LF, discarding a preceding CR.
pub(crate) fn split_lines(block: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0usize;

    while let Some(offset) = memchr::memchr(b'\n', block.get(start..).unwrap_or_default()) {
        let end = start + offset;
        let line = block.get(start..end).unwrap_or_default();
        lines.push(line.strip_suffix(b"\r").unwrap_or(line));
        start = end + 1;
    }

    if let Some(tail) = block.get(start..) {
        if !tail.is_empty() {
            lines.push(tail.strip_suffix(b"\r").unwrap_or(tail));
        }
    }

    lines
}

/// Collapses runs of whitespace into single spaces and trims the ends.
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;

    for ch in text.chars() {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(ch);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(block: &str) -> Headers {
        Headers::parse_block(block.as_bytes())
    }

    #[test]
    fn parses_simple_fields() {
        let headers = parse("From: a@b\r\nSubject: hello\r\n");
        assert_eq!(headers.len(), 2);
        assert_eq!(headers.get_decoded(names::FROM).as_deref(), Some("a@b"));
        assert_eq!(headers.get_decoded("subject").as_deref(), Some("hello"));
        assert!(headers.contains("SUBJECT"));
        assert!(!headers.contains("Newsgroups"));
    }

    #[test]
    fn lookup_is_case_insensitive_but_original_case_is_kept() {
        let headers = parse("mEsSaGe-ID: <a@b>\r\n");
        assert!(headers.contains("Message-Id"));
        let (name, _) = headers.iter().next().unwrap();
        assert_eq!(name.as_str(), "mEsSaGe-ID");
    }

    #[test]
    fn unfolds_continuation_lines() {
        let headers = parse("Subject: a very\r\n long subject\r\n\tand more\r\n");
        assert_eq!(
            headers.get_decoded(names::SUBJECT).as_deref(),
            Some("a very long subject and more")
        );
    }

    #[test]
    fn folding_whitespace_is_preserved_in_the_raw_value() {
        let headers = parse("Subject: a\r\n\tb\r\n");
        assert_eq!(headers.get(names::SUBJECT).unwrap().as_bytes(), b"a\tb");
    }

    #[test]
    fn stops_at_the_blank_line_separating_head_from_body() {
        let headers = parse("From: a@b\r\n\r\nSubject: not-a-header\r\n");
        assert_eq!(headers.len(), 1);
        assert!(!headers.contains(names::SUBJECT));
    }

    #[test]
    fn keeps_repeated_fields_separate() {
        let headers = parse("Received: one\r\nReceived: two\r\n");
        let all: Vec<_> = headers.get_all("received").map(|v| v.decoded()).collect();
        assert_eq!(all, ["one", "two"]);
        assert_eq!(headers.get_decoded("received").as_deref(), Some("one"));
    }

    #[test]
    fn collects_malformed_lines_without_failing() {
        let headers = parse("From: a@b\r\nthis-has-no-colon\r\nSubject: kept\r\n");
        assert_eq!(headers.len(), 2);
        assert_eq!(headers.malformed().len(), 1);
        assert_eq!(headers.malformed()[0], b"this-has-no-colon");
        assert_eq!(headers.get_decoded(names::SUBJECT).as_deref(), Some("kept"));
    }

    #[test]
    fn a_continuation_with_nothing_to_continue_is_malformed() {
        let headers = parse("   orphaned continuation\r\nFrom: a@b\r\n");
        assert_eq!(headers.malformed().len(), 1);
        assert_eq!(headers.len(), 1);
    }

    #[test]
    fn tolerates_space_before_the_colon() {
        let headers = parse("Subject : spaced\r\n");
        assert_eq!(
            headers.get_decoded(names::SUBJECT).as_deref(),
            Some("spaced")
        );
    }

    #[test]
    fn handles_an_empty_value() {
        let headers = parse("Subject:\r\nFrom: a@b\r\n");
        assert_eq!(headers.get(names::SUBJECT).unwrap().as_bytes(), b"");
        assert_eq!(headers.len(), 2);
    }

    #[test]
    fn accepts_bare_lf_line_endings() {
        let headers = parse("From: a@b\nSubject: hello\n");
        assert_eq!(headers.len(), 2);
    }

    #[test]
    fn handles_a_last_line_without_a_terminator() {
        let headers = parse("From: a@b");
        assert_eq!(headers.get_decoded(names::FROM).as_deref(), Some("a@b"));
    }

    #[test]
    fn decodes_encoded_words_on_demand() {
        let headers = parse("Subject: =?UTF-8?Q?caf=C3=A9?=\r\n");
        assert_eq!(headers.get_decoded(names::SUBJECT).as_deref(), Some("café"));
        // The raw bytes are untouched.
        assert_eq!(
            headers.get(names::SUBJECT).unwrap().as_bytes(),
            b"=?UTF-8?Q?caf=C3=A9?="
        );
    }

    #[test]
    fn decodes_an_encoded_word_split_across_a_fold() {
        // A single multi-byte character encoded across two folded encoded words.
        let headers = parse("Subject: =?UTF-8?Q?caf?=\r\n =?UTF-8?Q?=C3=A9?=\r\n");
        assert_eq!(headers.get_decoded(names::SUBJECT).as_deref(), Some("café"));
    }

    #[test]
    fn decodes_raw_eight_bit_values() {
        let headers = Headers::parse_block(b"Subject: caf\xe9\r\n");
        assert_eq!(headers.get_decoded(names::SUBJECT).as_deref(), Some("café"));
    }

    #[test]
    fn verbatim_decoding_keeps_internal_spacing() {
        let headers = parse("Xref: host  group:1   group:2\r\n");
        let value = headers.get(names::XREF).unwrap();
        assert_eq!(value.decoded_verbatim(), "host  group:1   group:2");
        assert_eq!(value.decoded(), "host group:1 group:2");
    }

    #[test]
    fn rejects_invalid_header_names() {
        for bad in ["", "has space", "with:colon", "caf\u{e9}"] {
            assert!(HeaderName::parse(bad).is_err(), "expected {bad:?} rejected");
        }
        assert!(HeaderName::parse("X-Custom_Field.1").is_ok());
    }

    #[test]
    fn insert_appends_and_validates() {
        let mut headers = Headers::new();
        headers.insert("Subject", "hi").unwrap();
        assert_eq!(headers.get_decoded("subject").as_deref(), Some("hi"));
        assert!(headers.insert("bad name", "x").is_err());
    }

    #[test]
    fn header_names_compare_and_hash_case_insensitively() {
        use std::collections::HashSet;
        let a = HeaderName::parse("Subject").unwrap();
        let b = HeaderName::parse("SUBJECT").unwrap();
        assert_eq!(a, b);
        let mut set = HashSet::new();
        set.insert(a);
        assert!(set.contains(&b));
    }

    #[test]
    fn splits_lines_on_both_terminators() {
        assert_eq!(split_lines(b"a\r\nb\nc"), vec![&b"a"[..], b"b", b"c"]);
        assert_eq!(split_lines(b""), Vec::<&[u8]>::new());
        assert_eq!(split_lines(b"\r\n"), vec![&b""[..]]);
    }
}
