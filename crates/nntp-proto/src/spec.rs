//! Ways of naming an article, and ranges of article numbers.

use crate::message_id::MessageId;

/// How an article is named in a command.
///
/// RFC 3977 §6.2 allows `ARTICLE`, `HEAD`, `BODY` and `STAT` to take a message-id, an
/// article number, or nothing at all (meaning the currently selected article).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArticleSpec {
    /// A message-id. Works without a group being selected.
    MessageId(MessageId),
    /// An article number within the currently selected group.
    Number(u64),
    /// The currently selected article.
    Current,
}

impl ArticleSpec {
    /// The command argument for this spec, or `None` for [`Self::Current`].
    pub fn to_argument(&self) -> Option<String> {
        match self {
            Self::MessageId(id) => Some(id.as_str().to_owned()),
            Self::Number(n) => Some(n.to_string()),
            Self::Current => None,
        }
    }

    /// Whether this spec requires a group to have been selected first.
    pub const fn needs_selected_group(&self) -> bool {
        matches!(self, Self::Number(_) | Self::Current)
    }
}

impl From<u64> for ArticleSpec {
    fn from(value: u64) -> Self {
        Self::Number(value)
    }
}

impl From<MessageId> for ArticleSpec {
    fn from(value: MessageId) -> Self {
        Self::MessageId(value)
    }
}

/// A range of article numbers, as accepted by `OVER`, `HDR` and `LISTGROUP`.
///
/// RFC 3977 §6.0 defines three forms: a single number, `low-high`, and `low-` meaning
/// "everything from `low` upwards".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    /// A single article number.
    Single(u64),
    /// An inclusive range.
    Between {
        /// First article number, inclusive.
        low: u64,
        /// Last article number, inclusive.
        high: u64,
    },
    /// Every article from this number to the end of the group.
    ///
    /// Not all servers accept the open form; the client falls back to an explicit
    /// `low-high` built from the group watermarks when a server rejects it.
    From(u64),
}

impl Range {
    /// Builds an inclusive range, normalising a reversed pair.
    ///
    /// A reversed range (`high < low`) is a legal thing to *send* — the RFC says it
    /// matches no articles — but it is almost always a caller bug, so the endpoints are
    /// swapped rather than silently returning nothing.
    pub const fn between(low: u64, high: u64) -> Self {
        if low <= high {
            Self::Between { low, high }
        } else {
            Self::Between {
                low: high,
                high: low,
            }
        }
    }

    /// The command argument for this range.
    pub fn to_argument(&self) -> String {
        match self {
            Self::Single(n) => n.to_string(),
            Self::Between { low, high } => format!("{low}-{high}"),
            Self::From(low) => format!("{low}-"),
        }
    }

    /// Whether `number` falls inside the range.
    pub const fn contains(&self, number: u64) -> bool {
        match *self {
            Self::Single(n) => number == n,
            Self::Between { low, high } => number >= low && number <= high,
            Self::From(low) => number >= low,
        }
    }

    /// How many article numbers the range spans, or `None` if it is unbounded.
    ///
    /// Named `count` rather than `len` because it counts numbers, not articles: gaps left
    /// by expiry and cancellation mean the server will usually return fewer records. A
    /// range always spans at least one number, so there is no empty case.
    pub const fn count(&self) -> Option<u64> {
        match *self {
            Self::Single(_) => Some(1),
            Self::Between { low, high } => Some(high - low + 1),
            Self::From(_) => None,
        }
    }

    /// Splits the range into chunks of at most `chunk` numbers each.
    ///
    /// Used to keep a single `OVER` request from returning a response too large to hold in
    /// memory, and to give the UI something to show before the whole range has arrived.
    /// An unbounded range cannot be split and is returned unchanged.
    pub fn chunks(&self, chunk: u64) -> Vec<Self> {
        let (low, high) = match *self {
            Self::Single(n) => return vec![Self::Single(n)],
            Self::From(low) => return vec![Self::From(low)],
            Self::Between { low, high } => (low, high),
        };

        if chunk == 0 {
            return vec![Self::between(low, high)];
        }

        let mut out = Vec::new();
        let mut start = low;
        loop {
            // Saturating arithmetic: `start + chunk - 1` can overflow near u64::MAX.
            let end = start.saturating_add(chunk - 1).min(high);
            out.push(Self::between(start, end));
            if end >= high {
                break;
            }
            start = end + 1;
        }
        out
    }
}

impl From<u64> for Range {
    fn from(value: u64) -> Self {
        Self::Single(value)
    }
}

/// The argument to `OVER` or `HDR`: either a range of numbers or a single message-id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeOrId {
    /// A range of article numbers in the selected group.
    Range(Range),
    /// A single article by message-id.
    MessageId(MessageId),
    /// The currently selected article.
    Current,
}

impl RangeOrId {
    /// The command argument, or `None` for [`Self::Current`].
    pub fn to_argument(&self) -> Option<String> {
        match self {
            Self::Range(range) => Some(range.to_argument()),
            Self::MessageId(id) => Some(id.as_str().to_owned()),
            Self::Current => None,
        }
    }
}

impl From<Range> for RangeOrId {
    fn from(value: Range) -> Self {
        Self::Range(value)
    }
}

impl From<MessageId> for RangeOrId {
    fn from(value: MessageId) -> Self {
        Self::MessageId(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_range_arguments() {
        assert_eq!(Range::Single(42).to_argument(), "42");
        assert_eq!(Range::between(1, 10).to_argument(), "1-10");
        assert_eq!(Range::From(7).to_argument(), "7-");
    }

    #[test]
    fn normalises_a_reversed_range() {
        assert_eq!(Range::between(10, 1), Range::between(1, 10));
    }

    #[test]
    fn reports_containment_and_length() {
        let range = Range::between(5, 9);
        assert!(range.contains(5) && range.contains(9) && !range.contains(10));
        assert_eq!(range.count(), Some(5));
        assert_eq!(Range::Single(3).count(), Some(1));
        assert_eq!(Range::From(3).count(), None);
        assert!(Range::From(3).contains(u64::MAX));
    }

    #[test]
    fn splits_a_range_into_chunks() {
        let chunks = Range::between(1, 10).chunks(4);
        assert_eq!(
            chunks,
            vec![
                Range::between(1, 4),
                Range::between(5, 8),
                Range::between(9, 10)
            ]
        );
    }

    #[test]
    fn chunking_edge_cases_do_not_loop_or_overflow() {
        assert_eq!(Range::between(1, 4).chunks(4), vec![Range::between(1, 4)]);
        assert_eq!(Range::between(3, 3).chunks(10), vec![Range::between(3, 3)]);
        assert_eq!(Range::between(1, 3).chunks(1).len(), 3);
        // A zero chunk size would otherwise divide by zero or spin forever.
        assert_eq!(Range::between(1, 9).chunks(0), vec![Range::between(1, 9)]);
        // Near the top of the number space the naive `start + chunk - 1` overflows.
        let top = Range::between(u64::MAX - 2, u64::MAX).chunks(2);
        assert_eq!(
            top,
            vec![
                Range::between(u64::MAX - 2, u64::MAX - 1),
                Range::between(u64::MAX, u64::MAX)
            ]
        );
        assert_eq!(Range::From(5).chunks(2), vec![Range::From(5)]);
    }

    #[test]
    fn article_specs_render_and_report_their_needs() {
        let id = MessageId::parse("<a@b>").unwrap();
        assert_eq!(
            ArticleSpec::MessageId(id.clone()).to_argument().as_deref(),
            Some("<a@b>")
        );
        assert_eq!(ArticleSpec::Number(9).to_argument().as_deref(), Some("9"));
        assert_eq!(ArticleSpec::Current.to_argument(), None);

        assert!(!ArticleSpec::MessageId(id).needs_selected_group());
        assert!(ArticleSpec::Number(9).needs_selected_group());
        assert!(ArticleSpec::Current.needs_selected_group());
    }

    #[test]
    fn range_or_id_renders() {
        assert_eq!(
            RangeOrId::from(Range::between(1, 2))
                .to_argument()
                .as_deref(),
            Some("1-2")
        );
        assert_eq!(
            RangeOrId::from(MessageId::parse("<a@b>").unwrap())
                .to_argument()
                .as_deref(),
            Some("<a@b>")
        );
        assert_eq!(RangeOrId::Current.to_argument(), None);
    }
}
