//! MIME bodies: the part tree, and which part a reader should show.
//!
//! An article body is not always text. A `multipart/alternative` from a mail-to-news
//! gateway carries the same message twice, once as plain text and once as HTML; a
//! `multipart/mixed` carries text plus whatever was attached to it. Until this module
//! existed the reader showed the raw body — boundary lines, base64 blobs and HTML tags
//! included — which is readable only by accident.
//!
//! Everything here is parsing. Nothing decides how to draw a part, and nothing does IO.

use crate::article::{ContentType, TransferEncoding};
use crate::headers::{Headers, names};
use crate::mime;

/// How deep a nested multipart tree is walked.
///
/// `multipart/mixed` containing a `multipart/alternative` containing a `multipart/related`
/// is three, and that is already unusual. The limit exists because the body comes from a
/// remote peer and recursion driven by remote input needs a bound; a part deeper than this
/// is kept as an undivided part rather than dropped, so nothing disappears.
pub const MAX_DEPTH: usize = 8;

/// How many parts are split out of one container.
///
/// A mail-to-news gateway can produce a long `multipart/mixed`, but not thousands of
/// parts. Past this the remaining lines are kept as one final part, so again nothing is
/// silently lost.
pub const MAX_PARTS: usize = 256;

/// What a part's `Content-Disposition` says it is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispositionKind {
    /// `inline`, or no header at all: meant to be shown in place.
    Inline,
    /// `attachment`: meant to be saved, not shown.
    Attachment,
    /// Something else entirely. Treated as an attachment, since a reader that cannot name
    /// the intent should not assume the friendly one.
    Other,
}

/// A parsed `Content-Disposition`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disposition {
    /// What the header said.
    pub kind: DispositionKind,
    /// Parameters, with lowercased names.
    pub params: Vec<(String, String)>,
}

impl Disposition {
    /// Parses a `Content-Disposition` value such as `attachment; filename="notes.txt"`.
    pub fn parse(value: &str) -> Self {
        let mut pieces = value.split(';');
        let kind = match pieces
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "" | "inline" => DispositionKind::Inline,
            "attachment" => DispositionKind::Attachment,
            _ => DispositionKind::Other,
        };

        Self {
            kind,
            params: crate::article::parse_params(pieces),
        }
    }

    /// A parameter's value, by lowercased name.
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, value)| value.as_str())
    }
}

/// One MIME part: its headers, its own body lines, and any parts nested inside it.
///
/// The root part is the article itself, so a plain `text/plain` article is a tree of one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyPart {
    /// The part's own headers. Empty for a part that had none.
    pub headers: Headers,
    /// The part's `Content-Type`, defaulting to `text/plain` per RFC 2045 §5.2.
    pub content_type: ContentType,
    /// The part's `Content-Transfer-Encoding`.
    pub encoding: TransferEncoding,
    /// The part's `Content-Disposition`, if it declared one.
    pub disposition: Option<Disposition>,
    /// The raw lines of this part's body, terminators removed. For a multipart container
    /// this is everything between its delimiters, boundary lines included, which is what
    /// makes "show it raw" possible when the split cannot be trusted.
    pub lines: Vec<Vec<u8>>,
    /// Parts nested inside this one. Empty unless this is a multipart container that was
    /// split.
    pub parts: Vec<BodyPart>,
}

impl BodyPart {
    /// Builds the root part from an article's headers and body.
    pub fn from_article(headers: &Headers, body: &[Vec<u8>]) -> Self {
        let content_type = headers
            .get_decoded(names::CONTENT_TYPE)
            .map(|value| ContentType::parse(&value))
            .unwrap_or_default();
        let encoding = headers
            .get_decoded(names::CONTENT_TRANSFER_ENCODING)
            .map_or(TransferEncoding::None, |value| {
                TransferEncoding::parse(&value)
            });
        let disposition = headers
            .get_decoded(names::CONTENT_DISPOSITION)
            .map(|value| Disposition::parse(&value));

        let mut part = Self {
            headers: headers.clone(),
            content_type,
            encoding,
            disposition,
            lines: body.to_vec(),
            parts: Vec::new(),
        };
        part.split(0);
        part
    }

    /// Splits this part's lines into nested parts, if it is a multipart container.
    fn split(&mut self, depth: usize) {
        if depth >= MAX_DEPTH || !self.content_type.is_multipart() {
            return;
        }
        // A multipart with no boundary parameter cannot be split at all (RFC 2046 §5.1.1
        // requires it). Showing it whole is the honest outcome.
        let Some(boundary) = self.content_type.boundary().map(str::to_owned) else {
            return;
        };
        if boundary.is_empty() {
            return;
        }

        for chunk in split_on_boundary(&self.lines, &boundary) {
            let (headers, body) = split_part_headers(&chunk);
            let mut child = Self::without_splitting(headers, body);
            child.split(depth + 1);
            self.parts.push(child);
        }
    }

    /// Builds a part from already-separated headers and body, without recursing.
    fn without_splitting(headers: Headers, lines: Vec<Vec<u8>>) -> Self {
        let content_type = headers
            .get_decoded(names::CONTENT_TYPE)
            .map(|value| ContentType::parse(&value))
            .unwrap_or_default();
        let encoding = headers
            .get_decoded(names::CONTENT_TRANSFER_ENCODING)
            .map_or(TransferEncoding::None, |value| {
                TransferEncoding::parse(&value)
            });
        let disposition = headers
            .get_decoded(names::CONTENT_DISPOSITION)
            .map(|value| Disposition::parse(&value));

        Self {
            headers,
            content_type,
            encoding,
            disposition,
            lines,
            parts: Vec::new(),
        }
    }

    /// Whether this part is a container rather than content.
    pub fn is_multipart(&self) -> bool {
        self.content_type.is_multipart()
    }

    /// Whether this part is meant to be saved rather than shown.
    ///
    /// A part with a `filename` is treated as an attachment even when it claims to be
    /// inline: the sender naming a file is the clearer signal of intent, and it is what
    /// other readers do.
    pub fn is_attachment(&self) -> bool {
        match &self.disposition {
            Some(disposition) => match disposition.kind {
                DispositionKind::Attachment | DispositionKind::Other => true,
                DispositionKind::Inline => self.filename().is_some(),
            },
            None => false,
        }
    }

    /// The file name the sender suggested, decoded.
    ///
    /// Reads `filename` from `Content-Disposition`, then RFC 2231's `filename*`, then
    /// `name` from `Content-Type`, which is where older software put it. RFC 2047 encoded
    /// words are decoded too: they are not legal here, but they are common, and showing
    /// `=?UTF-8?Q?relat=C3=B3rio?=` to a user helps nobody.
    pub fn filename(&self) -> Option<String> {
        let raw = self
            .disposition
            .as_ref()
            .and_then(|disposition| disposition.param("filename"))
            .map(str::to_owned);

        if let Some(name) = raw {
            return Some(mime::decode_header_value(name.as_bytes()));
        }

        if let Some(extended) = self
            .disposition
            .as_ref()
            .and_then(|disposition| disposition.param("filename*"))
        {
            return Some(decode_extended_parameter(extended));
        }

        self.content_type
            .param("name")
            .map(|name| mime::decode_header_value(name.as_bytes()))
    }

    /// The size of this part's body as it arrived, in octets.
    ///
    /// Before decoding, so a base64 attachment reports about a third more than the file
    /// it will become. That is the honest number for "how much did this article cost",
    /// and it is the only one available without decoding the part.
    pub fn encoded_size(&self) -> usize {
        self.lines
            .iter()
            .map(|line| line.len() + 2)
            .sum::<usize>()
            .saturating_sub(2)
    }

    /// This part's body as displayable text.
    ///
    /// Applies the transfer encoding, then the declared charset, falling back to
    /// Windows-1252 for unlabelled 8-bit bytes. `format=flowed` is *not* applied here:
    /// that is a display decision, and [`unflow`] is separate so a caller can choose.
    pub fn text(&self) -> String {
        let raw = join_lines(&self.lines);

        let decoded = match self.encoding {
            TransferEncoding::None | TransferEncoding::Unknown => raw,
            TransferEncoding::QuotedPrintable => mime::decode_quoted_printable(&raw),
            TransferEncoding::Base64 => mime::decode_base64(&raw).unwrap_or(raw),
        };

        let text = match self.content_type.charset() {
            Some(charset) => mime::decode_with_charset(&decoded, charset),
            None => mime::decode_8bit_lossy(&decoded),
        };

        text.replace("\r\n", "\n")
    }

    /// Whether this part can be shown as text at all.
    pub fn is_displayable(&self) -> bool {
        self.content_type.is_text() && !self.is_attachment()
    }

    /// The part a reader should show, or `None` if nothing here is text.
    ///
    /// For `multipart/alternative` the *least* faithful renderable part wins, which in
    /// practice means `text/plain` over `text/html`. That is the opposite of RFC 2046
    /// §5.1.4, which says to prefer the last part a reader can handle — written when the
    /// last part was the richest one the reader could actually render. In a terminal,
    /// `text/html` is not something this reader renders; it is something it would dump
    /// tags from. Preferring plain text is the honest reading of the rule's intent.
    ///
    /// For every other multipart type the first displayable part wins, depth first, which
    /// is the text that precedes the attachments in a `multipart/mixed`.
    pub fn display_part(&self) -> Option<&Self> {
        if !self.is_multipart() {
            return self.is_displayable().then_some(self);
        }

        if self.parts.is_empty() {
            // A container that could not be split. Showing it raw beats showing nothing,
            // and `text` will hand back the boundary lines as they arrived.
            return Some(self);
        }

        if self.content_type.subtype == "alternative" {
            let plain = self.parts.iter().find_map(|part| {
                part.display_part()
                    .filter(|candidate| candidate.content_type.subtype == "plain")
            });
            if plain.is_some() {
                return plain;
            }
        }

        self.parts.iter().find_map(Self::display_part)
    }

    /// Every part that is not the one being displayed and is not a container: the
    /// attachments and the alternatives the reader passed over.
    ///
    /// Listing them matters even though saving them is a later concern. An article whose
    /// text says "see the attached patch" is confusing if the reader never mentions that
    /// something was attached.
    pub fn other_parts(&self) -> Vec<&Self> {
        let shown = self.display_part().map(std::ptr::from_ref);
        let mut out = Vec::new();
        self.collect_leaves(shown, &mut out);
        out
    }

    fn collect_leaves<'a>(&'a self, shown: Option<*const Self>, out: &mut Vec<&'a Self>) {
        if self.is_multipart() && !self.parts.is_empty() {
            for part in &self.parts {
                part.collect_leaves(shown, out);
            }
            return;
        }

        if shown != Some(std::ptr::from_ref(self)) {
            out.push(self);
        }
    }

    /// A short description of this part for a list of attachments: type, name and size.
    pub fn summary(&self) -> String {
        let kind = if self.content_type.subtype.is_empty() {
            self.content_type.type_.clone()
        } else {
            format!("{}/{}", self.content_type.type_, self.content_type.subtype)
        };

        let size = self.encoded_size();
        match self.filename() {
            Some(name) => format!("{kind} — {name} ({size} octets)"),
            None => format!("{kind} ({size} octets)"),
        }
    }
}

/// Rejoins lines with CRLF, as they arrived on the wire.
fn join_lines(lines: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(line);
    }
    out
}

/// Splits multipart lines into one chunk per part.
///
/// Per RFC 2046 §5.1.1: everything before the first delimiter is the preamble and is
/// discarded, `--boundary` starts a part, `--boundary--` ends the last one, and anything
/// after that is the epilogue. Trailing whitespace on a delimiter line is allowed, which
/// real senders do produce.
fn split_on_boundary(lines: &[Vec<u8>], boundary: &str) -> Vec<Vec<Vec<u8>>> {
    let delimiter = format!("--{boundary}");
    let close = format!("--{boundary}--");

    let mut chunks: Vec<Vec<Vec<u8>>> = Vec::new();
    let mut current: Option<Vec<Vec<u8>>> = None;

    for line in lines {
        let trimmed = trim_trailing_space(line);

        if trimmed == close.as_bytes() {
            if let Some(chunk) = current.take() {
                chunks.push(chunk);
            }
            break;
        }

        if trimmed == delimiter.as_bytes() {
            if let Some(chunk) = current.take() {
                chunks.push(chunk);
            }
            if chunks.len() >= MAX_PARTS {
                // Stop splitting rather than growing without bound. The remaining lines
                // are dropped from the tree but still reachable through the container's
                // own `lines`, which is why this is a limit rather than a loss.
                return chunks;
            }
            current = Some(Vec::new());
            continue;
        }

        if let Some(chunk) = current.as_mut() {
            chunk.push(line.clone());
        }
        // Otherwise this is the preamble, before the first delimiter: discarded.
    }

    // A missing close delimiter is common in truncated articles. Keep what was collected.
    if let Some(chunk) = current.take() {
        chunks.push(chunk);
    }

    chunks
}

/// Splits a part's lines into its headers and its body, at the first blank line.
fn split_part_headers(lines: &[Vec<u8>]) -> (Headers, Vec<Vec<u8>>) {
    let blank = lines.iter().position(|line| line.is_empty());

    match blank {
        Some(index) => {
            let head = lines.get(..index).unwrap_or_default();
            let body = lines.get(index + 1..).unwrap_or_default();
            (
                Headers::parse_lines(head.iter().map(Vec::as_slice)),
                body.to_vec(),
            )
        }
        // No blank line at all: RFC 2046 says a part always has a header section, even an
        // empty one, but truncated and hand-built articles do turn up without it. Treating
        // the whole thing as a body shows the text; treating it as headers would show
        // nothing.
        None => (Headers::default(), lines.to_vec()),
    }
}

fn trim_trailing_space(line: &[u8]) -> &[u8] {
    let end = line
        .iter()
        .rposition(|byte| !matches!(byte, b' ' | b'\t'))
        .map_or(0, |index| index + 1);
    line.get(..end).unwrap_or_default()
}

/// Decodes an RFC 2231 extended parameter value: `UTF-8''caf%C3%A9.txt`.
fn decode_extended_parameter(value: &str) -> String {
    // charset'language'percent-encoded-text. Either of the first two may be empty.
    let mut pieces = value.splitn(3, '\'');
    let charset = pieces.next().unwrap_or_default();
    let _language = pieces.next();
    let encoded = pieces.next().unwrap_or(value);

    let mut bytes = Vec::with_capacity(encoded.len());
    let mut characters = encoded.chars();
    while let Some(character) = characters.next() {
        if character != '%' {
            let mut buffer = [0_u8; 4];
            bytes.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
            continue;
        }

        let high = characters.next().and_then(|c| c.to_digit(16));
        let low = characters.next().and_then(|c| c.to_digit(16));
        match (high, low) {
            (Some(high), Some(low)) => bytes.push((high * 16 + low) as u8),
            // A stray `%` is kept as itself rather than swallowed.
            _ => bytes.push(b'%'),
        }
    }

    if charset.is_empty() {
        mime::decode_8bit_lossy(&bytes)
    } else {
        mime::decode_with_charset(&bytes, charset)
    }
}

/// Rejoins the soft line breaks of a `format=flowed` body (RFC 3676).
///
/// A flowed body marks a soft break with a trailing space: the line continues. Without
/// this the reader shows a paragraph one short line at a time, which is how a
/// seventy-column mail client's output looks in a hundred-column terminal.
///
/// Three rules that matter and are easy to get wrong:
///
/// - **Quote depth is part of the line.** `> ` prefixes are counted and only lines at the
///   same depth are joined, so a reply does not absorb the text it is quoting.
/// - **Space-stuffing is undone.** A line may begin with an inserted space to protect a
///   leading `>` or `From `; it is removed before display (§4.4).
/// - **`delsp=yes` deletes the space** that marked the break instead of keeping it, which
///   is what makes flowed text work for languages that do not put spaces between words.
///
/// A signature separator (`-- `) ends flowing, per §4.3: it is a hard break that happens
/// to end in a space, and joining it to the next line is a visible, familiar bug.
pub fn unflow(text: &str, delete_space: bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut open_depth: Option<usize> = None;

    // One trailing newline is a terminator, not an empty last line: `split` would other-
    // wise hand back an extra empty piece and this would add a blank line to every body.
    let body = text.strip_suffix('\n').unwrap_or(text);

    for line in body.split('\n') {
        let (depth, rest) = split_quote_prefix(line);
        // Space-stuffing (§4.4): one leading space is an escape, not content.
        let content = rest.strip_prefix(' ').unwrap_or(rest);

        let flowed = content.ends_with(' ') && content != "-- ";

        match open_depth {
            // Continuing a paragraph at the same quote depth: the prefix was written when
            // the paragraph opened.
            Some(open) if open == depth => {}
            // A different depth ends the paragraph, whatever it was doing. The space
            // that marked the open line as flowed is dropped: it was a marker, and with
            // nothing to join it to, keeping it would leave a trailing space on screen.
            Some(_) => {
                trim_trailing_spaces(&mut out);
                out.push('\n');
                push_quote_prefix(&mut out, depth, content);
            }
            None => push_quote_prefix(&mut out, depth, content),
        }

        if flowed {
            let body = if delete_space {
                content.strip_suffix(' ').unwrap_or(content)
            } else {
                content
            };
            out.push_str(body);
            open_depth = Some(depth);
        } else {
            out.push_str(content);
            out.push('\n');
            open_depth = None;
        }
    }

    // A body that ends mid-paragraph — the sender's last line was marked flowed — leaves
    // its marker space with nothing to join to, same as above.
    if open_depth.is_some() {
        trim_trailing_spaces(&mut out);
    }

    // The last line, if it was not flowed, contributed a newline that no line follows.
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

fn trim_trailing_spaces(out: &mut String) {
    while out.ends_with(' ') {
        out.pop();
    }
}

/// Counts the `>` quote prefix of a line and returns the rest.
fn split_quote_prefix(line: &str) -> (usize, &str) {
    let mut depth = 0;
    let mut rest = line;
    while let Some(stripped) = rest.strip_prefix('>') {
        depth += 1;
        rest = stripped;
    }
    (depth, rest)
}

/// Writes a line's `>` prefix, with one space before the text.
///
/// The space is re-inserted rather than preserved. Space-stuffing (§4.4) has already
/// removed one leading space, and in a quoted line that space is usually the one
/// separating `>` from the text — so without this, `> quoted` would be displayed as
/// `>quoted`. Emitting it here means the result looks the same whether or not the sender
/// stuffed, which is the point: this text goes on a screen.
fn push_quote_prefix(out: &mut String, depth: usize, content: &str) {
    for _ in 0..depth {
        out.push('>');
    }
    if depth > 0 && !content.is_empty() {
        out.push(' ');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::DataBlock;

    fn part_of(text: &str) -> BodyPart {
        let block = DataBlock::parse(text.as_bytes());
        let (head, body) = block.split_head_body();
        let headers = Headers::parse_lines(head.iter().map(Vec::as_slice));
        BodyPart::from_article(&headers, body)
    }

    #[test]
    fn a_plain_article_is_a_tree_of_one() {
        let part = part_of(
            "Content-Type: text/plain; charset=UTF-8\r\n\
             \r\n\
             just text\r\n.\r\n",
        );

        assert!(!part.is_multipart());
        assert!(part.parts.is_empty());
        assert_eq!(part.text(), "just text");
        assert_eq!(
            part.display_part().map(BodyPart::text),
            Some("just text".to_owned())
        );
        assert!(part.other_parts().is_empty());
    }

    #[test]
    fn an_article_with_no_content_type_is_text_plain() {
        // RFC 2045 §5.2. Most of Usenet sends no Content-Type at all.
        let part = part_of("From: a@b\r\n\r\nhello\r\n.\r\n");

        assert!(part.content_type.is_text());
        assert_eq!(
            part.display_part().map(BodyPart::text),
            Some("hello".into())
        );
    }

    #[test]
    fn multipart_alternative_shows_the_plain_text_part() {
        let part = part_of(
            "Content-Type: multipart/alternative; boundary=\"sep\"\r\n\
             \r\n\
             this preamble is not part of any part\r\n\
             --sep\r\n\
             Content-Type: text/plain; charset=UTF-8\r\n\
             \r\n\
             the readable version\r\n\
             --sep\r\n\
             Content-Type: text/html; charset=UTF-8\r\n\
             \r\n\
             <p>the noisy version</p>\r\n\
             --sep--\r\n\
             and this epilogue is not either\r\n.\r\n",
        );

        assert!(part.is_multipart());
        assert_eq!(part.parts.len(), 2);
        // RFC 2046 says prefer the last renderable part; in a terminal the plain one is
        // the one a reader can actually render.
        assert_eq!(
            part.display_part().map(BodyPart::text),
            Some("the readable version".to_owned())
        );
        // The HTML alternative is still reachable rather than hidden.
        let others = part.other_parts();
        assert_eq!(others.len(), 1);
        assert_eq!(
            others.first().map(|p| p.content_type.subtype.as_str()),
            Some("html")
        );
    }

    #[test]
    fn multipart_alternative_falls_back_to_html_when_there_is_no_plain_part() {
        let part = part_of(
            "Content-Type: multipart/alternative; boundary=\"sep\"\r\n\
             \r\n\
             --sep\r\n\
             Content-Type: text/html\r\n\
             \r\n\
             <p>only this</p>\r\n\
             --sep--\r\n.\r\n",
        );

        // Showing HTML tags is poor; showing nothing at all is worse.
        assert_eq!(
            part.display_part().map(BodyPart::text),
            Some("<p>only this</p>".to_owned())
        );
    }

    #[test]
    fn multipart_mixed_shows_the_text_and_lists_the_attachment() {
        let part = part_of(
            "Content-Type: multipart/mixed; boundary=\"b\"\r\n\
             \r\n\
             --b\r\n\
             Content-Type: text/plain\r\n\
             \r\n\
             see the attached patch\r\n\
             --b\r\n\
             Content-Type: text/x-patch; name=\"fix.patch\"\r\n\
             Content-Disposition: attachment; filename=\"fix.patch\"\r\n\
             \r\n\
             --- a/x\r\n\
             +++ b/x\r\n\
             --b--\r\n.\r\n",
        );

        assert_eq!(
            part.display_part().map(BodyPart::text),
            Some("see the attached patch".to_owned())
        );

        let others = part.other_parts();
        assert_eq!(others.len(), 1);
        let attachment = others.first().expect("one attachment");
        assert!(attachment.is_attachment());
        assert_eq!(attachment.filename().as_deref(), Some("fix.patch"));
        assert!(
            attachment.summary().contains("fix.patch"),
            "{}",
            attachment.summary()
        );
        assert!(
            attachment.summary().contains("octets"),
            "{}",
            attachment.summary()
        );
    }

    #[test]
    fn a_text_part_marked_as_an_attachment_is_not_shown_as_the_body() {
        let part = part_of(
            "Content-Type: multipart/mixed; boundary=\"b\"\r\n\
             \r\n\
             --b\r\n\
             Content-Type: text/plain\r\n\
             Content-Disposition: attachment; filename=\"notes.txt\"\r\n\
             \r\n\
             attached notes\r\n\
             --b\r\n\
             Content-Type: text/plain\r\n\
             \r\n\
             the actual message\r\n\
             --b--\r\n.\r\n",
        );

        assert_eq!(
            part.display_part().map(BodyPart::text),
            Some("the actual message".to_owned())
        );
    }

    #[test]
    fn an_inline_part_with_a_file_name_counts_as_an_attachment() {
        // Senders label images inline with a filename; treating that as the message body
        // would replace the text with whatever the image decoded to.
        let part = part_of(
            "Content-Type: multipart/mixed; boundary=\"b\"\r\n\
             \r\n\
             --b\r\n\
             Content-Type: text/plain\r\n\
             Content-Disposition: inline; filename=\"quote.txt\"\r\n\
             \r\n\
             inline but named\r\n\
             --b\r\n\
             Content-Type: text/plain\r\n\
             \r\n\
             the message\r\n\
             --b--\r\n.\r\n",
        );

        assert_eq!(
            part.display_part().map(BodyPart::text),
            Some("the message".to_owned())
        );
    }

    #[test]
    fn nested_multiparts_are_walked() {
        let part = part_of(
            "Content-Type: multipart/mixed; boundary=\"outer\"\r\n\
             \r\n\
             --outer\r\n\
             Content-Type: multipart/alternative; boundary=\"inner\"\r\n\
             \r\n\
             --inner\r\n\
             Content-Type: text/plain\r\n\
             \r\n\
             nested plain text\r\n\
             --inner\r\n\
             Content-Type: text/html\r\n\
             \r\n\
             <p>nested html</p>\r\n\
             --inner--\r\n\
             --outer\r\n\
             Content-Type: application/octet-stream\r\n\
             Content-Disposition: attachment; filename=\"blob.bin\"\r\n\
             \r\n\
             AAAA\r\n\
             --outer--\r\n.\r\n",
        );

        assert_eq!(
            part.display_part().map(BodyPart::text),
            Some("nested plain text".to_owned())
        );
        // The html alternative and the binary attachment, not the containers.
        let others = part.other_parts();
        assert_eq!(
            others.len(),
            2,
            "{:?}",
            others.iter().map(|p| p.summary()).collect::<Vec<_>>()
        );
        assert!(
            others
                .iter()
                .any(|p| p.filename().as_deref() == Some("blob.bin"))
        );
    }

    #[test]
    fn a_base64_part_is_decoded() {
        let part = part_of(
            "Content-Type: multipart/mixed; boundary=\"b\"\r\n\
             \r\n\
             --b\r\n\
             Content-Type: text/plain; charset=UTF-8\r\n\
             Content-Transfer-Encoding: base64\r\n\
             \r\n\
             Y2Fm\r\n\
             w6k=\r\n\
             --b--\r\n.\r\n",
        );

        assert_eq!(part.display_part().map(BodyPart::text), Some("café".into()));
    }

    #[test]
    fn a_quoted_printable_part_is_decoded() {
        let part = part_of(
            "Content-Type: multipart/mixed; boundary=\"b\"\r\n\
             \r\n\
             --b\r\n\
             Content-Type: text/plain; charset=UTF-8\r\n\
             Content-Transfer-Encoding: quoted-printable\r\n\
             \r\n\
             caf=C3=A9\r\n\
             --b--\r\n.\r\n",
        );

        assert_eq!(part.display_part().map(BodyPart::text), Some("café".into()));
    }

    #[test]
    fn a_multipart_with_no_boundary_is_shown_whole_rather_than_lost() {
        let part = part_of(
            "Content-Type: multipart/mixed\r\n\
             \r\n\
             whatever this is\r\n.\r\n",
        );

        assert!(part.parts.is_empty());
        assert_eq!(
            part.display_part().map(BodyPart::text),
            Some("whatever this is".to_owned())
        );
    }

    #[test]
    fn a_truncated_multipart_keeps_the_part_it_did_receive() {
        // No closing delimiter: the article was cut short, or the sender was careless.
        let part = part_of(
            "Content-Type: multipart/mixed; boundary=\"b\"\r\n\
             \r\n\
             --b\r\n\
             Content-Type: text/plain\r\n\
             \r\n\
             the beginning of something\r\n.\r\n",
        );

        assert_eq!(part.parts.len(), 1);
        assert_eq!(
            part.display_part().map(BodyPart::text),
            Some("the beginning of something".to_owned())
        );
    }

    #[test]
    fn a_delimiter_with_trailing_whitespace_still_delimits() {
        let part = part_of(
            "Content-Type: multipart/mixed; boundary=\"b\"\r\n\
             \r\n\
             --b  \r\n\
             Content-Type: text/plain\r\n\
             \r\n\
             text\r\n\
             --b--\t\r\n.\r\n",
        );

        assert_eq!(part.parts.len(), 1);
        assert_eq!(part.display_part().map(BodyPart::text), Some("text".into()));
    }

    #[test]
    fn a_part_with_no_header_section_is_treated_as_body() {
        let part = part_of(
            "Content-Type: multipart/mixed; boundary=\"b\"\r\n\
             \r\n\
             --b\r\n\
             no headers here, just text\r\n\
             --b--\r\n.\r\n",
        );

        assert_eq!(
            part.display_part().map(BodyPart::text),
            Some("no headers here, just text".to_owned())
        );
    }

    #[test]
    fn nesting_deeper_than_the_limit_stops_splitting_instead_of_recursing() {
        // Built rather than written out: the point is the depth, not the content.
        let mut body = String::from("innermost\r\n");
        for level in (0..MAX_DEPTH + 4).rev() {
            body = format!(
                "Content-Type: multipart/mixed; boundary=\"b{level}\"\r\n\
                 \r\n\
                 --b{level}\r\n\
                 {body}\
                 --b{level}--\r\n"
            );
        }
        let part = part_of(&format!("{body}.\r\n"));

        // It terminates, and nothing panics. The deepest part that was split is kept
        // whole, so the text is still reachable.
        let mut depth = 0;
        let mut node = &part;
        while let Some(child) = node.parts.first() {
            depth += 1;
            node = child;
        }
        assert_eq!(depth, MAX_DEPTH);
        assert!(node.text().contains("innermost"), "{}", node.text());
    }

    #[test]
    fn more_parts_than_the_limit_stops_splitting() {
        let mut body = String::new();
        for index in 0..MAX_PARTS + 10 {
            body.push_str(&format!(
                "--b\r\nContent-Type: text/plain\r\n\r\npart {index}\r\n"
            ));
        }
        let part = part_of(&format!(
            "Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n{body}--b--\r\n.\r\n"
        ));

        assert_eq!(part.parts.len(), MAX_PARTS);
    }

    #[test]
    fn an_rfc_2231_file_name_is_decoded() {
        let disposition = Disposition::parse("attachment; filename*=UTF-8''relat%C3%B3rio.txt");
        let part = BodyPart {
            headers: Headers::default(),
            content_type: ContentType::default(),
            encoding: TransferEncoding::None,
            disposition: Some(disposition),
            lines: Vec::new(),
            parts: Vec::new(),
        };

        assert_eq!(part.filename().as_deref(), Some("relatório.txt"));
    }

    #[test]
    fn an_rfc_2047_file_name_is_decoded_even_though_it_is_not_legal_there() {
        let disposition = Disposition::parse("attachment; filename=\"=?UTF-8?Q?caf=C3=A9.txt?=\"");
        let part = BodyPart {
            headers: Headers::default(),
            content_type: ContentType::default(),
            encoding: TransferEncoding::None,
            disposition: Some(disposition),
            lines: Vec::new(),
            parts: Vec::new(),
        };

        assert_eq!(part.filename().as_deref(), Some("café.txt"));
    }

    #[test]
    fn a_disposition_is_read_for_what_it_is() {
        assert_eq!(Disposition::parse("inline").kind, DispositionKind::Inline);
        assert_eq!(
            Disposition::parse("ATTACHMENT; filename=x").kind,
            DispositionKind::Attachment
        );
        assert_eq!(Disposition::parse("").kind, DispositionKind::Inline);
        // Anything unrecognised is treated as an attachment rather than shown.
        assert_eq!(
            Disposition::parse("form-data; name=x").kind,
            DispositionKind::Other
        );
    }

    #[test]
    fn flowed_lines_are_joined_into_paragraphs() {
        let flowed =
            "This is a long paragraph \nthat was wrapped by the \nsender.\nA new paragraph.\n";

        assert_eq!(
            unflow(flowed, false),
            "This is a long paragraph that was wrapped by the sender.\nA new paragraph."
        );
    }

    #[test]
    fn delsp_deletes_the_space_that_marked_the_break() {
        // What makes flowed text work for languages that do not separate words with
        // spaces: the trailing space is the marker, not content.
        assert_eq!(unflow("abc \ndef\n", true), "abcdef");
        assert_eq!(unflow("abc \ndef\n", false), "abc def");
    }

    #[test]
    fn quote_depth_is_respected_when_joining() {
        let flowed = "> quoted and \n> wrapped\nmy reply, also \nwrapped\n";

        assert_eq!(
            unflow(flowed, false),
            "> quoted and wrapped\nmy reply, also wrapped"
        );
    }

    #[test]
    fn a_change_of_quote_depth_ends_a_paragraph() {
        // Without this the reply absorbs the text it is quoting, which reads as the
        // quoted author saying something they did not.
        let flowed = ">> deep and \n> shallower\n";

        assert_eq!(unflow(flowed, false), ">> deep and\n> shallower");
    }

    #[test]
    fn space_stuffing_is_undone() {
        // A line beginning with a space protects a leading `>` or `From ` (§4.4).
        assert_eq!(unflow(" >not a quote\n", false), ">not a quote");
        assert_eq!(unflow(" From here\n", false), "From here");
    }

    #[test]
    fn a_signature_separator_does_not_flow() {
        // `-- ` ends in a space but is a hard break (§4.3). Joining it to the next line is
        // the classic visible bug in a flowed implementation.
        let flowed = "the message\n-- \nÅsa\n";

        assert_eq!(unflow(flowed, false), "the message\n-- \nÅsa");
    }

    #[test]
    fn text_that_is_not_flowed_survives_unflowing() {
        let plain = "line one\nline two\n\nline four";

        assert_eq!(unflow(plain, false), plain);
    }

    #[test]
    fn unflowing_an_empty_body_is_empty() {
        assert_eq!(unflow("", false), "");
        assert_eq!(unflow("\n", false), "");
    }
}
