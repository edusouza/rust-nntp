//! The `LIST` family of responses (RFC 3977 §7.6).
//!
//! `LIST ACTIVE` on a full-feed server returns well over a hundred thousand lines, a
//! handful of which are usually malformed — a group name with a space in it, a missing
//! status field, a truncated line. One bad line must not lose the other hundred thousand,
//! so every parser here works line by line and [`ListResult`] keeps the lines it could not
//! use instead of failing.

use chrono::{DateTime, Utc};

use crate::block::DataBlock;
use crate::group::{GroupName, PostingStatus};
use crate::mime::decode_8bit_lossy;
use crate::{ProtoError, Result};

/// The outcome of parsing a `LIST` block: what was understood, and what was not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListResult<T> {
    /// The entries that parsed.
    pub entries: Vec<T>,
    /// Lines that did not parse, decoded for logging.
    ///
    /// Anything here is worth reporting: it is either a broken server or a broken parser.
    pub skipped: Vec<String>,
}

impl<T> ListResult<T> {
    /// Parses every line of `block` with `parse_line`, collecting failures separately.
    fn parse_block(block: &DataBlock, parse_line: impl Fn(&[u8]) -> Result<T>) -> Self {
        let mut entries = Vec::new();
        let mut skipped = Vec::new();

        for line in block.lines() {
            if line.is_empty() {
                continue;
            }
            match parse_line(line) {
                Ok(entry) => entries.push(entry),
                Err(_) => skipped.push(decode_8bit_lossy(line)),
            }
        }

        Self { entries, skipped }
    }

    /// How many entries parsed.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing parsed.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// One line of `LIST ACTIVE`: `group high low status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveEntry {
    /// The group name.
    pub name: GroupName,
    /// The highest article number in the group.
    pub high: u64,
    /// The lowest article number in the group.
    pub low: u64,
    /// Whether the group accepts postings.
    pub status: PostingStatus,
}

impl ActiveEntry {
    /// Parses one line.
    ///
    /// Note the field order: `LIST ACTIVE` gives `high` *before* `low`, the opposite of
    /// the `GROUP` response. Swapping them is an easy and near-invisible bug, which is
    /// why it has a test of its own.
    ///
    /// # Errors
    ///
    /// Returns [`ProtoError::MissingField`] if a field is absent,
    /// [`ProtoError::InvalidNumber`] if a watermark is not a number, and
    /// [`ProtoError::InvalidGroupName`] if the name is not usable.
    pub fn parse(line: &[u8]) -> Result<Self> {
        const CONTEXT: &str = "LIST ACTIVE line";
        let text = decode_8bit_lossy(line);
        let mut fields = text.split_ascii_whitespace();

        let name = GroupName::parse(next_field(&mut fields, CONTEXT, "group")?)?;
        let high = parse_u64(next_field(&mut fields, CONTEXT, "high")?, CONTEXT, "high")?;
        let low = parse_u64(next_field(&mut fields, CONTEXT, "low")?, CONTEXT, "low")?;
        let status = PostingStatus::parse(next_field(&mut fields, CONTEXT, "status")?);

        Ok(Self {
            name,
            high,
            low,
            status,
        })
    }

    /// Parses a whole `LIST ACTIVE` block.
    pub fn parse_block(block: &DataBlock) -> ListResult<Self> {
        ListResult::parse_block(block, Self::parse)
    }

    /// The number of articles the watermarks imply.
    ///
    /// An upper bound only: expiry and cancellation leave gaps.
    pub const fn estimated_count(&self) -> u64 {
        if self.high >= self.low && self.high > 0 {
            self.high - self.low + 1
        } else {
            0
        }
    }

    /// Whether the watermarks say the group is empty.
    pub const fn is_empty(&self) -> bool {
        self.estimated_count() == 0
    }
}

/// One line of `LIST NEWSGROUPS`: `group description`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewsgroupEntry {
    /// The group name.
    pub name: GroupName,
    /// The human-readable description, which may be empty.
    pub description: String,
}

impl NewsgroupEntry {
    /// Parses one line.
    ///
    /// The separator is nominally a tab but plenty of servers use spaces, so any run of
    /// whitespace ends the name. Descriptions are frequently 8-bit and unlabelled, so they
    /// are decoded with the Windows-1252 fallback.
    ///
    /// # Errors
    ///
    /// Returns [`ProtoError::MissingField`] if the line is blank and
    /// [`ProtoError::InvalidGroupName`] if the first field is not a usable group name.
    pub fn parse(line: &[u8]) -> Result<Self> {
        const CONTEXT: &str = "LIST NEWSGROUPS line";
        let text = decode_8bit_lossy(line);
        let trimmed = text.trim_start();

        let name_len = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
        let raw_name = trimmed.get(..name_len).unwrap_or_default();
        if raw_name.is_empty() {
            return Err(ProtoError::MissingField {
                context: CONTEXT,
                field: "group",
            });
        }

        let description = trimmed
            .get(name_len..)
            .unwrap_or_default()
            .trim()
            .to_owned();

        Ok(Self {
            name: GroupName::parse(raw_name)?,
            description,
        })
    }

    /// Parses a whole `LIST NEWSGROUPS` block.
    pub fn parse_block(block: &DataBlock) -> ListResult<Self> {
        ListResult::parse_block(block, Self::parse)
    }
}

/// One line of `LIST ACTIVE.TIMES`: `group created-seconds creator`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveTimesEntry {
    /// The group name.
    pub name: GroupName,
    /// When the group was created, or `None` if the timestamp was out of range.
    pub created: Option<DateTime<Utc>>,
    /// Who created it, usually an email address. May be empty.
    pub creator: String,
}

impl ActiveTimesEntry {
    /// Parses one line.
    ///
    /// # Errors
    ///
    /// Returns [`ProtoError::MissingField`], [`ProtoError::InvalidNumber`] or
    /// [`ProtoError::InvalidGroupName`] as for the other `LIST` variants.
    pub fn parse(line: &[u8]) -> Result<Self> {
        const CONTEXT: &str = "LIST ACTIVE.TIMES line";
        let text = decode_8bit_lossy(line);
        let mut fields = text.split_ascii_whitespace();

        let name = GroupName::parse(next_field(&mut fields, CONTEXT, "group")?)?;
        let seconds = parse_u64(
            next_field(&mut fields, CONTEXT, "created")?,
            CONTEXT,
            "created",
        )?;
        let creator = fields.collect::<Vec<_>>().join(" ");

        Ok(Self {
            name,
            // Timestamps beyond the representable range are treated as unknown rather
            // than as a parse failure; the group itself is still usable.
            created: i64::try_from(seconds)
                .ok()
                .and_then(|secs| DateTime::from_timestamp(secs, 0)),
            creator,
        })
    }

    /// Parses a whole `LIST ACTIVE.TIMES` block.
    pub fn parse_block(block: &DataBlock) -> ListResult<Self> {
        ListResult::parse_block(block, Self::parse)
    }
}

fn next_field<'a>(
    fields: &mut impl Iterator<Item = &'a str>,
    context: &'static str,
    field: &'static str,
) -> Result<&'a str> {
    fields
        .next()
        .ok_or(ProtoError::MissingField { context, field })
}

fn parse_u64(raw: &str, context: &'static str, field: &'static str) -> Result<u64> {
    raw.parse().map_err(|_| ProtoError::InvalidNumber {
        context,
        value: format!("{field}={raw}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_an_active_line_with_high_before_low() {
        // RFC 3977 §7.6.3: the order is `group high low status`, which is the reverse of
        // the GROUP response. Getting this backwards silently shows wrong article counts.
        let entry = ActiveEntry::parse(b"misc.test 3002322 3000234 y").unwrap();
        assert_eq!(entry.name.as_str(), "misc.test");
        assert_eq!(entry.high, 3_002_322);
        assert_eq!(entry.low, 3_000_234);
        assert_eq!(entry.status, PostingStatus::Permitted);
        assert_eq!(entry.estimated_count(), 2089);
    }

    #[test]
    fn parses_the_moderated_and_alias_status_flags() {
        assert_eq!(
            ActiveEntry::parse(b"comp.lang.rust 10 1 m").unwrap().status,
            PostingStatus::Moderated
        );
        assert_eq!(
            ActiveEntry::parse(b"old.group 0 1 =new.group")
                .unwrap()
                .status,
            PostingStatus::AliasFor("new.group".to_owned())
        );
    }

    #[test]
    fn treats_an_empty_group_as_zero_articles() {
        let entry = ActiveEntry::parse(b"empty.group 0 1 y").unwrap();
        assert!(entry.is_empty());
        assert_eq!(entry.estimated_count(), 0);
        assert!(ActiveEntry::parse(b"empty.group 0 0 y").unwrap().is_empty());
    }

    #[test]
    fn tolerates_extra_whitespace_in_an_active_line() {
        let entry = ActiveEntry::parse(b"  misc.test\t10   1  y  ").unwrap();
        assert_eq!(entry.high, 10);
    }

    #[test]
    fn rejects_malformed_active_lines() {
        for bad in [
            &b"misc.test 10 1"[..],
            b"misc.test",
            b"misc.test ten 1 y",
            b"misc.test 10 one y",
            b"comp.* 10 1 y",
            b"",
        ] {
            assert!(
                ActiveEntry::parse(bad).is_err(),
                "expected {:?} rejected",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn one_bad_line_does_not_lose_the_block() {
        let block = DataBlock::parse(
            b"misc.test 10 1 y\r\n\
              this line is broken\r\n\
              comp.lang.rust 5 1 y\r\n\
              .\r\n",
        );
        let result = ActiveEntry::parse_block(&block);
        assert_eq!(result.len(), 2);
        assert_eq!(result.skipped, ["this line is broken"]);
        assert!(!result.is_empty());
    }

    #[test]
    fn parses_newsgroup_descriptions_separated_by_a_tab() {
        let entry = NewsgroupEntry::parse(b"misc.test\tFor testing purposes only").unwrap();
        assert_eq!(entry.name.as_str(), "misc.test");
        assert_eq!(entry.description, "For testing purposes only");
    }

    #[test]
    fn parses_newsgroup_descriptions_separated_by_spaces() {
        // Not what the RFC says, but what many servers send.
        let entry = NewsgroupEntry::parse(b"misc.test   For testing").unwrap();
        assert_eq!(entry.description, "For testing");
    }

    #[test]
    fn accepts_a_group_with_no_description() {
        let entry = NewsgroupEntry::parse(b"misc.test").unwrap();
        assert!(entry.description.is_empty());
        let padded = NewsgroupEntry::parse(b"misc.test\t").unwrap();
        assert!(padded.description.is_empty());
    }

    #[test]
    fn decodes_eight_bit_descriptions() {
        let entry = NewsgroupEntry::parse(b"de.test\tCaf\xe9 und Kuchen").unwrap();
        assert_eq!(entry.description, "Café und Kuchen");
    }

    #[test]
    fn rejects_a_blank_newsgroups_line() {
        assert!(NewsgroupEntry::parse(b"   ").is_err());
    }

    #[test]
    fn parses_active_times() {
        let entry = ActiveTimesEntry::parse(b"misc.test 930445408 <tale@isc.org>").unwrap();
        assert_eq!(entry.name.as_str(), "misc.test");
        assert_eq!(entry.creator, "<tale@isc.org>");
        assert_eq!(
            entry.created.map(|dt| dt.to_rfc3339()),
            Some("1999-06-27T01:03:28+00:00".to_owned())
        );
    }

    #[test]
    fn active_times_tolerates_a_missing_creator_and_an_absurd_timestamp() {
        let no_creator = ActiveTimesEntry::parse(b"misc.test 0").unwrap();
        assert!(no_creator.creator.is_empty());
        assert!(no_creator.created.is_some());

        let absurd = ActiveTimesEntry::parse(b"misc.test 99999999999999999 x").unwrap();
        assert!(absurd.created.is_none());
        assert_eq!(absurd.name.as_str(), "misc.test");
    }

    #[test]
    fn parses_a_newsgroups_block() {
        let block = DataBlock::parse(b"a.b\tfirst\r\n\r\nc.d\tsecond\r\n.\r\n");
        let result = NewsgroupEntry::parse_block(&block);
        assert_eq!(result.len(), 2);
        assert!(result.skipped.is_empty());
    }
}
