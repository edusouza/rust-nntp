//! Newsgroup names and the `GROUP` response.

use crate::response::StatusLine;
use crate::{ProtoError, Result};

/// The longest group name accepted, chosen so that any single-argument command built from
/// one still fits inside the 512-octet command line limit of RFC 3977 §3.1.
pub const MAX_GROUP_NAME_LEN: usize = 480;

/// A newsgroup name, for example `comp.lang.rust`.
///
/// Validated at construction so that it is always safe to write into a command line.
/// Wildmat metacharacters (`*`, `?`, `,`, `!`) are rejected: a pattern is a different kind
/// of argument and is carried by [`crate::command::Wildmat`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GroupName(String);

impl GroupName {
    /// Validates and wraps a group name.
    ///
    /// # Errors
    ///
    /// Returns [`ProtoError::InvalidGroupName`] if the name is empty, longer than
    /// [`MAX_GROUP_NAME_LEN`], contains a byte outside printable US-ASCII, or contains a
    /// wildmat metacharacter.
    pub fn parse(value: &str) -> Result<Self> {
        let invalid = || ProtoError::InvalidGroupName(value.to_owned());

        if value.is_empty() || value.len() > MAX_GROUP_NAME_LEN {
            return Err(invalid());
        }
        if !value
            .bytes()
            .all(|b| b.is_ascii_graphic() && !matches!(b, b'*' | b'?' | b',' | b'!' | b'\\'))
        {
            return Err(invalid());
        }

        Ok(Self(value.to_owned()))
    }

    /// The name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The dot-separated components of the name.
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('.')
    }

    /// The first component, conventionally the hierarchy: `comp` for `comp.lang.rust`.
    pub fn hierarchy(&self) -> &str {
        self.0.split('.').next().unwrap_or(&self.0)
    }
}

impl core::fmt::Display for GroupName {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // `pad` rather than `write_str`, so that `{:<40}` in a caller's format string
        // actually pads. A manual `Display` that ignores the width silently breaks every
        // aligned column a caller tries to build.
        f.pad(&self.0)
    }
}

impl core::str::FromStr for GroupName {
    type Err = ProtoError;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

/// The estimated article counts returned when a group is selected.
///
/// RFC 3977 §6.1.1.2: `211 number low high group`. `number` is an *estimate* of the
/// article count, and the RFC explicitly allows it to be wrong; `low` and `high` are the
/// watermarks. An empty group is reported either as `0 0 0` or with `low` one greater than
/// `high`, so [`Self::is_empty`] checks both forms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupSummary {
    /// The group that is now selected.
    pub name: GroupName,
    /// The server's estimate of how many articles the group holds.
    pub estimated_count: u64,
    /// The lowest article number present, or `0` for an empty group.
    pub low: u64,
    /// The highest article number present, or `0` for an empty group.
    pub high: u64,
}

impl GroupSummary {
    /// Parses a `211` status line.
    ///
    /// The group name is the fourth argument. Some old servers omit it, in which case the
    /// caller's requested name is used via `fallback_name`.
    ///
    /// # Errors
    ///
    /// Returns [`ProtoError::MissingField`] or [`ProtoError::InvalidNumber`] if the
    /// numeric watermarks are absent or unparseable, and [`ProtoError::InvalidGroupName`]
    /// if the returned name is not a valid group name.
    pub fn parse(line: &StatusLine, fallback_name: Option<&GroupName>) -> Result<Self> {
        const CONTEXT: &str = "GROUP response";

        let estimated_count = line.arg_u64(0, CONTEXT, "count")?;
        let low = line.arg_u64(1, CONTEXT, "low")?;
        let high = line.arg_u64(2, CONTEXT, "high")?;

        let name = match line.args().nth(3) {
            Some(raw) => GroupName::parse(raw)?,
            None => fallback_name.cloned().ok_or(ProtoError::MissingField {
                context: CONTEXT,
                field: "group",
            })?,
        };

        Ok(Self {
            name,
            estimated_count,
            low,
            high,
        })
    }

    /// Whether the group holds no articles.
    pub const fn is_empty(&self) -> bool {
        self.estimated_count == 0 || self.high == 0 || self.low > self.high
    }

    /// The inclusive range of article numbers that may be present, if any.
    ///
    /// Numbers inside the range can still be absent: cancelled and expired articles leave
    /// gaps, which is why `OVER` over a range may return fewer records than requested.
    pub const fn range(&self) -> Option<(u64, u64)> {
        if self.is_empty() {
            None
        } else {
            Some((self.low, self.high))
        }
    }
}

/// Whether a group accepts postings, as reported by `LIST ACTIVE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostingStatus {
    /// `y` — posting is permitted.
    Permitted,
    /// `n` — posting is not permitted.
    Prohibited,
    /// `m` — the group is moderated; postings are mailed to the moderator.
    Moderated,
    /// `j` — articles are filed in `junk` instead.
    Junked,
    /// `x` — the group is locally disabled.
    Disabled,
    /// `=name` — the group is an alias for another group.
    AliasFor(String),
    /// Anything else the server reported. Servers invent status flags; an unknown one must
    /// not make the group unusable.
    Other(String),
}

impl PostingStatus {
    /// Interprets the status field of a `LIST ACTIVE` line.
    pub fn parse(raw: &str) -> Self {
        match raw {
            "y" => Self::Permitted,
            "n" => Self::Prohibited,
            "m" => Self::Moderated,
            "j" => Self::Junked,
            "x" => Self::Disabled,
            other => match other.strip_prefix('=') {
                Some(target) => Self::AliasFor(target.to_owned()),
                None => Self::Other(other.to_owned()),
            },
        }
    }

    /// Whether a client may attempt to post to this group.
    ///
    /// Moderated groups count as postable: the server accepts the article and forwards it.
    pub const fn allows_posting(&self) -> bool {
        matches!(self, Self::Permitted | Self::Moderated)
    }

    /// A short human-readable description, for a group listing.
    pub fn describe(&self) -> String {
        match self {
            Self::Permitted => "posting allowed".to_owned(),
            Self::Prohibited => "read only".to_owned(),
            Self::Moderated => "moderated".to_owned(),
            Self::Junked => "postings junked".to_owned(),
            Self::Disabled => "locally disabled".to_owned(),
            Self::AliasFor(target) => format!("alias for {target}"),
            Self::Other(flag) => format!("status {flag:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(line: &str) -> StatusLine {
        StatusLine::parse(line.as_bytes()).unwrap()
    }

    #[test]
    fn accepts_ordinary_group_names() {
        for name in ["misc.test", "comp.lang.rust", "de.alt.fan.0815", "a"] {
            assert!(GroupName::parse(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn rejects_empty_wildmat_and_whitespace_names() {
        for bad in [
            "",
            "comp.*",
            "comp.lang.?",
            "a,b",
            "!a",
            "back\\slash",
            "has space",
            "has\ttab",
            "trailing\r\n",
            "caf\u{e9}.test",
        ] {
            assert!(GroupName::parse(bad).is_err(), "expected {bad:?} rejected");
        }
        assert!(GroupName::parse(&"a".repeat(MAX_GROUP_NAME_LEN + 1)).is_err());
    }

    #[test]
    fn display_honours_field_width() {
        // Callers build aligned listings with `{:<20}`; a Display that ignored the width
        // would leave every column ragged.
        let name = GroupName::parse("misc.test").unwrap();
        assert_eq!(format!("[{name:<20}]"), "[misc.test           ]");
        assert_eq!(format!("[{name:>20}]"), "[           misc.test]");
        assert_eq!(format!("[{name}]"), "[misc.test]");
    }

    #[test]
    fn exposes_hierarchy_and_components() {
        let name = GroupName::parse("comp.lang.rust").unwrap();
        assert_eq!(name.hierarchy(), "comp");
        assert_eq!(
            name.components().collect::<Vec<_>>(),
            ["comp", "lang", "rust"]
        );
    }

    #[test]
    fn parses_a_group_response() {
        let summary =
            GroupSummary::parse(&status("211 1234 3000234 3002322 misc.test"), None).unwrap();
        assert_eq!(summary.name.as_str(), "misc.test");
        assert_eq!(summary.estimated_count, 1234);
        assert_eq!(summary.range(), Some((3_000_234, 3_002_322)));
        assert!(!summary.is_empty());
    }

    #[test]
    fn recognises_both_spellings_of_an_empty_group() {
        let zeros = GroupSummary::parse(&status("211 0 0 0 empty.group"), None).unwrap();
        assert!(zeros.is_empty());
        assert_eq!(zeros.range(), None);

        // RFC 3977 §6.1.1.2 also permits low = high + 1 for an empty group.
        let inverted = GroupSummary::parse(&status("211 0 4 3 empty.group"), None).unwrap();
        assert!(inverted.is_empty());
    }

    #[test]
    fn falls_back_to_the_requested_name_when_the_server_omits_it() {
        let requested = GroupName::parse("misc.test").unwrap();
        let summary = GroupSummary::parse(&status("211 3 1 3"), Some(&requested)).unwrap();
        assert_eq!(summary.name, requested);

        assert!(matches!(
            GroupSummary::parse(&status("211 3 1 3"), None),
            Err(ProtoError::MissingField { .. })
        ));
    }

    #[test]
    fn rejects_a_group_response_with_unparseable_numbers() {
        assert!(matches!(
            GroupSummary::parse(&status("211 many 1 3 misc.test"), None),
            Err(ProtoError::InvalidNumber { .. })
        ));
        assert!(matches!(
            GroupSummary::parse(&status("211 1"), None),
            Err(ProtoError::MissingField { .. })
        ));
    }

    #[test]
    fn parses_posting_status_flags() {
        assert_eq!(PostingStatus::parse("y"), PostingStatus::Permitted);
        assert_eq!(PostingStatus::parse("n"), PostingStatus::Prohibited);
        assert_eq!(PostingStatus::parse("m"), PostingStatus::Moderated);
        assert_eq!(
            PostingStatus::parse("=comp.lang.rust"),
            PostingStatus::AliasFor("comp.lang.rust".to_owned())
        );
        assert_eq!(
            PostingStatus::parse("weird"),
            PostingStatus::Other("weird".to_owned())
        );
        assert!(PostingStatus::parse("m").allows_posting());
        assert!(!PostingStatus::parse("n").allows_posting());
        assert!(!PostingStatus::parse("weird").allows_posting());
    }

    #[test]
    fn describes_every_posting_status() {
        assert_eq!(PostingStatus::parse("y").describe(), "posting allowed");
        assert_eq!(PostingStatus::parse("n").describe(), "read only");
        assert_eq!(PostingStatus::parse("m").describe(), "moderated");
        assert_eq!(PostingStatus::parse("j").describe(), "postings junked");
        assert_eq!(PostingStatus::parse("x").describe(), "locally disabled");
        assert_eq!(PostingStatus::parse("=other").describe(), "alias for other");
        assert_eq!(PostingStatus::parse("q").describe(), "status \"q\"");
    }
}
