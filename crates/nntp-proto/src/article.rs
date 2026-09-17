//! Whole articles: headers plus body, and the decoding needed to display one.

use chrono::{DateTime, FixedOffset};

use crate::block::DataBlock;
use crate::date::parse_date;
use crate::group::GroupName;
use crate::headers::{Headers, names};
use crate::message_id::MessageId;
use crate::mime;

/// A parsed `Content-Type` header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentType {
    /// The type, lowercased: `text`.
    pub type_: String,
    /// The subtype, lowercased: `plain`.
    pub subtype: String,
    /// Parameters, with lowercased names and values as written.
    pub params: Vec<(String, String)>,
}

impl ContentType {
    /// Parses a `Content-Type` value such as `text/plain; charset="UTF-8"; format=flowed`.
    ///
    /// Malformed input degrades rather than failing: a value with no `/` becomes that type
    /// with an empty subtype, which is enough for the "is this text?" question that
    /// actually gets asked.
    pub fn parse(value: &str) -> Self {
        let mut parts = value.split(';');
        let full = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
        let (type_, subtype) = match full.split_once('/') {
            Some((type_, subtype)) => (type_.trim().to_owned(), subtype.trim().to_owned()),
            None => (full, String::new()),
        };

        let params = parts
            .filter_map(|part| {
                let (name, raw) = part.split_once('=')?;
                let value = raw.trim().trim_matches('"').to_owned();
                Some((name.trim().to_ascii_lowercase(), value))
            })
            .collect();

        Self {
            type_,
            subtype,
            params,
        }
    }

    /// A parameter's value, by lowercased name.
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, value)| value.as_str())
    }

    /// The declared charset, if any.
    pub fn charset(&self) -> Option<&str> {
        self.param("charset")
    }

    /// The multipart boundary, if any.
    pub fn boundary(&self) -> Option<&str> {
        self.param("boundary")
    }

    /// Whether this is a textual type that can be shown directly.
    pub fn is_text(&self) -> bool {
        self.type_ == "text"
    }

    /// Whether this is a multipart container.
    ///
    /// Multipart bodies are not split apart in v0.1; the raw body is shown instead. See
    /// the roadmap issue for MIME support.
    pub fn is_multipart(&self) -> bool {
        self.type_ == "multipart"
    }
}

impl Default for ContentType {
    /// The default assumed when no `Content-Type` header is present: `text/plain`
    /// (RFC 2045 §5.2).
    fn default() -> Self {
        Self {
            type_: "text".to_owned(),
            subtype: "plain".to_owned(),
            params: Vec::new(),
        }
    }
}

/// How a body was encoded for transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferEncoding {
    /// `7bit`, `8bit`, `binary` or absent: the body is as it appears.
    None,
    /// `quoted-printable`.
    QuotedPrintable,
    /// `base64`.
    Base64,
    /// Something this crate does not decode; the body is shown as it arrived.
    Unknown,
}

impl TransferEncoding {
    /// Interprets a `Content-Transfer-Encoding` value.
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "7bit" | "8bit" | "binary" => Self::None,
            "quoted-printable" => Self::QuotedPrintable,
            "base64" => Self::Base64,
            _ => Self::Unknown,
        }
    }
}

/// An article as received.
///
/// Bodies are stored as raw lines, not as a `String`: charset and transfer encoding are
/// declared in the headers, so decoding is a decision that needs both halves and is made
/// by [`Article::body_text`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Article {
    /// The article number in the group it was fetched from, if it was fetched by number.
    pub number: Option<u64>,
    /// The header block.
    pub headers: Headers,
    /// The body, one entry per line, with line terminators removed.
    pub body: Vec<Vec<u8>>,
}

impl Article {
    /// Builds an article from an `ARTICLE` data block.
    pub fn from_block(block: &DataBlock) -> Self {
        let (head, body) = block.split_head_body();
        Self {
            number: None,
            headers: Headers::parse_lines(head.iter().map(Vec::as_slice)),
            body: body.to_vec(),
        }
    }

    /// Builds a headers-only article from a `HEAD` data block.
    pub fn from_head_block(block: &DataBlock) -> Self {
        Self {
            number: None,
            headers: Headers::parse_lines(block.lines().iter().map(Vec::as_slice)),
            body: Vec::new(),
        }
    }

    /// Attaches the article number the article was fetched by.
    pub fn with_number(mut self, number: u64) -> Self {
        self.number = Some(number);
        self
    }

    /// Whether any body lines were received.
    ///
    /// A `HEAD` response and an article with an empty body are indistinguishable here;
    /// callers that need to tell them apart should track which command they sent.
    pub fn has_body(&self) -> bool {
        !self.body.is_empty()
    }

    /// The decoded `Subject`, or an empty string.
    pub fn subject(&self) -> String {
        self.headers.get_decoded(names::SUBJECT).unwrap_or_default()
    }

    /// The decoded `From`, or an empty string.
    pub fn author(&self) -> String {
        self.headers.get_decoded(names::FROM).unwrap_or_default()
    }

    /// The parsed `Date`, if present and interpretable.
    pub fn date(&self) -> Option<DateTime<FixedOffset>> {
        parse_date(&self.headers.get_decoded(names::DATE)?).ok()
    }

    /// The `Message-ID`, if present and well-formed.
    pub fn message_id(&self) -> Option<MessageId> {
        MessageId::parse(&self.headers.get_decoded(names::MESSAGE_ID)?).ok()
    }

    /// The `References` chain, oldest first.
    ///
    /// Falls back to `In-Reply-To` when `References` is absent, which is how mail-to-news
    /// gateways often thread.
    pub fn references(&self) -> Vec<MessageId> {
        if let Some(value) = self.headers.get(names::REFERENCES) {
            let ids = MessageId::parse_list(&value.decoded_verbatim());
            if !ids.is_empty() {
                return ids;
            }
        }
        match self.headers.get(names::IN_REPLY_TO) {
            Some(value) => MessageId::parse_list(&value.decoded_verbatim()),
            None => Vec::new(),
        }
    }

    /// The groups listed in `Newsgroups`, skipping any that are not valid names.
    pub fn newsgroups(&self) -> Vec<GroupName> {
        self.headers
            .get_decoded(names::NEWSGROUPS)
            .unwrap_or_default()
            .split(',')
            .filter_map(|name| GroupName::parse(name.trim()).ok())
            .collect()
    }

    /// The parsed `Content-Type`, defaulting to `text/plain`.
    pub fn content_type(&self) -> ContentType {
        self.headers
            .get_decoded(names::CONTENT_TYPE)
            .map(|value| ContentType::parse(&value))
            .unwrap_or_default()
    }

    /// The declared `Content-Transfer-Encoding`.
    pub fn transfer_encoding(&self) -> TransferEncoding {
        self.headers
            .get_decoded(names::CONTENT_TRANSFER_ENCODING)
            .map_or(TransferEncoding::None, |value| {
                TransferEncoding::parse(&value)
            })
    }

    /// The body as raw octets, rejoined with CRLF.
    pub fn body_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (index, line) in self.body.iter().enumerate() {
            if index > 0 {
                out.extend_from_slice(b"\r\n");
            }
            out.extend_from_slice(line);
        }
        out
    }

    /// The body as displayable text.
    ///
    /// Applies the transfer encoding, then the declared charset, then falls back to
    /// Windows-1252 for unlabelled 8-bit bytes. Lines are separated by `\n`.
    ///
    /// Multipart bodies are returned whole, including their boundary lines: splitting them
    /// is v0.2 work. An encoding this crate cannot decode is likewise returned as it
    /// arrived, on the grounds that a reader showing base64 is more useful than one
    /// showing nothing.
    pub fn body_text(&self) -> String {
        let raw = self.body_bytes();

        let decoded = match self.transfer_encoding() {
            TransferEncoding::None | TransferEncoding::Unknown => raw,
            TransferEncoding::QuotedPrintable => mime::decode_quoted_printable(&raw),
            TransferEncoding::Base64 => mime::decode_base64(&raw).unwrap_or(raw),
        };

        let text = match self.content_type().charset() {
            Some(charset) => mime::decode_with_charset(&decoded, charset),
            None => mime::decode_8bit_lossy(&decoded),
        };

        text.replace("\r\n", "\n")
    }

    /// The number of body lines received.
    pub fn line_count(&self) -> usize {
        self.body.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn article(text: &str) -> Article {
        Article::from_block(&DataBlock::parse(text.as_bytes()))
    }

    const SAMPLE: &str = "From: \"Demo User\" <nobody@example.net>\r\n\
        Subject: =?UTF-8?Q?caf=C3=A9?=\r\n\
        Date: Tue, 6 Oct 1998 04:38:40 -0500\r\n\
        Message-ID: <45223423@example.com>\r\n\
        References: <45454@example.net> <45455@example.net>\r\n\
        Newsgroups: misc.test, comp.lang.rust\r\n\
        \r\n\
        This is the body.\r\n\
        Second line.\r\n\
        .\r\n";

    #[test]
    fn parses_headers_and_body() {
        let article = article(SAMPLE);
        assert_eq!(article.subject(), "café");
        assert_eq!(article.author(), "\"Demo User\" <nobody@example.net>");
        assert_eq!(
            article.message_id().map(|id| id.as_str().to_owned()),
            Some("<45223423@example.com>".to_owned())
        );
        assert_eq!(
            article.date().map(|d| d.to_rfc3339()),
            Some("1998-10-06T04:38:40-05:00".to_owned())
        );
        assert_eq!(article.references().len(), 2);
        assert_eq!(
            article
                .newsgroups()
                .iter()
                .map(GroupName::as_str)
                .collect::<Vec<_>>(),
            ["misc.test", "comp.lang.rust"]
        );
        assert_eq!(article.body_text(), "This is the body.\nSecond line.");
        assert_eq!(article.line_count(), 2);
        assert!(article.has_body());
    }

    #[test]
    fn a_body_line_starting_with_a_dot_is_preserved() {
        let article = article("From: a@b\r\n\r\n..dotted\r\nnormal\r\n.\r\n");
        assert_eq!(article.body_text(), ".dotted\nnormal");
    }

    #[test]
    fn head_only_responses_have_no_body() {
        let article =
            Article::from_head_block(&DataBlock::parse(b"From: a@b\r\nSubject: s\r\n.\r\n"));
        assert_eq!(article.headers.len(), 2);
        assert!(!article.has_body());
        assert!(article.body_text().is_empty());
    }

    #[test]
    fn attaches_an_article_number() {
        assert_eq!(article(SAMPLE).with_number(42).number, Some(42));
        assert_eq!(article(SAMPLE).number, None);
    }

    #[test]
    fn falls_back_to_in_reply_to_for_threading() {
        let article = article("In-Reply-To: <parent@x>\r\n\r\nbody\r\n.\r\n");
        assert_eq!(
            article
                .references()
                .iter()
                .map(MessageId::as_str)
                .collect::<Vec<_>>(),
            ["<parent@x>"]
        );
    }

    #[test]
    fn prefers_references_over_in_reply_to() {
        let article = article("References: <a@x>\r\nIn-Reply-To: <b@x>\r\n\r\nbody\r\n.\r\n");
        assert_eq!(
            article
                .references()
                .iter()
                .map(MessageId::as_str)
                .collect::<Vec<_>>(),
            ["<a@x>"]
        );
    }

    #[test]
    fn an_empty_references_header_falls_through() {
        let article = article("References: \r\nIn-Reply-To: <b@x>\r\n\r\nx\r\n.\r\n");
        assert_eq!(
            article
                .references()
                .iter()
                .map(MessageId::as_str)
                .collect::<Vec<_>>(),
            ["<b@x>"]
        );
    }

    #[test]
    fn skips_invalid_group_names_in_newsgroups() {
        let article = article("Newsgroups: good.group, bad group, other.group\r\n\r\nx\r\n.\r\n");
        assert_eq!(
            article
                .newsgroups()
                .iter()
                .map(GroupName::as_str)
                .collect::<Vec<_>>(),
            ["good.group", "other.group"]
        );
    }

    #[test]
    fn decodes_a_quoted_printable_body() {
        // Built by concatenation rather than with a line-continuation literal, because
        // `\` before a newline in a Rust string also eats the next line's indentation,
        // which would silently change the body being tested.
        let article = article(concat!(
            "Content-Type: text/plain; charset=UTF-8\r\n",
            "Content-Transfer-Encoding: quoted-printable\r\n",
            "\r\n",
            "caf=C3=A9 and a soft=\r\n",
            " break\r\n",
            ".\r\n"
        ));
        assert_eq!(article.body_text(), "café and a soft break");
    }

    #[test]
    fn decodes_a_base64_body() {
        let article = article(
            "Content-Type: text/plain; charset=UTF-8\r\n\
             Content-Transfer-Encoding: base64\r\n\
             \r\n\
             Y2Fmw6kgYm9keQ==\r\n\
             .\r\n",
        );
        assert_eq!(article.body_text(), "café body");
    }

    #[test]
    fn decodes_a_declared_legacy_charset() {
        let block = DataBlock::parse(
            b"Content-Type: text/plain; charset=\"ISO-8859-1\"\r\n\r\ncaf\xe9\r\n.\r\n",
        );
        assert_eq!(Article::from_block(&block).body_text(), "café");
    }

    #[test]
    fn falls_back_when_no_charset_is_declared() {
        let block = DataBlock::parse(b"From: a@b\r\n\r\ncaf\xe9\r\n.\r\n");
        assert_eq!(Article::from_block(&block).body_text(), "café");
    }

    #[test]
    fn an_undecodable_base64_body_is_shown_as_it_arrived() {
        let article =
            article("Content-Transfer-Encoding: base64\r\n\r\nthis is not base64!!\r\n.\r\n");
        assert_eq!(article.body_text(), "this is not base64!!");
    }

    #[test]
    fn an_unknown_transfer_encoding_is_left_alone() {
        let article = article("Content-Transfer-Encoding: x-uuencode\r\n\r\nraw\r\n.\r\n");
        assert_eq!(article.transfer_encoding(), TransferEncoding::Unknown);
        assert_eq!(article.body_text(), "raw");
    }

    #[test]
    fn parses_content_type_parameters() {
        let ct = ContentType::parse("Text/Plain; charset=\"UTF-8\"; format=flowed");
        assert_eq!(ct.type_, "text");
        assert_eq!(ct.subtype, "plain");
        assert_eq!(ct.charset(), Some("UTF-8"));
        assert_eq!(ct.param("format"), Some("flowed"));
        assert!(ct.is_text());
        assert!(!ct.is_multipart());
    }

    #[test]
    fn recognises_multipart_and_its_boundary() {
        let ct = ContentType::parse("multipart/mixed; boundary=\"----=_Part_1\"");
        assert!(ct.is_multipart());
        assert_eq!(ct.boundary(), Some("----=_Part_1"));
    }

    #[test]
    fn degrades_on_a_malformed_content_type() {
        let ct = ContentType::parse("nonsense");
        assert_eq!(ct.type_, "nonsense");
        assert!(ct.subtype.is_empty());
        assert!(!ct.is_text());
        assert_eq!(ContentType::parse("").type_, "");
    }

    #[test]
    fn defaults_to_text_plain_when_the_header_is_absent() {
        let ct = article("From: a@b\r\n\r\nx\r\n.\r\n").content_type();
        assert_eq!(ct, ContentType::default());
        assert!(ct.is_text());
        assert_eq!(ct.charset(), None);
    }

    #[test]
    fn parses_transfer_encoding_names() {
        assert_eq!(TransferEncoding::parse("7bit"), TransferEncoding::None);
        assert_eq!(TransferEncoding::parse(" 8BIT "), TransferEncoding::None);
        assert_eq!(TransferEncoding::parse(""), TransferEncoding::None);
        assert_eq!(
            TransferEncoding::parse("Quoted-Printable"),
            TransferEncoding::QuotedPrintable
        );
        assert_eq!(TransferEncoding::parse("BASE64"), TransferEncoding::Base64);
        assert_eq!(TransferEncoding::parse("weird"), TransferEncoding::Unknown);
    }

    #[test]
    fn body_bytes_rejoins_with_crlf() {
        let article = article("From: a@b\r\n\r\none\r\ntwo\r\n.\r\n");
        assert_eq!(article.body_bytes(), b"one\r\ntwo");
    }

    #[test]
    fn an_article_with_no_headers_still_parses() {
        let article = article("\r\njust a body\r\n.\r\n");
        assert!(article.headers.is_empty());
        assert_eq!(article.body_text(), "just a body");
        assert!(article.subject().is_empty());
        assert!(article.date().is_none());
        assert!(article.message_id().is_none());
    }
}
