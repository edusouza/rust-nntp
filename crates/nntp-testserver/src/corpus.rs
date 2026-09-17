//! The articles and groups a test server serves.
//!
//! Stored as raw lines rather than strings so that a fixture can contain the things that
//! break parsers: unlabelled 8-bit bytes, a body line starting with `.`, a folded header,
//! a malformed `Date`.

use std::collections::BTreeMap;

/// One stored article.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Article {
    /// The message-id, including angle brackets.
    pub message_id: String,
    /// Header lines, without terminators and before dot-stuffing.
    pub head: Vec<Vec<u8>>,
    /// Body lines, without terminators and before dot-stuffing.
    pub body: Vec<Vec<u8>>,
}

impl Article {
    /// An article with a message-id and nothing else.
    pub fn new(message_id: impl Into<String>) -> Self {
        Self {
            message_id: message_id.into(),
            head: Vec::new(),
            body: Vec::new(),
        }
    }

    /// Adds a header line.
    #[must_use]
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.head.push(format!("{name}: {value}").into_bytes());
        self
    }

    /// Adds a header line from raw bytes, for values that are not valid UTF-8.
    #[must_use]
    pub fn raw_header(mut self, line: impl Into<Vec<u8>>) -> Self {
        self.head.push(line.into());
        self
    }

    /// Adds a body line.
    #[must_use]
    pub fn line(mut self, line: &str) -> Self {
        self.body.push(line.as_bytes().to_vec());
        self
    }

    /// Adds a body line from raw bytes.
    #[must_use]
    pub fn raw_line(mut self, line: impl Into<Vec<u8>>) -> Self {
        self.body.push(line.into());
        self
    }

    /// A convenience constructor for the common shape: subject, author, date and a body.
    ///
    /// The `Message-ID` header is added automatically so it always agrees with
    /// [`Self::message_id`].
    pub fn simple(message_id: &str, subject: &str, from: &str, date: &str, body: &[&str]) -> Self {
        let mut article = Self::new(message_id)
            .header("Message-ID", message_id)
            .header("From", from)
            .header("Subject", subject)
            .header("Date", date)
            .header("Newsgroups", "misc.test");

        for line in body {
            article = article.line(line);
        }
        article
    }

    /// The first value of a header, compared case-insensitively.
    ///
    /// Folded continuation lines are joined, as a reader would see them.
    pub fn header_value(&self, name: &str) -> Option<Vec<u8>> {
        let mut found: Option<Vec<u8>> = None;

        for line in &self.head {
            let is_continuation = matches!(line.first(), Some(b' ' | b'\t'));
            if is_continuation {
                if let Some(value) = &mut found {
                    value.extend_from_slice(line);
                    continue;
                }
                continue;
            }

            if found.is_some() {
                break;
            }

            let Some(colon) = line.iter().position(|b| *b == b':') else {
                continue;
            };
            let Some(field) = line.get(..colon) else {
                continue;
            };
            if !field.eq_ignore_ascii_case(name.as_bytes()) {
                continue;
            }
            let value = line.get(colon + 1..).unwrap_or_default();
            found = Some(trim(value).to_vec());
        }

        found
    }

    /// The article's size in octets, counting CRLF terminators, as `:bytes` reports it.
    pub fn byte_len(&self) -> usize {
        let head: usize = self.head.iter().map(|line| line.len() + 2).sum();
        let body: usize = self.body.iter().map(|line| line.len() + 2).sum();
        // Plus the empty line separating head from body.
        head + 2 + body
    }

    /// The article's length in body lines, as `:lines` reports it.
    pub fn line_count(&self) -> usize {
        self.body.len()
    }

    /// The overview line for this article, in the standard eight-field layout.
    pub fn overview_line(&self, number: u64, group: &str) -> Vec<u8> {
        let field = |name: &str| self.header_value(name).unwrap_or_default();

        let mut line = number.to_string().into_bytes();
        for value in [
            field("Subject"),
            field("From"),
            field("Date"),
            self.message_id.clone().into_bytes(),
            field("References"),
            self.byte_len().to_string().into_bytes(),
            self.line_count().to_string().into_bytes(),
        ] {
            line.push(b'\t');
            line.extend_from_slice(&value);
        }

        line.push(b'\t');
        line.extend_from_slice(format!("Xref: test.invalid {group}:{number}").as_bytes());
        line
    }
}

/// Whether a group accepts postings, as `LIST ACTIVE` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Posting {
    /// `y`
    Permitted,
    /// `n`
    Prohibited,
    /// `m`
    Moderated,
}

impl Posting {
    /// The flag character used on the wire.
    pub const fn flag(self) -> &'static str {
        match self {
            Self::Permitted => "y",
            Self::Prohibited => "n",
            Self::Moderated => "m",
        }
    }
}

/// One newsgroup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// The group name.
    pub name: String,
    /// The description reported by `LIST NEWSGROUPS`.
    pub description: String,
    /// The posting status reported by `LIST ACTIVE`.
    pub posting: Posting,
    /// When the group was created, in seconds since the epoch.
    pub created: u64,
    /// Articles by number. A `BTreeMap` because article numbers are sparse — expiry and
    /// cancellation leave gaps — and every listing has to come out in order.
    pub articles: BTreeMap<u64, Article>,
}

impl Group {
    /// An empty group.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            posting: Posting::Permitted,
            created: 1_000_000_000,
            articles: BTreeMap::new(),
        }
    }

    /// Sets the description.
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Sets the posting status.
    #[must_use]
    pub fn posting(mut self, posting: Posting) -> Self {
        self.posting = posting;
        self
    }

    /// Adds an article at an explicit number.
    #[must_use]
    pub fn article_at(mut self, number: u64, article: Article) -> Self {
        self.articles.insert(number, article);
        self
    }

    /// Adds an article after the current highest number.
    #[must_use]
    pub fn article(self, article: Article) -> Self {
        let number = self.high().map_or(1, |high| high + 1);
        self.article_at(number, article)
    }

    /// The lowest article number present, or `None` if the group is empty.
    pub fn low(&self) -> Option<u64> {
        self.articles.keys().next().copied()
    }

    /// The highest article number present, or `None` if the group is empty.
    pub fn high(&self) -> Option<u64> {
        self.articles.keys().next_back().copied()
    }

    /// The watermarks as `LIST ACTIVE` and `GROUP` report them for an empty group:
    /// `high = 0`, `low = 1`.
    pub fn watermarks(&self) -> (u64, u64) {
        match (self.low(), self.high()) {
            (Some(low), Some(high)) => (low, high),
            _ => (1, 0),
        }
    }

    /// Looks up an article by number.
    pub fn article_by_number(&self, number: u64) -> Option<&Article> {
        self.articles.get(&number)
    }

    /// Looks up an article by message-id.
    pub fn article_by_id(&self, message_id: &str) -> Option<(u64, &Article)> {
        self.articles
            .iter()
            .find(|(_, article)| article.message_id.eq_ignore_ascii_case(message_id))
            .map(|(number, article)| (*number, article))
    }
}

/// Everything a test server serves.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Corpus {
    groups: Vec<Group>,
}

impl Corpus {
    /// An empty corpus.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a group.
    #[must_use]
    pub fn group(mut self, group: Group) -> Self {
        self.groups.push(group);
        self
    }

    /// The groups, in the order they were added.
    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    /// Looks up a group by name, compared case-sensitively as servers do.
    pub fn find(&self, name: &str) -> Option<&Group> {
        self.groups.iter().find(|group| group.name == name)
    }

    /// Looks up an article by message-id across every group.
    pub fn find_by_id(&self, message_id: &str) -> Option<(&Group, u64, &Article)> {
        self.groups.iter().find_map(|group| {
            group
                .article_by_id(message_id)
                .map(|(number, article)| (group, number, article))
        })
    }

    /// A corpus shaped like real traffic, including the cases that break parsers.
    ///
    /// Contains:
    ///
    /// - `misc.test` — a plain thread of three articles, one of which has a body line
    ///   beginning with `.` and a signature separator, and one with a `Date` no parser
    ///   can read;
    /// - `comp.lang.rust` — non-ASCII subjects and authors in both RFC 2047 encodings,
    ///   plus an unlabelled Latin-1 header, and a sparse numbering with gaps;
    /// - `de.comp.test` — a moderated group with a non-ASCII description;
    /// - `empty.group` — no articles at all.
    pub fn sample() -> Self {
        Self::new()
            .group(
                Group::new("misc.test")
                    .description("For testing purposes only")
                    .article(Article::simple(
                        "<root@test.invalid>",
                        "A plain test article",
                        "Demo User <nobody@example.net>",
                        "Wed, 17 Sep 2026 08:00:00 +0000",
                        &["This is the first article.", "It has two lines."],
                    ))
                    .article(
                        Article::simple(
                            "<reply@test.invalid>",
                            "Re: A plain test article",
                            "Another User <other@example.net>",
                            "Wed, 17 Sep 2026 09:30:00 +0200",
                            &[
                                "> This is the first article.",
                                "Quite so.",
                                ".signature-like line that starts with a dot",
                                "-- ",
                                "Another User",
                            ],
                        )
                        .header("References", "<root@test.invalid>"),
                    )
                    .article(Article::simple(
                        "<undated@test.invalid>",
                        "An article with an unreadable date",
                        "Broken Client <broken@example.net>",
                        "yesterday afternoon",
                        &["The Date header above is not a date."],
                    )),
            )
            .group(
                Group::new("comp.lang.rust")
                    .description("Discussion of the Rust programming language")
                    // Sparse numbering: expiry left a gap between 4237 and 4242.
                    .article_at(
                        4237,
                        Article::simple(
                            "<qp@test.invalid>",
                            "=?UTF-8?Q?caf=C3=A9_and_crates?=",
                            "=?ISO-8859-1?Q?Bj=F8rn_Nordm=E6l?= <bjorn@example.no>",
                            "Wed, 17 Sep 2026 08:09:10 +0200",
                            &["Quoted-printable subject, plain body."],
                        )
                        .header("Newsgroups", "comp.lang.rust"),
                    )
                    .article_at(
                        4242,
                        Article::new("<b64@test.invalid>")
                            .header("Message-ID", "<b64@test.invalid>")
                            .header("From", "=?UTF-8?B?w4VzYSBMaW5kcXZpc3Q=?= <asa@example.se>")
                            .header("Subject", "=?UTF-8?B?UmU6IGNhZsOpIGFuZCBjcmF0ZXM=?=")
                            .header("Date", "Wed, 17 Sep 2026 10:11:12 +0200")
                            .header("Newsgroups", "comp.lang.rust")
                            .header("References", "<qp@test.invalid>")
                            .header("Content-Type", "text/plain; charset=UTF-8")
                            .header("Content-Transfer-Encoding", "quoted-printable")
                            // An unlabelled 8-bit header value, which is not legal and is
                            // nonetheless common.
                            .raw_header(b"Organization: Caf\xe9 Central".to_vec())
                            .line("Base64 subject, quoted-printable body: caf=C3=A9.")
                            .line("A soft line break follows here =")
                            .line("and this continues the same line."),
                    ),
            )
            .group(
                Group::new("de.comp.test")
                    .description("Deutschsprachige Testgruppe für Umlaute: äöü")
                    .posting(Posting::Moderated)
                    .article(Article::simple(
                        "<de@test.invalid>",
                        "Grüße",
                        "Tester <test@example.de>",
                        "Wed, 17 Sep 2026 11:00:00 +0200",
                        &["Ein Test."],
                    )),
            )
            .group(Group::new("empty.group").description("A group with no articles"))
    }

    /// [`Self::sample`] plus a group of MIME traffic: a mail-to-news gateway multipart,
    /// an article in `format=flowed`, and an article that is nothing but an attachment.
    ///
    /// Separate from `sample` so that the counts every other test asserts do not move
    /// whenever this corpus gains an article.
    pub fn sample_with_mime() -> Self {
        Self::sample().group(mime_group())
    }
}

/// A group of the MIME traffic a reader has to survive.
///
/// Kept out of [`Corpus::sample`] on purpose: that corpus is the shared fixture for most
/// of the suite, and a shared fixture that grows whenever one test wants something new is
/// a fixture whose article and group counts nobody can assert. [`Corpus::sample_with_mime`]
/// is for the tests that need this.
fn mime_group() -> Group {
    Group::new("news.software.readers")
        .description("Newsreaders, and the MIME they have to survive")
        // A mail-to-news gateway article: the same text twice, plus an
        // attachment. Before MIME support the reader showed all of this,
        // boundary lines and base64 included.
        .article(
            Article::new("<mixed@test.invalid>")
                .header("Message-ID", "<mixed@test.invalid>")
                .header("From", "Gateway User <gw@example.org>")
                .header("Subject", "A multipart article from a gateway")
                .header("Date", "Wed, 17 Sep 2026 12:00:00 +0000")
                .header("Newsgroups", "news.software.readers")
                .header("Content-Type", "multipart/mixed; boundary=\"outer\"")
                .line("This preamble belongs to no part and must not be shown.")
                .line("--outer")
                .line("Content-Type: multipart/alternative; boundary=\"inner\"")
                .line("")
                .line("--inner")
                .line("Content-Type: text/plain; charset=UTF-8")
                .line("")
                .line("The readable version, with an accent: café.")
                .line("--inner")
                .line("Content-Type: text/html; charset=UTF-8")
                .line("")
                .line("<html><body><p>The noisy version.</p></body></html>")
                .line("--inner--")
                .line("--outer")
                .line("Content-Type: text/x-patch; name=\"fix.patch\"")
                .line("Content-Disposition: attachment; filename=\"fix.patch\"")
                .line("")
                .line("--- a/x")
                .line("+++ b/x")
                .line("--outer--")
                .line("This epilogue belongs to no part either."),
        )
        // `format=flowed`: paragraphs wrapped by the sender, with the soft
        // breaks marked by a trailing space. Shown one short line at a time
        // before RFC 3676 support.
        .article(
            Article::new("<flowed@test.invalid>")
                .header("Message-ID", "<flowed@test.invalid>")
                .header("From", "Flowed Sender <flow@example.org>")
                .header("Subject", "An article in format=flowed")
                .header("Date", "Wed, 17 Sep 2026 12:30:00 +0000")
                .header("Newsgroups", "news.software.readers")
                .header("Content-Type", "text/plain; charset=UTF-8; format=flowed")
                .line("This paragraph was wrapped by the sender at a narrow ")
                .line("width, and should be shown as one paragraph rather ")
                .line("than as three short lines.")
                .line("> The quoted part was wrapped too, and must not be ")
                .line("> joined to the reply below it.")
                .line("A second paragraph.")
                .line("-- ")
                .line("The signature separator above ends in a space and is")
                .line("still a hard break."),
        )
        // Nothing but an attachment: the body pane would otherwise be blank,
        // which reads as a bug rather than as a fact about the article.
        .article(
            Article::new("<onlyblob@test.invalid>")
                .header("Message-ID", "<onlyblob@test.invalid>")
                .header("From", "Binary Poster <bin@example.org>")
                .header("Subject", "An article that is only an attachment")
                .header("Date", "Wed, 17 Sep 2026 13:00:00 +0000")
                .header("Newsgroups", "news.software.readers")
                .header("Content-Type", "multipart/mixed; boundary=\"b\"")
                .line("--b")
                .line("Content-Type: application/octet-stream")
                .line("Content-Transfer-Encoding: base64")
                .line("Content-Disposition: attachment; filename*=UTF-8''relat%C3%B3rio.bin")
                .line("")
                .line("AAECAwQFBgc=")
                .line("--b--"),
        )
}

fn trim(bytes: &[u8]) -> &[u8] {
    let mut start = 0usize;
    let mut end = bytes.len();
    while matches!(bytes.get(start), Some(b' ' | b'\t')) {
        start += 1;
    }
    while end > start && matches!(bytes.get(end - 1), Some(b' ' | b'\t')) {
        end -= 1;
    }
    bytes.get(start..end).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_group_watermarks_follow_the_rfc() {
        // RFC 3977 §7.6.3: an empty group reports high = 0 and low = 1.
        let group = Group::new("empty.group");
        assert_eq!(group.watermarks(), (1, 0));
        assert_eq!(group.low(), None);
        assert_eq!(group.high(), None);
    }

    #[test]
    fn articles_are_numbered_sequentially_unless_placed() {
        let group = Group::new("g")
            .article(Article::new("<a@x>"))
            .article(Article::new("<b@x>"))
            .article_at(100, Article::new("<c@x>"))
            .article(Article::new("<d@x>"));

        assert_eq!(
            group.articles.keys().copied().collect::<Vec<_>>(),
            [1, 2, 100, 101]
        );
        assert_eq!(group.watermarks(), (1, 101));
    }

    #[test]
    fn looks_articles_up_by_number_and_by_id() {
        let group = Group::new("g").article_at(7, Article::new("<a@x>"));
        assert!(group.article_by_number(7).is_some());
        assert!(group.article_by_number(8).is_none());
        assert_eq!(group.article_by_id("<a@x>").map(|(n, _)| n), Some(7));
        // Message-ids are matched case-insensitively.
        assert!(group.article_by_id("<A@X>").is_some());
    }

    #[test]
    fn reads_header_values_including_folded_ones() {
        let article = Article::new("<a@x>")
            .header("Subject", "hello")
            .raw_header(b"References: <one@x>".to_vec())
            .raw_header(b"\t<two@x>".to_vec());

        assert_eq!(
            article.header_value("subject").as_deref(),
            Some(&b"hello"[..])
        );
        assert_eq!(
            article.header_value("References").as_deref(),
            Some(&b"<one@x>\t<two@x>"[..])
        );
        assert_eq!(article.header_value("Missing"), None);
    }

    #[test]
    fn builds_an_overview_line_in_the_standard_order() {
        let article = Article::simple(
            "<a@x>",
            "subject here",
            "author@x",
            "Wed, 17 Sep 2026 08:00:00 +0000",
            &["one", "two"],
        );
        let line = String::from_utf8(article.overview_line(42, "misc.test")).unwrap();
        let fields: Vec<&str> = line.split('\t').collect();

        assert_eq!(fields[0], "42");
        assert_eq!(fields[1], "subject here");
        assert_eq!(fields[2], "author@x");
        assert_eq!(fields[3], "Wed, 17 Sep 2026 08:00:00 +0000");
        assert_eq!(fields[4], "<a@x>");
        assert_eq!(fields[5], "");
        assert_eq!(fields[7], "2");
        assert_eq!(fields[8], "Xref: test.invalid misc.test:42");
    }

    #[test]
    fn counts_bytes_including_terminators_and_the_blank_line() {
        let article = Article::new("<a@x>").header("A", "b").line("body");
        // "A: b\r\n" = 6, blank line = 2, "body\r\n" = 6.
        assert_eq!(article.byte_len(), 14);
        assert_eq!(article.line_count(), 1);
    }

    #[test]
    fn the_sample_corpus_contains_the_awkward_cases() {
        let corpus = Corpus::sample();

        assert!(corpus.find("misc.test").is_some());
        assert!(corpus.find("no.such.group").is_none());
        assert!(corpus.find("empty.group").unwrap().articles.is_empty());
        assert_eq!(
            corpus.find("de.comp.test").unwrap().posting,
            Posting::Moderated
        );

        // A sparse group, which is what makes OVER return fewer records than requested.
        let rust = corpus.find("comp.lang.rust").unwrap();
        assert_eq!(rust.watermarks(), (4237, 4242));
        assert_eq!(rust.articles.len(), 2);

        // A body line beginning with a dot, which must be stuffed on the wire.
        let reply = corpus
            .find("misc.test")
            .unwrap()
            .article_by_id("<reply@test.invalid>")
            .unwrap()
            .1;
        assert!(reply.body.iter().any(|line| line.first() == Some(&b'.')));

        // Cross-group lookup by message-id, which is how ARTICLE <id> works without a
        // selected group.
        let (group, number, _) = corpus.find_by_id("<qp@test.invalid>").unwrap();
        assert_eq!(group.name, "comp.lang.rust");
        assert_eq!(number, 4237);
    }
}
