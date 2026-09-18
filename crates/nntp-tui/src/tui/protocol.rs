//! The messages exchanged between the render loop and the network worker.
//!
//! Two channels, no shared mutable state, no locks on the render path — see
//! [ADR-0003](https://github.com/edusouza/rust-nntp/blob/main/docs/adr/0003-blocking-io-on-a-worker-thread.md).
//! Keeping the vocabulary in its own module makes the boundary explicit: anything that is
//! not in here cannot cross it.

use nntp_proto::{ArticleSpec, GroupName, GroupSummary, OverviewRecord, PostingStatus, Range};

/// Something the user interface wants done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Fetch the group list, with descriptions if the server will give them.
    LoadGroups,
    /// Select a group and fetch the newest `count` overview records from it.
    OpenGroup {
        /// The group to select.
        group: GroupName,
        /// How many of the newest articles to fetch.
        count: u64,
    },
    /// Fetch overview records for an explicit range in the selected group.
    LoadOverview {
        /// The group the range belongs to.
        group: GroupName,
        /// The range to fetch.
        range: Range,
    },
    /// Fetch one article.
    LoadArticle {
        /// The group to select first, if the article is named by number.
        group: Option<GroupName>,
        /// Which article.
        spec: ArticleSpec,
    },
    /// Close the connection and stop the worker.
    Shutdown,
}

/// Something that happened, for the user interface to react to.
#[derive(Debug, Clone)]
pub enum Event {
    /// The worker has a usable connection.
    Connected {
        /// A short description of the server, for the status bar.
        server: String,
        /// The greeting text.
        greeting: String,
        /// Whether the link is encrypted.
        encrypted: bool,
    },
    /// The group list arrived.
    Groups(Vec<GroupRow>),
    /// A group was selected.
    GroupOpened(Box<GroupSummary>),
    /// Overview records arrived for a group.
    Overview {
        /// The group they belong to, so a late reply for a group the user has navigated
        /// away from can be discarded rather than displayed under the wrong heading.
        group: GroupName,
        /// The records, in article-number order.
        records: Vec<OverviewRecord>,
        /// How many lines the server sent that could not be parsed.
        skipped: usize,
    },
    /// An article arrived.
    Article(Box<nntp_proto::Article>),
    /// Progress on a long operation, for the status bar.
    Progress(String),
    /// A request the user abandoned.
    ///
    /// Separate from [`Self::Failed`] on purpose: the user asked for this, and telling
    /// them their request failed when they stopped it themselves is the kind of message
    /// that makes a program feel broken.
    Cancelled {
        /// What was being read, for the status line.
        context: String,
    },
    /// Something went wrong. Not fatal to the interface.
    Failed {
        /// What was being attempted.
        context: String,
        /// The error, already formatted.
        message: String,
    },
    /// The connection is gone; the worker will reconnect on the next request.
    Disconnected(String),
    /// The worker has stopped and will send nothing further.
    Stopped,
}

/// A group as the interface shows it.
///
/// Flattened out of [`nntp_proto::ActiveEntry`] plus the description from
/// `LIST NEWSGROUPS`, because the interface wants them together and the protocol
/// delivers them separately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupRow {
    /// The group name.
    pub name: GroupName,
    /// Lowest article number.
    pub low: u64,
    /// Highest article number.
    pub high: u64,
    /// Whether posting is allowed.
    pub status: PostingStatus,
    /// The description, if the server provided one.
    pub description: Option<String>,
}

impl GroupRow {
    /// An upper bound on the number of articles.
    ///
    /// The watermarks are all `LIST ACTIVE` gives, and expiry leaves gaps, so this is a
    /// bound rather than a count. The interface labels it as such.
    pub const fn article_bound(&self) -> u64 {
        if self.high >= self.low && self.high > 0 {
            self.high - self.low + 1
        } else {
            0
        }
    }

    /// Whether the group appears to hold no articles.
    pub const fn is_empty(&self) -> bool {
        self.article_bound() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(low: u64, high: u64) -> GroupRow {
        GroupRow {
            name: GroupName::parse("misc.test").unwrap(),
            low,
            high,
            status: PostingStatus::Permitted,
            description: None,
        }
    }

    #[test]
    fn bounds_the_article_count_from_the_watermarks() {
        assert_eq!(row(1, 3).article_bound(), 3);
        assert_eq!(row(4237, 4242).article_bound(), 6);
    }

    #[test]
    fn recognises_both_spellings_of_an_empty_group() {
        // RFC 3977 §7.6.3 allows high = 0, low = 1 for an empty group.
        assert!(row(1, 0).is_empty());
        assert!(row(0, 0).is_empty());
        assert!(!row(1, 1).is_empty());
    }
}
