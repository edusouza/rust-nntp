//! Decoding header text and article text into Rust strings.
//!
//! Two problems are solved here.
//!
//! **RFC 2047 encoded words.** Header fields are restricted to US-ASCII, so non-ASCII
//! subjects and author names arrive encoded as `=?UTF-8?Q?caf=C3=A9?=`. Without decoding
//! them a reader shows mojibake on a large fraction of real traffic.
//!
//! **Unlabelled 8-bit text.** Plenty of articles put raw 8-bit bytes straight into headers
//! and bodies with no charset declaration at all. There is no correct answer for those, so
//! [`decode_8bit_lossy`] tries UTF-8 first and falls back to Windows-1252, which is what
//! browsers and other news readers do and what the bytes usually are.

use base64::Engine as _;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};
use base64::engine::{DecodePaddingMode, general_purpose::STANDARD as BASE64_STANDARD};

/// Base64 engine that tolerates the padding and trailing-bit sloppiness found in real
/// encoded words. A strict decoder rejects a noticeable share of live traffic.
const BASE64_LENIENT: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

/// Decodes a header field value, expanding any RFC 2047 encoded words.
///
/// Text outside encoded words is decoded with [`decode_8bit_lossy`]. Per RFC 2047 §6.2,
/// whitespace that separates two adjacent encoded words is dropped, since it exists only
/// to allow the header to be folded.
///
/// A sequence that looks like an encoded word but is malformed — unknown charset, bad
/// base64, missing terminator — is passed through literally rather than discarded: showing
/// `=?x-unknown?B?zzz?=` is more useful than showing nothing.
pub fn decode_header_value(bytes: &[u8]) -> String {
    let pieces = split_encoded_words(bytes);
    let mut out = String::new();

    for (index, piece) in pieces.iter().enumerate() {
        match piece {
            Piece::Encoded(text) => out.push_str(text),
            Piece::Literal(raw) => {
                if is_blank(raw) && between_encoded_words(&pieces, index) {
                    continue;
                }
                out.push_str(&decode_8bit_lossy(raw));
            }
        }
    }

    out
}

/// Encodes a header field value as RFC 2047 encoded words, if it needs it at all.
///
/// The mirror of [`decode_header_value`], and the reason it exists is posting: a header
/// field is ASCII (RFC 5536 §2.2), so a subject written by somebody whose name or language
/// has an accent in it cannot be sent verbatim. Plenty of servers would accept the raw
/// bytes; sending them anyway would make this reader one of the programs that put mojibake
/// into other people's newsreaders.
///
/// Pure ASCII is returned unchanged — wrapping `Re: hello` in an encoded word would be
/// legal, unreadable in a client that does not decode it, and pointless.
///
/// "B" (base64) rather than "Q": "Q" is more readable for a value that is *mostly* ASCII,
/// and unreadable the moment it is not, and having one encoder is worth more here than
/// saving bytes on a subject line.
pub fn encode_header_value(value: &str) -> String {
    if value.is_ascii() {
        return value.to_owned();
    }

    // RFC 2047 §2: an encoded word is at most 75 characters including the markers, and a
    // long value becomes several of them separated by a space. 15 markers + 4/3 expansion
    // leaves 45 input bytes per word, rounded down to a multiple of 3 so that no word ends
    // in base64 padding it does not need.
    const PREFIX: &str = "=?UTF-8?B?";
    const SUFFIX: &str = "?=";
    const BYTES_PER_WORD: usize = 45;

    let mut words = Vec::new();
    let mut chunk = Vec::new();

    // Split on character boundaries, never in the middle of one: half a code point in one
    // encoded word and half in the next decodes to a replacement character.
    for character in value.chars() {
        let mut buffer = [0u8; 4];
        let encoded = character.encode_utf8(&mut buffer).as_bytes();
        if chunk.len() + encoded.len() > BYTES_PER_WORD {
            words.push(std::mem::take(&mut chunk));
        }
        chunk.extend_from_slice(encoded);
    }
    if !chunk.is_empty() {
        words.push(chunk);
    }

    words
        .iter()
        .map(|chunk| format!("{PREFIX}{}{SUFFIX}", BASE64_STANDARD.encode(chunk)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Decodes bytes labelled with a charset name, such as a MIME `charset=` parameter.
///
/// An unknown or absent label falls back to [`decode_8bit_lossy`]. The label may carry an
/// RFC 2231 language suffix (`UTF-8*en`), which is ignored.
pub fn decode_with_charset(bytes: &[u8], label: &str) -> String {
    let label = label.split('*').next().unwrap_or(label).trim();
    let label = label.trim_matches('"');

    match encoding_rs::Encoding::for_label(label.as_bytes()) {
        Some(encoding) => encoding.decode(bytes).0.into_owned(),
        None => decode_8bit_lossy(bytes),
    }
}

/// Decodes bytes of unknown encoding: UTF-8 if valid, otherwise Windows-1252.
///
/// Windows-1252 maps every byte to some character, so this never fails and never inserts a
/// replacement character. It can be wrong — the bytes might be KOI8-R — but it is right
/// far more often than any other single guess for Usenet traffic, and it is reversible
/// enough that a future charset-detection pass can improve on it.
pub fn decode_8bit_lossy(bytes: &[u8]) -> String {
    match core::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned(),
    }
}

/// A run of a header value: either a decoded encoded-word or raw bytes.
#[derive(Debug, PartialEq, Eq)]
enum Piece<'a> {
    Encoded(String),
    Literal(&'a [u8]),
}

fn is_blank(raw: &[u8]) -> bool {
    !raw.is_empty()
        && raw
            .iter()
            .all(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
}

/// Whether the literal at `index` sits between two encoded words.
fn between_encoded_words(pieces: &[Piece<'_>], index: usize) -> bool {
    let before = index
        .checked_sub(1)
        .and_then(|i| pieces.get(i))
        .is_some_and(|p| matches!(p, Piece::Encoded(_)));
    let after = pieces
        .get(index + 1)
        .is_some_and(|p| matches!(p, Piece::Encoded(_)));
    before && after
}

/// Splits a header value into literal runs and decoded encoded-words.
fn split_encoded_words(bytes: &[u8]) -> Vec<Piece<'_>> {
    let mut pieces = Vec::new();
    let mut literal_start = 0usize;
    let mut cursor = 0usize;

    while cursor < bytes.len() {
        let Some(rest) = bytes.get(cursor..) else {
            break;
        };
        if !rest.starts_with(b"=?") {
            cursor += 1;
            continue;
        }

        match parse_encoded_word(rest) {
            Some((decoded, consumed)) => {
                if let Some(literal) = bytes.get(literal_start..cursor)
                    && !literal.is_empty()
                {
                    pieces.push(Piece::Literal(literal));
                }
                pieces.push(Piece::Encoded(decoded));
                cursor += consumed;
                literal_start = cursor;
            }
            // Not a well-formed encoded word: keep scanning from the next byte so that
            // `=?=?UTF-8?Q?x?=` still finds the inner one.
            None => cursor += 1,
        }
    }

    if let Some(tail) = bytes.get(literal_start..)
        && !tail.is_empty()
    {
        pieces.push(Piece::Literal(tail));
    }

    pieces
}

/// Parses one `=?charset?encoding?text?=` at the start of `input`.
///
/// Returns the decoded text and how many bytes were consumed, or `None` if `input` does
/// not begin with a well-formed encoded word.
fn parse_encoded_word(input: &[u8]) -> Option<(String, usize)> {
    let after_prefix = input.strip_prefix(b"=?")?;

    let charset_len = after_prefix.iter().position(|&b| b == b'?')?;
    let charset = after_prefix.get(..charset_len)?;
    // RFC 2047 §2 limits an encoded word to 75 octets and forbids whitespace inside it.
    if charset.is_empty() || charset.iter().any(|b| b.is_ascii_whitespace()) {
        return None;
    }

    let after_charset = after_prefix.get(charset_len + 1..)?;
    let (&encoding, after_encoding) = after_charset.split_first()?;
    if !matches!(after_encoding.first(), Some(b'?')) {
        return None;
    }
    let encoded_text_start = after_encoding.get(1..)?;

    let text_len = find_subsequence(encoded_text_start, b"?=")?;
    let encoded_text = encoded_text_start.get(..text_len)?;
    if encoded_text.iter().any(|b| b.is_ascii_whitespace()) {
        return None;
    }

    let raw = match encoding {
        b'q' | b'Q' => decode_q(encoded_text),
        b'b' | b'B' => decode_b(encoded_text)?,
        _ => return None,
    };

    let charset_label = core::str::from_utf8(charset).ok()?;
    let decoded = decode_with_charset(&raw, charset_label);

    // "=?" + charset + "?" + encoding + "?" + text + "?="
    let consumed = 2 + charset.len() + 1 + 1 + 1 + text_len + 2;
    Some((decoded, consumed))
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// RFC 2047 "Q" encoding: quoted-printable with `_` standing for a space.
fn decode_q(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut iter = input.iter().copied();

    while let Some(byte) = iter.next() {
        match byte {
            b'_' => out.push(b' '),
            b'=' => {
                let hi = iter.next();
                let lo = iter.next();
                match (hi.and_then(hex_value), lo.and_then(hex_value)) {
                    (Some(hi), Some(lo)) => out.push(hi * 16 + lo),
                    // A malformed escape is emitted literally; dropping it would corrupt
                    // otherwise readable text.
                    _ => {
                        out.push(b'=');
                        out.extend(hi);
                        out.extend(lo);
                    }
                }
            }
            other => out.push(other),
        }
    }

    out
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// RFC 2047 "B" encoding: base64, accepting the sloppy variants seen in practice.
fn decode_b(input: &[u8]) -> Option<Vec<u8>> {
    BASE64_LENIENT
        .decode(input)
        .or_else(|_| BASE64_STANDARD.decode(input))
        .ok()
}

/// Decodes a `quoted-printable` body (RFC 2045 §6.7).
///
/// Three rules matter in practice:
///
/// - `=XX` hex escapes become the octet they name.
/// - A `=` immediately before a line ending is a *soft* line break: the `=` and the line
///   ending both disappear and the two lines join. Whitespace before the `=` is
///   significant and is kept — the encoder was required to escape it as `=20` if it
///   wanted it removed, and stripping it here would delete real spaces between words.
/// - Whitespace immediately before a *hard* line break is removed, per rule 3: an encoder
///   may not leave it there, so anything present was added in transit.
///
/// A malformed escape is emitted literally rather than dropped, on the same reasoning as
/// [`decode_header_value`]: showing `=ZZ` beats showing nothing.
pub fn decode_quoted_printable(bytes: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    // Output written by a `=XX` escape must never be trimmed: `--=20` at the end of a
    // line is the standard way to preserve the trailing space of a signature separator,
    // and trimming it would silently rewrite the article.
    let mut protected = 0usize;

    while let Some(&byte) = bytes.get(index) {
        match byte {
            b'=' => match (bytes.get(index + 1), bytes.get(index + 2)) {
                // Soft line break: "=\r\n", "=\n", or "=" at the end of the input.
                (Some(b'\r'), Some(b'\n')) => index += 3,
                (Some(b'\n'), _) => index += 2,
                (None, _) => index += 1,
                (Some(&hi), Some(&lo)) => match (hex_value(hi), hex_value(lo)) {
                    (Some(hi), Some(lo)) => {
                        out.push(hi * 16 + lo);
                        protected = out.len();
                        index += 3;
                    }
                    _ => {
                        out.push(b'=');
                        index += 1;
                    }
                },
                (Some(&hi), None) => {
                    out.push(b'=');
                    out.push(hi);
                    index += 2;
                }
            },
            // Hard line break: drop literal whitespace the encoder was not allowed to
            // leave there.
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                trim_trailing_blanks(&mut out, protected);
                out.extend_from_slice(b"\r\n");
                protected = out.len();
                index += 2;
            }
            b'\n' => {
                trim_trailing_blanks(&mut out, protected);
                out.push(b'\n');
                protected = out.len();
                index += 1;
            }
            other => {
                out.push(other);
                index += 1;
            }
        }
    }

    out
}

/// Removes literal spaces and tabs from the end of `out`, never reaching below
/// `protected` — the point up to which the output came from an explicit `=XX` escape.
fn trim_trailing_blanks(out: &mut Vec<u8>, protected: usize) {
    while out.len() > protected && matches!(out.last(), Some(b' ' | b'\t')) {
        out.pop();
    }
}

/// Decodes a base64 body, ignoring the line breaks the encoder inserted.
///
/// Returns `None` if the input is not base64 at all, so the caller can fall back to
/// showing the raw text instead of an empty article.
pub fn decode_base64(bytes: &[u8]) -> Option<Vec<u8>> {
    let compact: Vec<u8> = bytes
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    decode_b(&compact)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_header_values_are_left_alone() {
        assert_eq!(encode_header_value("Re: hello"), "Re: hello");
        assert_eq!(encode_header_value(""), "");
    }

    #[test]
    fn encoded_words_round_trip_through_the_decoder() {
        // The pairing that matters: whatever this writes, this crate must be able to read.
        for original in [
            "café and crates",
            "Åsa Lindqvist <asa@example.se>",
            "Olá, tudo bem?",
            "Ré: uma pergunta sobre ação",
            "\u{4e2d}\u{6587}\u{4e3b}\u{9898}",
            // Long enough to be split across several encoded words.
            &"Mensagem com acentuação repetida ".repeat(8),
        ] {
            let encoded = encode_header_value(original);
            assert!(encoded.is_ascii(), "{encoded}");
            assert_eq!(
                decode_header_value(encoded.as_bytes()),
                *original,
                "round trip failed for {original}"
            );
        }
    }

    #[test]
    fn no_encoded_word_exceeds_the_seventy_five_character_limit() {
        // RFC 2047 §2. A word over the limit is the kind of thing a strict server rejects
        // and a lenient one passes on to somebody else's strict reader.
        let long = "Assunto muito comprido com acentuação ".repeat(10);
        for word in encode_header_value(&long).split(' ') {
            assert!(word.len() <= 75, "{} characters: {word}", word.len());
        }
    }

    #[test]
    fn a_split_never_cuts_a_character_in_half() {
        // Four-byte code points, deliberately: a naive split by byte count lands in the
        // middle of one and each half decodes to a replacement character.
        let emoji = "\u{1f600}".repeat(40);
        assert_eq!(
            decode_header_value(encode_header_value(&emoji).as_bytes()),
            emoji
        );
    }

    #[test]
    fn passes_plain_ascii_through() {
        assert_eq!(
            decode_header_value(b"Re: a plain subject"),
            "Re: a plain subject"
        );
    }

    #[test]
    fn decodes_q_encoded_utf8() {
        assert_eq!(decode_header_value(b"=?UTF-8?Q?caf=C3=A9?="), "café");
    }

    #[test]
    fn decodes_underscore_as_space_in_q_encoding() {
        assert_eq!(
            decode_header_value(b"=?UTF-8?Q?hello_world?="),
            "hello world"
        );
    }

    #[test]
    fn decodes_b_encoded_utf8() {
        assert_eq!(decode_header_value(b"=?UTF-8?B?Y2Fmw6k=?="), "café");
    }

    #[test]
    fn decodes_b_encoding_without_padding() {
        assert_eq!(decode_header_value(b"=?UTF-8?B?Y2Fmw6k?="), "café");
    }

    #[test]
    fn decodes_legacy_charsets() {
        // "café" in ISO-8859-1 and in KOI8-R Cyrillic.
        assert_eq!(decode_header_value(b"=?ISO-8859-1?Q?caf=E9?="), "café");
        assert_eq!(
            decode_header_value(b"=?koi8-r?Q?=D0=D2=C9=D7=C5=D4?="),
            "привет"
        );
    }

    #[test]
    fn mixes_literal_text_and_encoded_words() {
        assert_eq!(
            decode_header_value(b"Re: =?UTF-8?Q?caf=C3=A9?= today"),
            "Re: café today"
        );
    }

    #[test]
    fn drops_whitespace_between_adjacent_encoded_words() {
        // RFC 2047 §6.2: the space exists only so the header could be folded.
        assert_eq!(
            decode_header_value(b"=?UTF-8?Q?caf?= =?UTF-8?Q?=C3=A9?="),
            "café"
        );
    }

    #[test]
    fn keeps_whitespace_that_is_not_between_encoded_words() {
        assert_eq!(
            decode_header_value(b"a =?UTF-8?Q?b?= c =?UTF-8?Q?d?= e"),
            "a b c d e"
        );
    }

    #[test]
    fn passes_malformed_encoded_words_through_literally() {
        for input in [
            &b"=?UTF-8?Q?unterminated"[..],
            b"=?UTF-8?X?unknown-encoding?=",
            b"=??Q?empty-charset?=",
            b"=?UTF-8?Q?has space?=",
            b"=?",
            b"=?UTF-8?",
        ] {
            let decoded = decode_header_value(input);
            assert_eq!(
                decoded,
                String::from_utf8_lossy(input),
                "input {:?} should pass through",
                String::from_utf8_lossy(input)
            );
        }
    }

    #[test]
    fn unknown_charset_falls_back_instead_of_failing() {
        // The bytes are still shown; only the charset guess is wrong.
        assert_eq!(decode_header_value(b"=?x-nonesuch?Q?abc?="), "abc");
    }

    #[test]
    fn finds_an_encoded_word_after_a_false_start() {
        assert_eq!(decode_header_value(b"=?=?UTF-8?Q?x?="), "=?x");
    }

    #[test]
    fn decodes_two_encoded_words_in_a_row_without_separator() {
        assert_eq!(decode_header_value(b"=?UTF-8?Q?a?==?UTF-8?Q?b?="), "ab");
    }

    #[test]
    fn malformed_q_escape_is_kept_literally() {
        assert_eq!(decode_header_value(b"=?UTF-8?Q?a=ZZb?="), "a=ZZb");
        assert_eq!(decode_header_value(b"=?UTF-8?Q?trailing=?="), "trailing=");
    }

    #[test]
    fn eight_bit_bytes_fall_back_to_windows_1252() {
        // A raw 0xE9 is not valid UTF-8; Windows-1252 reads it as "é".
        assert_eq!(decode_8bit_lossy(b"caf\xe9"), "café");
        assert_eq!(decode_8bit_lossy("café".as_bytes()), "café");
        assert_eq!(decode_header_value(b"caf\xe9"), "café");
    }

    #[test]
    fn charset_labels_tolerate_quotes_and_language_suffixes() {
        assert_eq!(decode_with_charset(b"caf\xe9", "\"ISO-8859-1\""), "café");
        assert_eq!(decode_with_charset(b"caf\xe9", "ISO-8859-1*en"), "café");
        assert_eq!(decode_with_charset(b"caf\xe9", "nonesuch"), "café");
        assert_eq!(decode_with_charset("café".as_bytes(), "utf-8"), "café");
    }

    #[test]
    fn decodes_quoted_printable_escapes() {
        assert_eq!(decode_quoted_printable(b"caf=C3=A9"), "café".as_bytes());
        assert_eq!(decode_quoted_printable(b"plain text"), b"plain text");
        // Unlike RFC 2047 "Q", an underscore is literal in a body.
        assert_eq!(decode_quoted_printable(b"a_b"), b"a_b");
    }

    #[test]
    fn joins_quoted_printable_soft_line_breaks() {
        assert_eq!(
            decode_quoted_printable(b"a very long li=\r\nne"),
            b"a very long line"
        );
        assert_eq!(decode_quoted_printable(b"a=\nb"), b"ab");
        assert_eq!(decode_quoted_printable(b"trailing="), b"trailing");
    }

    #[test]
    fn keeps_whitespace_before_a_soft_line_break() {
        // The space belongs to the text. An encoder that wanted it gone had to write
        // "=20"; stripping it here would join words that were separated, turning
        // "this to =\r\nsay" into "this tosay".
        assert_eq!(decode_quoted_printable(b"a b =\r\nc"), b"a b c");
        assert_eq!(
            decode_quoted_printable(b"I have this to =\r\nsay."),
            b"I have this to say."
        );
    }

    #[test]
    fn strips_whitespace_before_a_hard_line_break() {
        // RFC 2045 §6.7 rule 3: an encoder may not leave trailing whitespace on a line,
        // so whatever is there was added in transit.
        assert_eq!(decode_quoted_printable(b"a   \r\nb"), b"a\r\nb");
        assert_eq!(decode_quoted_printable(b"a\t\nb"), b"a\nb");
        // Encoded trailing whitespace is deliberate and survives: this is how a "-- "
        // signature separator reaches the reader intact.
        assert_eq!(decode_quoted_printable(b"--=20\r\nx"), b"-- \r\nx");
    }

    #[test]
    fn keeps_hard_line_breaks_in_quoted_printable() {
        assert_eq!(decode_quoted_printable(b"a\r\nb"), b"a\r\nb");
    }

    #[test]
    fn malformed_quoted_printable_escapes_survive() {
        assert_eq!(decode_quoted_printable(b"a=ZZb"), b"a=ZZb");
        assert_eq!(decode_quoted_printable(b"100=%"), b"100=%");
        assert_eq!(decode_quoted_printable(b"a=C"), b"a=C");
    }

    #[test]
    fn decodes_base64_across_line_breaks() {
        assert_eq!(
            decode_base64(b"Y2Fm\r\nw6k=").as_deref(),
            Some("café".as_bytes())
        );
        assert_eq!(decode_base64(b"").as_deref(), Some(&b""[..]));
        assert!(decode_base64(b"not valid base64!!").is_none());
    }

    #[test]
    fn empty_input_decodes_to_empty_string() {
        assert!(decode_header_value(b"").is_empty());
        assert!(decode_8bit_lossy(b"").is_empty());
    }
}
