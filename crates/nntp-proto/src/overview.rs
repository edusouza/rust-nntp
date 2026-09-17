//! Overview records: the `OVER` and `XOVER` responses (RFC 3977 §8.3).
//!
//! Overview is what makes a news reader usable: one round trip returns the subject,
//! author, date and size of a whole range of articles, so the article list can be drawn
//! without fetching any article.
//!
//! The wire format is tab-separated, and the first seven fields after the article number
//! are fixed by the RFC. Anything after that is described by `LIST OVERVIEW.FMT`, which is
//! why [`OverviewFmt`] exists — without it the extra fields cannot be named, and a server
//! that appends `Xref:full` would look like it was sending a corrupt `:lines` value.

use chrono::{DateTime, FixedOffset};

use crate::block::DataBlock;
use crate::date::parse_date;
use crate::list::ListResult;
use crate::message_id::MessageId;
use crate::mime::{decode_8bit_lossy, decode_header_value};
use crate::{ProtoError, Result};

/// The seven fields every `OVER` response starts with, after the article number.
pub const STANDARD_FIELDS: [&str; 7] = [
    "Subject:",
    "From:",
    "Date:",
    "Message-ID:",
    "References:",
    ":bytes",
    ":lines",
];

/// One field described by `LIST OVERVIEW.FMT`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverviewField {
    /// The field name without its trailing colon or `:full` suffix: `Subject`, `bytes`.
    pub name: String,
    /// Whether the value on the wire includes the header name and colon.
    ///
    /// Advertised as `Xref:full`. The prefix must be stripped before the value is used.
    pub full: bool,
    /// Whether this is server metadata (`:bytes`, `:lines`) rather than a header field.
    pub metadata: bool,
}

impl OverviewField {
    /// Parses one line of `LIST OVERVIEW.FMT`.
    fn parse(line: &str) -> Option<Self> {
        let text = line.trim();
        if text.is_empty() {
            return None;
        }

        if let Some(name) = text.strip_prefix(':') {
            return Some(Self {
                name: name.to_ascii_lowercase(),
                full: false,
                metadata: true,
            });
        }

        let (name, full) = match text.strip_suffix(":full") {
            Some(name) => (name, true),
            None => (text.strip_suffix(':').unwrap_or(text), false),
        };

        Some(Self {
            name: name.to_owned(),
            full,
            metadata: false,
        })
    }

    /// Whether this field is the given standard field, compared case-insensitively.
    fn is(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }
}

/// The field layout of `OVER` responses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverviewFmt {
    fields: Vec<OverviewField>,
}

impl Default for OverviewFmt {
    fn default() -> Self {
        Self::standard()
    }
}

impl OverviewFmt {
    /// The layout every server must support: the seven RFC 3977 fields.
    ///
    /// Used when `LIST OVERVIEW.FMT` is unavailable or unparseable. Extra fields sent by
    /// such a server are still returned, under positional names.
    pub fn standard() -> Self {
        Self {
            fields: STANDARD_FIELDS
                .iter()
                .filter_map(|name| OverviewField::parse(name))
                .collect(),
        }
    }

    /// Parses a `LIST OVERVIEW.FMT` data block.
    ///
    /// Some servers list the article number as a first `:number` field and some do not;
    /// the number is always present on the wire, so a leading number field is dropped to
    /// keep the indices aligned with the fields that follow it.
    pub fn parse(block: &DataBlock) -> Self {
        let mut fields: Vec<OverviewField> = block
            .lines()
            .iter()
            .filter_map(|line| OverviewField::parse(&decode_8bit_lossy(line)))
            .collect();

        if fields
            .first()
            .is_some_and(|field| field.is("number") || field.is("article"))
        {
            fields.remove(0);
        }

        if fields.is_empty() {
            return Self::standard();
        }

        Self { fields }
    }

    /// The fields, in wire order, excluding the leading article number.
    pub fn fields(&self) -> &[OverviewField] {
        &self.fields
    }

    /// Whether the layout begins with the seven RFC 3977 fields in the required order.
    ///
    /// A server that fails this is not necessarily broken, but the mapping of its fields
    /// relies entirely on the names it reported, so it is worth logging.
    pub fn is_standard_prefix(&self) -> bool {
        STANDARD_FIELDS
            .iter()
            .filter_map(|name| OverviewField::parse(name))
            .enumerate()
            .all(|(index, expected)| {
                self.fields
                    .get(index)
                    .is_some_and(|actual| actual.is(&expected.name))
            })
    }

    /// The name to use for the field at `index`, falling back to the standard order.
    fn name_at(&self, index: usize) -> Option<&OverviewField> {
        self.fields.get(index)
    }
}

/// One article's overview data.
///
/// The raw `Date` text is kept alongside the parsed value so that an unparseable date
/// still shows something. Subject and author are decoded eagerly, because every use of
/// them is for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverviewRecord {
    /// The article number within the selected group.
    pub number: u64,
    /// The subject, with RFC 2047 encoded words decoded.
    pub subject: String,
    /// The author, with RFC 2047 encoded words decoded.
    pub from: String,
    /// The `Date` header exactly as the server sent it.
    pub date_raw: String,
    /// The parsed date, or `None` if it could not be interpreted.
    pub date: Option<DateTime<FixedOffset>>,
    /// The message-id, or `None` if absent or malformed.
    pub message_id: Option<MessageId>,
    /// The articles this one replies to, oldest first.
    pub references: Vec<MessageId>,
    /// The article's size in octets, as reported by the server.
    pub bytes: Option<u64>,
    /// The article's length in lines, as reported by the server.
    pub lines: Option<u64>,
    /// Any further fields, named from `LIST OVERVIEW.FMT`.
    pub extra: Vec<(String, String)>,
}

impl OverviewRecord {
    /// Parses one overview line.
    ///
    /// # Errors
    ///
    /// Returns [`ProtoError::MissingField`] if the line is empty and
    /// [`ProtoError::InvalidNumber`] if the leading article number is not a number.
    /// Everything after the article number is optional: a missing or malformed field
    /// becomes `None` or an empty string rather than an error, because a single bad field
    /// must not hide the article.
    pub fn parse(line: &[u8], fmt: &OverviewFmt) -> Result<Self> {
        const CONTEXT: &str = "OVER line";

        let mut parts = line.split(|byte| *byte == b'\t');
        let raw_number = parts.next().ok_or(ProtoError::MissingField {
            context: CONTEXT,
            field: "article number",
        })?;
        let number = decode_8bit_lossy(raw_number).trim().parse().map_err(|_| {
            ProtoError::InvalidNumber {
                context: CONTEXT,
                value: decode_8bit_lossy(raw_number),
            }
        })?;

        let mut record = Self {
            number,
            subject: String::new(),
            from: String::new(),
            date_raw: String::new(),
            date: None,
            message_id: None,
            references: Vec::new(),
            bytes: None,
            lines: None,
            extra: Vec::new(),
        };

        for (index, raw) in parts.enumerate() {
            let field = fmt.name_at(index);
            let raw = match field {
                Some(field) if field.full => strip_full_prefix(raw, &field.name),
                _ => raw,
            };

            match field {
                Some(field) if field.is("subject") => {
                    record.subject = decode_header_value(raw).trim().to_owned();
                }
                Some(field) if field.is("from") => {
                    record.from = decode_header_value(raw).trim().to_owned();
                }
                Some(field) if field.is("date") => {
                    record.date_raw = decode_8bit_lossy(raw).trim().to_owned();
                    record.date = parse_date(&record.date_raw).ok();
                }
                Some(field) if field.is("message-id") => {
                    record.message_id = MessageId::parse(&decode_8bit_lossy(raw)).ok();
                }
                Some(field) if field.is("references") => {
                    record.references = MessageId::parse_list(&decode_8bit_lossy(raw));
                }
                Some(field) if field.is("bytes") => record.bytes = parse_count(raw),
                Some(field) if field.is("lines") => record.lines = parse_count(raw),
                Some(field) => record.extra.push((
                    field.name.clone(),
                    decode_header_value(raw).trim().to_owned(),
                )),
                // Beyond what OVERVIEW.FMT described. Keep it under a positional name
                // rather than dropping data we cannot label.
                None => record.extra.push((
                    format!("field-{}", index + 1),
                    decode_header_value(raw).trim().to_owned(),
                )),
            }
        }

        Ok(record)
    }

    /// Parses a whole `OVER` or `XOVER` data block.
    pub fn parse_block(block: &DataBlock, fmt: &OverviewFmt) -> ListResult<Self> {
        let mut entries = Vec::new();
        let mut skipped = Vec::new();

        for line in block.lines() {
            if line.is_empty() {
                continue;
            }
            match Self::parse(line, fmt) {
                Ok(record) => entries.push(record),
                Err(_) => skipped.push(decode_8bit_lossy(line)),
            }
        }

        ListResult { entries, skipped }
    }

    /// The value of an extra field, by name, compared case-insensitively.
    pub fn extra(&self, name: &str) -> Option<&str> {
        self.extra
            .iter()
            .find(|(field, _)| field.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Whether this article is a reply.
    pub fn is_reply(&self) -> bool {
        !self.references.is_empty()
    }

    /// The message-id of the article this one directly replies to.
    pub fn parent(&self) -> Option<&MessageId> {
        self.references.last()
    }
}

/// Removes a `Name: ` prefix from a `:full` overview field.
fn strip_full_prefix<'a>(raw: &'a [u8], name: &str) -> &'a [u8] {
    let prefix_len = name.len() + 1;
    match raw.get(..prefix_len) {
        Some(head)
            if head
                .strip_suffix(b":")
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(name.as_bytes())) =>
        {
            let rest = raw.get(prefix_len..).unwrap_or_default();
            match rest.split_first() {
                Some((b' ' | b'\t', tail)) => tail,
                _ => rest,
            }
        }
        _ => raw,
    }
}

fn parse_count(raw: &[u8]) -> Option<u64> {
    decode_8bit_lossy(raw).trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: &[u8] = b"3000234\tI am just a test article\t\"Demo User\" <nobody@example.net>\t6 Oct 1998 04:38:40 -0500\t<45223423@example.com>\t<45454@example.net>\t1234\t17\tXref: news.example.com misc.test:3000363";

    fn standard() -> OverviewFmt {
        OverviewFmt::standard()
    }

    #[test]
    fn parses_the_rfc_example_line() {
        // RFC 3977 §8.3.3.
        let record = OverviewRecord::parse(LINE, &standard()).unwrap();
        assert_eq!(record.number, 3_000_234);
        assert_eq!(record.subject, "I am just a test article");
        assert_eq!(record.from, "\"Demo User\" <nobody@example.net>");
        assert_eq!(record.date_raw, "6 Oct 1998 04:38:40 -0500");
        assert_eq!(
            record.date.map(|d| d.to_rfc3339()),
            Some("1998-10-06T04:38:40-05:00".to_owned())
        );
        assert_eq!(
            record.message_id.as_ref().map(MessageId::as_str),
            Some("<45223423@example.com>")
        );
        assert_eq!(record.bytes, Some(1234));
        assert_eq!(record.lines, Some(17));
        assert!(record.is_reply());
        assert_eq!(
            record.parent().map(MessageId::as_str),
            Some("<45454@example.net>")
        );
        // With the standard format the ninth field has no name.
        assert_eq!(
            record.extra("field-8"),
            Some("Xref: news.example.com misc.test:3000363")
        );
    }

    #[test]
    fn names_extra_fields_from_overview_fmt() {
        let fmt = OverviewFmt::parse(&DataBlock::parse(
            b"Subject:\r\nFrom:\r\nDate:\r\nMessage-ID:\r\nReferences:\r\n:bytes\r\n:lines\r\nXref:full\r\n.\r\n",
        ));
        assert!(fmt.is_standard_prefix());

        let record = OverviewRecord::parse(LINE, &fmt).unwrap();
        // `Xref:full` means the value repeats the header name, which must be stripped.
        assert_eq!(
            record.extra("Xref"),
            Some("news.example.com misc.test:3000363")
        );
    }

    #[test]
    fn drops_a_leading_number_field_from_overview_fmt() {
        // Some servers list the article number; it is always on the wire regardless, so
        // keeping it would shift every following field by one.
        let fmt = OverviewFmt::parse(&DataBlock::parse(
            b":number\r\nSubject:\r\nFrom:\r\nDate:\r\nMessage-ID:\r\nReferences:\r\n:bytes\r\n:lines\r\n.\r\n",
        ));
        let record = OverviewRecord::parse(LINE, &fmt).unwrap();
        assert_eq!(record.subject, "I am just a test article");
        assert_eq!(record.bytes, Some(1234));
    }

    #[test]
    fn falls_back_to_the_standard_layout_for_an_empty_format_block() {
        let fmt = OverviewFmt::parse(&DataBlock::parse(b".\r\n"));
        assert_eq!(fmt, OverviewFmt::standard());
        assert!(fmt.is_standard_prefix());
    }

    #[test]
    fn maps_fields_by_name_when_a_server_reorders_them() {
        let fmt = OverviewFmt::parse(&DataBlock::parse(
            b"From:\r\nSubject:\r\n:lines\r\n:bytes\r\n.\r\n",
        ));
        assert!(!fmt.is_standard_prefix());

        let record = OverviewRecord::parse(b"7\tauthor\ttopic\t3\t99", &fmt).unwrap();
        assert_eq!(record.from, "author");
        assert_eq!(record.subject, "topic");
        assert_eq!(record.lines, Some(3));
        assert_eq!(record.bytes, Some(99));
    }

    #[test]
    fn decodes_encoded_words_in_subject_and_author() {
        let line = b"5\t=?UTF-8?Q?caf=C3=A9?=\t=?ISO-8859-1?Q?Bj=F8rn?= <b@x>\t\t\t\t\t";
        let record = OverviewRecord::parse(line, &standard()).unwrap();
        assert_eq!(record.subject, "café");
        assert_eq!(record.from, "Bjørn <b@x>");
    }

    #[test]
    fn empty_fields_become_none_rather_than_errors() {
        let record = OverviewRecord::parse(b"9\t\t\t\t\t\t\t", &standard()).unwrap();
        assert_eq!(record.number, 9);
        assert!(record.subject.is_empty());
        assert!(record.date.is_none());
        assert!(record.date_raw.is_empty());
        assert!(record.message_id.is_none());
        assert!(record.references.is_empty());
        assert_eq!(record.bytes, None);
        assert_eq!(record.lines, None);
        assert!(!record.is_reply());
    }

    #[test]
    fn a_truncated_line_keeps_the_fields_it_has() {
        let record = OverviewRecord::parse(b"9\tsubject only", &standard()).unwrap();
        assert_eq!(record.subject, "subject only");
        assert!(record.from.is_empty());
        assert_eq!(record.bytes, None);
    }

    #[test]
    fn a_malformed_date_keeps_the_raw_text() {
        let line = b"9\ts\tf\tnot a date\t<a@b>\t\t1\t2";
        let record = OverviewRecord::parse(line, &standard()).unwrap();
        assert_eq!(record.date_raw, "not a date");
        assert!(record.date.is_none());
    }

    #[test]
    fn a_malformed_message_id_does_not_fail_the_record() {
        let line = b"9\ts\tf\t\tnot-an-id\t\t1\t2";
        let record = OverviewRecord::parse(line, &standard()).unwrap();
        assert!(record.message_id.is_none());
        assert_eq!(record.number, 9);
    }

    #[test]
    fn a_non_numeric_byte_count_becomes_none() {
        let line = b"9\ts\tf\t\t<a@b>\t\tlots\tsome";
        let record = OverviewRecord::parse(line, &standard()).unwrap();
        assert_eq!(record.bytes, None);
        assert_eq!(record.lines, None);
    }

    #[test]
    fn rejects_a_line_without_a_usable_article_number() {
        assert!(matches!(
            OverviewRecord::parse(b"not-a-number\tsubject", &standard()),
            Err(ProtoError::InvalidNumber { .. })
        ));
        assert!(OverviewRecord::parse(b"", &standard()).is_err());
    }

    #[test]
    fn one_bad_line_does_not_lose_the_block() {
        let block = DataBlock::from_lines(vec![
            b"1\tfirst\t\t\t\t\t\t".to_vec(),
            b"broken\tline".to_vec(),
            b"2\tsecond\t\t\t\t\t\t".to_vec(),
        ]);
        let result = OverviewRecord::parse_block(&block, &standard());
        assert_eq!(result.len(), 2);
        assert_eq!(result.skipped.len(), 1);
    }

    #[test]
    fn strips_the_full_prefix_case_insensitively_and_without_a_space() {
        assert_eq!(strip_full_prefix(b"Xref: value", "Xref"), b"value");
        assert_eq!(strip_full_prefix(b"xref:value", "Xref"), b"value");
        assert_eq!(strip_full_prefix(b"XREF:\tvalue", "Xref"), b"value");
        // Not the advertised field: leave it alone.
        assert_eq!(strip_full_prefix(b"Other: value", "Xref"), b"Other: value");
        assert_eq!(strip_full_prefix(b"", "Xref"), b"");
        assert_eq!(strip_full_prefix(b"Xref:", "Xref"), b"");
    }

    #[test]
    fn parses_overview_fmt_field_shapes() {
        assert_eq!(
            OverviewField::parse("Subject:"),
            Some(OverviewField {
                name: "Subject".to_owned(),
                full: false,
                metadata: false
            })
        );
        assert_eq!(
            OverviewField::parse("Xref:full"),
            Some(OverviewField {
                name: "Xref".to_owned(),
                full: true,
                metadata: false
            })
        );
        assert_eq!(
            OverviewField::parse(":bytes"),
            Some(OverviewField {
                name: "bytes".to_owned(),
                full: false,
                metadata: true
            })
        );
        // A name without the trailing colon is accepted; servers omit it.
        assert_eq!(
            OverviewField::parse("Subject").map(|f| f.name),
            Some("Subject".to_owned())
        );
        assert_eq!(OverviewField::parse("   "), None);
    }
}
