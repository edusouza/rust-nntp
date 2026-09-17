//! Multi-line data blocks.
//!
//! RFC 3977 §3.1.1: after certain status lines the server sends a sequence of lines
//! terminated by a line containing a single `.`. Any line in the block that begins with
//! `.` has an extra `.` prepended by the sender, which the receiver must remove. Get that
//! wrong and an article whose body contains a line starting with `.` truncates the whole
//! block — a bug that only shows up on the articles that trigger it.

use crate::headers::split_lines;

/// The terminator line of a data block, without its CRLF.
pub const TERMINATOR: &[u8] = b".";

/// Whether `line` (with its CRLF already removed) terminates a data block.
pub const fn is_terminator(line: &[u8]) -> bool {
    matches!(line, [b'.'])
}

/// Removes the dot-stuffing from a received line.
///
/// A line that begins with `.` had a second `.` prepended by the sender; only that one is
/// removed. `..text` becomes `.text`, and `...` becomes `..`.
pub fn unstuff(line: &[u8]) -> &[u8] {
    match line.split_first() {
        Some((b'.', rest)) => rest,
        _ => line,
    }
}

/// Adds dot-stuffing to a line that is about to be sent inside a data block.
///
/// The inverse of [`unstuff`]. Required for `POST` and `IHAVE`; a body line starting with
/// `.` that is not stuffed ends the block early and truncates the article.
pub fn stuff(line: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(line.len() + 1);
    if line.first() == Some(&b'.') {
        out.push(b'.');
    }
    out.extend_from_slice(line);
    out
}

/// A received multi-line data block: the lines between the status line and the
/// terminator, unstuffed, without line terminators.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DataBlock {
    lines: Vec<Vec<u8>>,
}

impl DataBlock {
    /// An empty block.
    pub fn new() -> Self {
        Self::default()
    }

    /// Wraps lines that have already been unstuffed and stripped of terminators.
    pub fn from_lines(lines: Vec<Vec<u8>>) -> Self {
        Self { lines }
    }

    /// Parses a whole block from a buffer, applying unstuffing and stopping at the
    /// terminator line.
    ///
    /// Intended for tests and for callers holding a complete response; a client reading
    /// from a socket should instead feed lines to [`Self::push_raw_line`] so it can
    /// enforce size limits as it goes.
    pub fn parse(block: &[u8]) -> Self {
        let mut out = Self::new();
        for line in split_lines(block) {
            if is_terminator(line) {
                break;
            }
            out.lines.push(unstuff(line).to_vec());
        }
        out
    }

    /// Appends a raw received line, unstuffing it.
    ///
    /// Returns `false` if the line was the block terminator, in which case it is not
    /// appended and the block is complete.
    pub fn push_raw_line(&mut self, line: &[u8]) -> bool {
        if is_terminator(line) {
            return false;
        }
        self.lines.push(unstuff(line).to_vec());
        true
    }

    /// The block's lines.
    pub fn lines(&self) -> &[Vec<u8>] {
        &self.lines
    }

    /// Consumes the block and returns its lines.
    pub fn into_lines(self) -> Vec<Vec<u8>> {
        self.lines
    }

    /// The number of lines.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Whether the block has no lines.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The total size of the block's content in octets, excluding line terminators.
    pub fn byte_len(&self) -> usize {
        self.lines.iter().map(Vec::len).sum()
    }

    /// The block rejoined with CRLF separators, as it would appear in a file.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.byte_len() + self.lines.len() * 2);
        for line in &self.lines {
            out.extend_from_slice(line);
            out.extend_from_slice(b"\r\n");
        }
        out
    }

    /// The block as text, decoding unlabelled 8-bit bytes as Windows-1252.
    ///
    /// Lines are joined with `\n`, which is what a terminal wants; use [`Self::to_bytes`]
    /// when the CRLF form matters.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for (index, line) in self.lines.iter().enumerate() {
            if index > 0 {
                out.push('\n');
            }
            out.push_str(&crate::mime::decode_8bit_lossy(line));
        }
        out
    }

    /// Splits the block at the first empty line into a header part and a body part.
    ///
    /// `ARTICLE` returns both; the empty line is the separator and belongs to neither.
    /// If there is no empty line the whole block is treated as headers, which is what a
    /// `HEAD` response looks like.
    pub fn split_head_body(&self) -> (&[Vec<u8>], &[Vec<u8>]) {
        match self.lines.iter().position(Vec::is_empty) {
            Some(index) => {
                let head = self.lines.get(..index).unwrap_or_default();
                let body = self.lines.get(index + 1..).unwrap_or_default();
                (head, body)
            }
            None => (self.lines.as_slice(), &[]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_the_terminator() {
        assert!(is_terminator(b"."));
        assert!(!is_terminator(b".."));
        assert!(!is_terminator(b""));
        assert!(!is_terminator(b". "));
        assert!(!is_terminator(b".x"));
    }

    #[test]
    fn unstuffs_only_the_first_dot() {
        assert_eq!(unstuff(b"..hidden"), b"..hidden".get(1..).unwrap());
        assert_eq!(unstuff(b"..hidden"), b".hidden");
        assert_eq!(unstuff(b"..."), b"..");
        assert_eq!(unstuff(b"plain"), b"plain");
        assert_eq!(unstuff(b""), b"");
    }

    #[test]
    fn stuffing_round_trips() {
        for line in [&b""[..], b"plain", b".hidden", b"..", b"...", b".", b"a.b"] {
            assert_eq!(unstuff(&stuff(line)), line, "round trip for {line:?}");
        }
        assert_eq!(stuff(b".signature"), b"..signature");
        assert_eq!(stuff(b"plain"), b"plain");
    }

    #[test]
    fn parses_a_block_and_stops_at_the_terminator() {
        let block = DataBlock::parse(b"one\r\ntwo\r\n.\r\nnot-part-of-the-block\r\n");
        assert_eq!(block.len(), 2);
        assert_eq!(block.lines(), [b"one".to_vec(), b"two".to_vec()]);
    }

    #[test]
    fn a_body_line_starting_with_a_dot_does_not_truncate_the_block() {
        // The bug this test exists to prevent: reading "..sig" as the terminator.
        let block = DataBlock::parse(b"before\r\n..sig\r\nafter\r\n.\r\n");
        assert_eq!(
            block.lines(),
            [b"before".to_vec(), b".sig".to_vec(), b"after".to_vec()]
        );
    }

    #[test]
    fn handles_an_empty_block() {
        let block = DataBlock::parse(b".\r\n");
        assert!(block.is_empty());
        assert_eq!(block.byte_len(), 0);
        assert!(block.to_bytes().is_empty());
        assert!(block.to_text().is_empty());
    }

    #[test]
    fn push_raw_line_reports_the_terminator() {
        let mut block = DataBlock::new();
        assert!(block.push_raw_line(b"one"));
        assert!(block.push_raw_line(b"..two"));
        assert!(!block.push_raw_line(b"."));
        assert_eq!(block.lines(), [b"one".to_vec(), b".two".to_vec()]);
    }

    #[test]
    fn rejoins_with_crlf_and_renders_text_with_lf() {
        let block = DataBlock::from_lines(vec![b"a".to_vec(), b"b".to_vec()]);
        assert_eq!(block.to_bytes(), b"a\r\nb\r\n");
        assert_eq!(block.to_text(), "a\nb");
    }

    #[test]
    fn decodes_eight_bit_text() {
        let block = DataBlock::from_lines(vec![b"caf\xe9".to_vec()]);
        assert_eq!(block.to_text(), "café");
    }

    #[test]
    fn splits_head_from_body_at_the_first_empty_line() {
        let block =
            DataBlock::parse(b"From: a@b\r\nSubject: s\r\n\r\nbody one\r\n\r\nbody two\r\n.\r\n");
        let (head, body) = block.split_head_body();
        assert_eq!(head.len(), 2);
        assert_eq!(
            body,
            [b"body one".to_vec(), Vec::new(), b"body two".to_vec()]
        );
    }

    #[test]
    fn a_head_only_block_has_no_body() {
        let block = DataBlock::parse(b"From: a@b\r\n.\r\n");
        let (head, body) = block.split_head_body();
        assert_eq!(head.len(), 1);
        assert!(body.is_empty());
    }

    #[test]
    fn an_article_with_no_headers_yields_an_empty_head() {
        let block = DataBlock::parse(b"\r\nbody\r\n.\r\n");
        let (head, body) = block.split_head_body();
        assert!(head.is_empty());
        assert_eq!(body, [b"body".to_vec()]);
    }
}
