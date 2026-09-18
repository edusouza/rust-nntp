//! The interface state machine.
//!
//! Deliberately free of both terminal and network: it takes key events and worker events
//! in, and produces state changes and [`Request`]s out. Nothing here draws anything and
//! nothing here blocks. That is what makes the behaviour testable — the tests at the
//! bottom of this file drive the whole reader without a terminal or a socket.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;

use nntp_proto::{ArticleSpec, GroupSummary, OverviewRecord, Range, ThreadNode, Wildmat};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use nntp_client::Cancel;

use crate::config::UiConfig;
use crate::readstate::ReadStore;
use crate::tui::protocol::{Event, FetchToken, GroupRow, GroupScope, Request};

/// How many messages to keep for the message pane.
const LOG_CAPACITY: usize = 200;

/// Which pane has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    /// The group list.
    Groups,
    /// The article list for the selected group.
    Articles,
    /// The article body.
    Body,
}

impl Pane {
    /// The pane to the right, wrapping.
    pub const fn next(self) -> Self {
        match self {
            Self::Groups => Self::Articles,
            Self::Articles => Self::Body,
            Self::Body => Self::Groups,
        }
    }

    /// The pane to the left, wrapping.
    pub const fn previous(self) -> Self {
        match self {
            Self::Groups => Self::Body,
            Self::Articles => Self::Groups,
            Self::Body => Self::Articles,
        }
    }

    /// A short title for the pane border.
    pub const fn title(self) -> &'static str {
        match self {
            Self::Groups => "Groups",
            Self::Articles => "Articles",
            Self::Body => "Article",
        }
    }
}

/// What is on screen instead of the normal layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    /// Nothing; the normal three panes.
    None,
    /// The key binding help.
    Help,
    /// The message log.
    Messages,
}

/// An article prepared for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArticleView {
    /// The article number, if it was fetched by number.
    pub number: Option<u64>,
    /// The decoded subject.
    pub subject: String,
    /// The decoded author.
    pub author: String,
    /// The date, formatted, or the raw text if it could not be parsed.
    pub date: String,
    /// Header lines to show above the body, already decoded.
    pub headers: Vec<(String, String)>,
    /// The body, split into lines: the part chosen for display, decoded and unflowed.
    pub body: Vec<String>,
    /// The parts that are not on screen — attachments, and the alternatives passed over —
    /// one summary line each.
    ///
    /// Empty for the ordinary single-part article, which is most of Usenet.
    pub attachments: Vec<String>,
    /// Whether the article carries a signature, detached or inline.
    ///
    /// Only that one is *present*: nothing here verifies anything, and a reader implying
    /// otherwise would be worse than one that stays quiet.
    pub signed: bool,

    /// The article's own message-id, for the `References` chain of a follow-up.
    pub message_id: Option<nntp_proto::MessageId>,
    /// The chain it already carries, oldest first.
    pub references: Vec<nntp_proto::MessageId>,
    /// Where a follow-up should go: `Followup-To` if the author set one, otherwise the
    /// groups the article was posted to.
    ///
    /// Resolved here rather than at the point of composing, because it is a fact about the
    /// article and RFC 5536 §3.2.6 is easy to get wrong twice.
    pub followup_groups: Vec<String>,
    /// Whether `Followup-To: poster` asked for a mail reply rather than a posting.
    ///
    /// This reader cannot send mail, so a follow-up is refused rather than quietly posted
    /// to a group the author asked it not to go to.
    pub followup_to_poster: bool,
}

impl ArticleView {
    /// Prepares an article for display.
    fn new(article: &nntp_proto::Article, date_format: &str) -> Self {
        // Only the headers a reader actually wants above the body. The rest are available
        // through `nntp-tui article --raw`; putting forty trace headers on screen would
        // push the article off it.
        const SHOWN: [&str; 7] = [
            "From",
            "Newsgroups",
            "Subject",
            "Date",
            "Organization",
            "References",
            "Followup-To",
        ];

        let headers = SHOWN
            .iter()
            .filter_map(|name| {
                article
                    .headers
                    .get_decoded(name)
                    .filter(|value| !value.is_empty())
                    .map(|value| ((*name).to_owned(), value))
            })
            .collect();

        let date = article.date().map_or_else(
            || {
                article
                    .headers
                    .get_decoded("Date")
                    .unwrap_or_else(|| "(no date)".to_owned())
            },
            |date| date.format(date_format).to_string(),
        );

        // `display_text` rather than `body_text`: the part a reader can read, with
        // `format=flowed` applied, instead of the raw body with its MIME boundaries and
        // base64 in it.
        let mut body: Vec<String> = article.display_text().lines().map(str::to_owned).collect();
        let attachments = article.attachments();

        // An article that is nothing but an attachment would otherwise be a blank pane,
        // which reads as a bug rather than as a fact about the article.
        if body.is_empty() && !attachments.is_empty() {
            body.push("(no text in this article)".to_owned());
        }

        // RFC 5536 §3.2.6: Followup-To names where replies go, and the reserved value
        // `poster` means "mail me instead, do not post".
        let followup_to = article.headers.get_decoded("Followup-To");
        let followup_to_poster = followup_to
            .as_deref()
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("poster"));

        let groups_header = if followup_to_poster {
            None
        } else {
            followup_to.or_else(|| article.headers.get_decoded("Newsgroups"))
        };
        let followup_groups = groups_header
            .unwrap_or_default()
            .split(',')
            .map(|group| group.trim().to_owned())
            .filter(|group| !group.is_empty())
            .collect();

        Self {
            number: article.number,
            subject: article.subject(),
            author: article.author(),
            date,
            headers,
            body,
            attachments,
            signed: article.is_signed(),
            message_id: article.message_id(),
            references: article.references(),
            followup_groups,
            followup_to_poster,
        }
    }
}

/// An article the user has asked to write.
///
/// Carries the text the editor should open with. Everything the reader knows how to fill
/// in — who from, which group, what it answers, what it quotes — is filled in here, so the
/// editor opens on something closer to finished than to blank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeRequest {
    /// The text to open the editor on.
    pub template: String,
    /// What is being written, for the status line: `a new article` or `a follow-up`.
    pub what: &'static str,
}

/// One line of the article pane.
///
/// Flat and threaded views produce the same kind of row; only `depth` and the fold fields
/// differ, so the renderer has one code path and the state machine has one cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArticleRow {
    /// Index into `App::articles`.
    pub index: usize,
    /// How far to indent: 0 for a thread root, and always 0 in the flat view.
    pub depth: usize,
    /// Replies folded away under this row, or 0 if none are.
    pub hidden: usize,
    /// Whether this article has replies at all, folded or not.
    pub has_replies: bool,
    /// Whether this article's replies are folded away.
    pub collapsed: bool,
}

/// The whole interface state.
#[derive(Debug)]
pub struct App {
    /// Which pane has focus.
    pub focus: Pane,
    /// What is covering the layout.
    pub overlay: Overlay,
    /// First visible line of the overlay.
    ///
    /// The key list outgrew a 24-line terminal, which is still the size of a great many
    /// of them. An overlay that silently cuts off the bottom third of the help is worse
    /// than no help.
    pub overlay_scroll: usize,

    /// Every group the server carries.
    pub groups: Vec<GroupRow>,
    /// Index into [`Self::visible_groups`], not into [`Self::groups`].
    pub group_cursor: usize,
    /// The substring groups are filtered by.
    pub filter: String,
    /// Whether the filter is being typed.
    pub editing_filter: bool,
    /// The patterns this server is subscribed to, from the configuration.
    ///
    /// Empty means the whole catalogue. Set by the run loop, like [`Self::from`], because
    /// it belongs to the server rather than to the interface.
    pub subscriptions: Vec<Wildmat>,
    /// Which groups the list on screen was fetched with.
    ///
    /// Kept so that reloading asks for the same thing again: a user who has searched past
    /// their subscriptions and pressed `r` wants the search back, not the subscriptions.
    pub group_scope: GroupScope,

    /// The selected group, once the server has confirmed it.
    pub group: Option<GroupSummary>,
    /// Overview records for the selected group, newest last.
    pub articles: Vec<OverviewRecord>,
    /// Index into [`Self::visible_articles`], not into [`Self::articles`] — the two differ
    /// only while [`Self::unread_only`] is on.
    pub article_cursor: usize,
    /// Whether the article list hides articles that have been read.
    pub unread_only: bool,
    /// Whether the article list is grouped into conversations.
    pub threaded: bool,
    /// The threads [`Self::articles`] falls into, rebuilt whenever the records change.
    ///
    /// Cached rather than computed per draw: threading is the one derived value in here
    /// that is not linear in the number of records, and the article pane is redrawn on
    /// every keystroke.
    threads: Vec<ThreadNode>,
    /// Article numbers whose replies are folded away, by number rather than by index
    /// because a refetch changes indices and a reader's folds should survive one.
    collapsed: HashSet<u64>,

    /// The overview fetch whose records the article list is showing.
    ///
    /// Records arrive in chunks, so the list is assembled over time and something has to
    /// say which fetch is being assembled. `None` means no fetch is outstanding and any
    /// chunk that turns up belongs to one that has been superseded.
    overview_token: Option<FetchToken>,
    /// Mints the next token. Monotonic, never reused within a run.
    next_token: u64,
    /// Chunks accepted for [`Self::overview_token`] so far.
    overview_chunks: usize,
    /// Unparsable overview lines seen so far for [`Self::overview_token`].
    ///
    /// Accumulated rather than reported per chunk: a malformed group would otherwise
    /// produce one message per chunk, and the interesting number is the total.
    overview_skipped: usize,

    /// Which articles have been read, per group.
    ///
    /// The state machine owns it and mutates it; loading and saving belong to the run
    /// loop, because ADR-0008 keeps IO out of here. Everything below therefore reads and
    /// writes this freely and never touches the file.
    pub read: ReadStore,

    /// The article on display.
    pub article: Option<ArticleView>,
    /// First visible body line.
    pub body_scroll: usize,

    /// A one-line description of the connection, for the status bar.
    pub server: String,
    /// Whether the connection is encrypted.
    pub encrypted: bool,
    /// Whether the worker has a connection.
    pub connected: bool,
    /// The current status message.
    pub status: String,
    /// The most recent error, shown until something else happens.
    pub error: Option<String>,
    /// Recent messages, newest first.
    pub messages: VecDeque<String>,

    /// A composing session the run loop has not started yet.
    ///
    /// The state machine decides *what* to compose and pre-fills it; running an editor is
    /// IO and belongs to the run loop, which is the only part that owns the terminal
    /// (ADR-0008). Taken with [`Self::take_compose`].
    pub compose: Option<ComposeRequest>,
    /// The groups the article being posted is addressed to.
    ///
    /// Kept so that the list can be refreshed when it lands in the group on screen: the
    /// article list is a snapshot of the last fetch, so without this the article a person
    /// has just written is the one article missing from it.
    posting_groups: Vec<String>,
    /// The draft file behind the article currently being posted.
    ///
    /// Held until the server accepts it. Anything else — a rejection, a dropped
    /// connection, a crash — leaves the file where it is, which is how a draft survives.
    pub posting: Option<PathBuf>,
    /// Who articles are posted as.
    ///
    /// Per server rather than global, because an identity is: the address you post from on
    /// a private server is rarely the one you post from on a public one. Set by the run
    /// loop from the resolved server, the same way [`Self::cancel`] is.
    pub from: Option<String>,

    /// How many requests are outstanding, for the spinner.
    pub inflight: usize,
    /// Raised to abandon the response the worker is reading.
    ///
    /// Shared with the worker thread. A `Request` would not do: the channel is
    /// first-in-first-out and the worker is inside the request being cancelled, so the
    /// message would arrive only once the wait had ended on its own.
    pub cancel: Cancel,
    /// Spinner phase.
    pub spinner: usize,
    /// Whether the screen needs redrawing.
    pub dirty: bool,
    /// Whether to stop.
    pub should_quit: bool,

    /// How many of a group's newest articles to load.
    initial_articles: u64,
    /// Whether opening an article marks it read.
    mark_read_on_open: bool,
    /// `strftime` format for dates.
    date_format: String,
    /// Height of the article list, so page keys can move by a screenful.
    pub list_height: usize,
    /// Height of the body pane, for the same reason.
    pub body_height: usize,
}

impl App {
    /// A fresh interface, with the group list already requested.
    ///
    /// `read` is this server's read state, already loaded. It is passed in rather than
    /// loaded here so that the state machine stays free of IO, and so that the tests can
    /// start the reader with any reading history they like.
    pub fn new(config: &UiConfig, read: ReadStore) -> Self {
        Self {
            focus: Pane::Groups,
            overlay: Overlay::None,
            overlay_scroll: 0,
            groups: Vec::new(),
            group_cursor: 0,
            filter: String::new(),
            editing_filter: false,
            subscriptions: Vec::new(),
            group_scope: GroupScope::everything(),
            group: None,
            articles: Vec::new(),
            article_cursor: 0,
            unread_only: config.unread_only,
            threaded: config.threaded,
            threads: Vec::new(),
            collapsed: HashSet::new(),
            overview_token: None,
            next_token: 0,
            overview_chunks: 0,
            overview_skipped: 0,
            read,
            article: None,
            body_scroll: 0,
            server: String::new(),
            encrypted: false,
            connected: false,
            status: "connecting…".to_owned(),
            error: None,
            messages: VecDeque::new(),
            compose: None,
            posting: None,
            posting_groups: Vec::new(),
            from: None,
            inflight: 0,
            cancel: Cancel::new(),
            spinner: 0,
            dirty: true,
            should_quit: false,
            initial_articles: config.initial_articles,
            mark_read_on_open: config.mark_read_on_open,
            date_format: config.date_format.clone(),
            list_height: 20,
            body_height: 20,
        }
    }

    /// The requests to send when the interface starts.
    pub fn initial_requests(&mut self) -> Vec<Request> {
        self.inflight += 1;
        self.group_scope = GroupScope::Subscribed(self.subscriptions.clone());
        vec![Request::LoadGroups {
            scope: self.group_scope.clone(),
        }]
    }

    /// Asks the server for groups matching whatever is in the filter box.
    ///
    /// The deliberate counterpart to `/`: that narrows what has been fetched, this fetches
    /// something else. It costs a round trip and, with no filter text, the server's whole
    /// catalogue — so it is bound to a key of its own and says what it is doing, rather
    /// than happening whenever a filter finds nothing.
    fn search_groups(&mut self) -> Vec<Request> {
        let scope = if self.filter.trim().is_empty() {
            GroupScope::everything()
        } else {
            match Wildmat::containing(self.filter.trim()) {
                Ok(pattern) => GroupScope::Search(pattern),
                // A wildmat is printable ASCII without spaces; a filter box is not. Saying
                // which text cannot be sent is more use than a bare refusal.
                Err(_) => {
                    self.status = format!(
                        "{:?} cannot be sent as a pattern: no spaces or accents",
                        self.filter.trim()
                    );
                    return Vec::new();
                }
            }
        };

        self.status = format!("asking the server for {}…", scope.describe());
        self.inflight += 1;
        self.group_scope = scope.clone();
        vec![Request::LoadGroups { scope }]
    }

    /// Indices into [`Self::groups`] that pass the filter, in order.
    pub fn visible_groups(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            return (0..self.groups.len()).collect();
        }

        let needle = self.filter.to_lowercase();
        self.groups
            .iter()
            .enumerate()
            .filter(|(_, group)| {
                group.name.as_str().to_lowercase().contains(&needle)
                    || group
                        .description
                        .as_deref()
                        .is_some_and(|text| text.to_lowercase().contains(&needle))
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// The group under the cursor.
    pub fn selected_group(&self) -> Option<&GroupRow> {
        let visible = self.visible_groups();
        visible
            .get(self.group_cursor)
            .and_then(|index| self.groups.get(*index))
    }

    /// Indices into [`Self::articles`] that the article list shows, in order.
    ///
    /// Everything unless [`Self::unread_only`] is on, in which case only the unread, and
    /// in thread order when [`Self::threaded`] is on. The same shape as
    /// [`Self::visible_groups`], deliberately: one filtering pattern in the interface
    /// rather than two, and one thing the cursor is an index into.
    pub fn visible_articles(&self) -> Vec<usize> {
        self.article_rows()
            .into_iter()
            .map(|row| row.index)
            .collect()
    }

    /// The article list as rows to draw, in order.
    ///
    /// The threaded and flat views differ only here; everything else — the cursor, the
    /// unread filter, marking read — works off the same list of indices either way.
    pub fn article_rows(&self) -> Vec<ArticleRow> {
        if !self.threaded {
            return self
                .articles
                .iter()
                .enumerate()
                .filter(|(_, record)| !self.unread_only || self.is_unread(record.number))
                .map(|(index, _)| ArticleRow {
                    index,
                    depth: 0,
                    hidden: 0,
                    has_replies: false,
                    collapsed: false,
                })
                .collect();
        }

        let mut rows = Vec::new();
        for root in &self.threads {
            self.push_rows(root, 0, &mut rows);
        }
        rows
    }

    /// Flattens one thread into rows, depth first.
    ///
    /// Two rules worth stating, because both are visible:
    ///
    /// - **Depth is the node's real depth in the thread**, whether or not its ancestors
    ///   are drawn. A reply to a read article under the unread filter therefore keeps its
    ///   indent, and turning the filter on and off does not slide the list sideways.
    /// - **A fold only applies to a row that is drawn.** If the unread filter hides the
    ///   article you collapsed, its unread replies are shown anyway: the filter's job is
    ///   to show you what you have not read, and a fold must not be able to hide it.
    fn push_rows(&self, node: &ThreadNode, depth: usize, rows: &mut Vec<ArticleRow>) {
        let drawn = node
            .article
            .and_then(|index| {
                self.articles
                    .get(index)
                    .map(|record| (index, record.number))
            })
            .filter(|(_, number)| !self.unread_only || self.is_unread(*number));

        if let Some((index, number)) = drawn {
            let collapsed = self.collapsed.contains(&number);
            let hidden = if collapsed {
                node.children.iter().map(ThreadNode::len).sum()
            } else {
                0
            };

            rows.push(ArticleRow {
                index,
                depth,
                hidden,
                has_replies: !node.children.is_empty(),
                collapsed,
            });

            if collapsed {
                return;
            }
        }

        for child in &node.children {
            self.push_rows(child, depth + 1, rows);
        }
    }

    /// Rebuilds the thread tree from the records on display.
    ///
    /// Called whenever [`Self::articles`] changes, which is the only thing the tree
    /// depends on — collapse state is applied at draw time, so folds survive a refetch.
    fn rebuild_threads(&mut self) {
        self.threads = if self.threaded {
            nntp_proto::thread(&self.articles)
        } else {
            Vec::new()
        };
    }

    /// The overview record under the cursor.
    pub fn selected_article(&self) -> Option<&OverviewRecord> {
        let visible = self.visible_articles();
        visible
            .get(self.article_cursor)
            .and_then(|index| self.articles.get(*index))
    }

    /// Whether article `number` in the selected group is unread.
    ///
    /// An article in no group at all counts as unread: there is nowhere to have recorded
    /// otherwise, and calling something read on no evidence is the worse mistake.
    pub fn is_unread(&self, number: u64) -> bool {
        match &self.group {
            Some(summary) => !self.read.read_set(summary.name.as_str()).contains(number),
            None => true,
        }
    }

    /// How many of `group`'s numbers are unread, as an upper bound.
    ///
    /// Built from the watermarks, so it counts numbers left by cancelled and expired
    /// articles as unread — the same reason the group list writes its totals as `≤n`.
    pub fn unread_in(&self, group: &GroupRow) -> u64 {
        if group.is_empty() {
            return 0;
        }
        self.read
            .read_set(group.name.as_str())
            .unread_in(group.low, group.high)
    }

    /// Advances the spinner. Called on each idle tick.
    pub fn tick(&mut self) {
        if self.inflight > 0 {
            self.spinner = self.spinner.wrapping_add(1);
            self.dirty = true;
        }
    }

    /// The spinner character, or a space when nothing is outstanding.
    pub fn spinner_frame(&self) -> char {
        const FRAMES: [char; 4] = ['|', '/', '-', '\\'];
        if self.inflight == 0 {
            return ' ';
        }
        // `get` rather than indexing: the modulo makes it in-range, but the workspace
        // forbids indexing that a reader has to reason about to trust.
        FRAMES
            .get(self.spinner % FRAMES.len())
            .copied()
            .unwrap_or(' ')
    }

    /// Records a message, for the message pane.
    ///
    /// Public because the run loop has things to report that the state machine cannot
    /// know — a read-state file it could not parse, for one.
    pub fn note(&mut self, message: impl Into<String>) {
        let message = message.into();
        tracing::debug!(%message, "ui message");
        self.messages.push_front(message);
        self.messages.truncate(LOG_CAPACITY);
        self.dirty = true;
    }

    /// Applies an event from the worker, returning any requests it produced.
    ///
    /// Almost every event produces none: the interface asks, the worker answers. The
    /// exception is posting, where the answer changes the group on screen and the reader
    /// has to go and look again — see [`Event::Posted`].
    pub fn on_event(&mut self, event: Event) -> Vec<Request> {
        self.dirty = true;

        match event {
            Event::Connected {
                server,
                greeting,
                encrypted,
            } => {
                self.connected = true;
                self.server = server;
                self.encrypted = encrypted;
                self.status = greeting.clone();
                self.note(format!("connected: {greeting}"));
            }

            Event::Groups {
                rows,
                scope,
                filtered_locally,
            } => {
                self.inflight = self.inflight.saturating_sub(1);
                // An empty answer to a narrowed fetch is the one case where a count on its
                // own misleads: nothing is wrong with the server, the patterns simply match
                // nothing it carries, and saying so has to come with the way out.
                self.status = match (&scope, rows.len()) {
                    (GroupScope::Subscribed(patterns), 0) if !patterns.is_empty() => {
                        "no group matches your subscriptions; press S to search the server"
                            .to_owned()
                    }
                    (GroupScope::Subscribed(patterns), count) if !patterns.is_empty() => {
                        format!("{count} subscribed groups")
                    }
                    (GroupScope::Search(pattern), 0) => {
                        format!("no group matches {}", pattern.as_str())
                    }
                    (GroupScope::Search(pattern), count) => {
                        format!("{count} groups matching {}", pattern.as_str())
                    }
                    (GroupScope::Subscribed(_), count) => format!("{count} groups"),
                };
                if filtered_locally {
                    self.note(
                        "this server will not narrow LIST itself, so the whole group list \
                         was fetched and filtered here"
                            .to_owned(),
                    );
                }
                self.groups = rows;
                self.group_scope = scope;
                self.group_cursor = 0;
            }

            Event::GroupOpened(summary) => {
                // Numbers below the low watermark are gone for good, so read state about
                // them can never be useful again and would otherwise accumulate in the
                // store forever. This is the only moment the watermarks are known.
                self.read.forget_expired(summary.name.as_str(), summary.low);

                let unread = self
                    .read
                    .read_set(summary.name.as_str())
                    .unread_in(summary.low, summary.high);
                self.status = format!(
                    "{}: \u{2264}{} unread of {}, {}..{}",
                    summary.name, unread, summary.estimated_count, summary.low, summary.high
                );
                self.group = Some(*summary);
            }

            Event::OverviewChunk {
                group,
                token,
                records,
                skipped,
            } => {
                // A chunk of a superseded fetch is dropped without a word: the fetch that
                // replaced it says so once, when its own completion arrives, and one note
                // per chunk would bury everything else in the message pane.
                if !self.accepts_overview(&group, token) {
                    return Vec::new();
                }

                self.overview_skipped += skipped;
                self.absorb_overview(records);

                // Only when the first records land, so that a user who has gone back to
                // the group list mid-fetch is not dragged into the article pane by each
                // chunk that arrives.
                if self.overview_chunks == 0 {
                    self.focus = Pane::Articles;
                }
                self.overview_chunks += 1;

                self.status = format!("{group}: {} articles listed…", self.articles.len());
            }

            Event::OverviewComplete { group, token } => {
                // The request was counted when it was issued, so its completion brings the
                // count down whether or not anything is still interested in the records.
                self.inflight = self.inflight.saturating_sub(1);

                if !self.accepts_overview(&group, token) {
                    // Two different situations, and saying "discarded" for both reads as
                    // data loss for the one where nothing was lost. A superseded fetch is
                    // routine now that posting reloads the group: post twice and the first
                    // reload is overtaken by the second.
                    let showing = self.group.as_ref().map(|summary| &summary.name);
                    if showing == Some(&group) {
                        self.note(format!("{group}: a reload was overtaken by a newer one"));
                    } else {
                        self.note(format!(
                            "{group}: an overview reply arrived after you had moved on"
                        ));
                    }
                    return Vec::new();
                }

                if self.overview_skipped > 0 {
                    self.note(format!(
                        "{group}: {} overview line(s) could not be parsed",
                        self.overview_skipped
                    ));
                }

                self.focus = Pane::Articles;
                self.overview_token = None;

                let unread = self
                    .articles
                    .iter()
                    .filter(|record| self.is_unread(record.number))
                    .count();
                self.status = format!(
                    "{}: {} articles listed, {unread} unread",
                    group,
                    self.articles.len()
                );
            }

            Event::Article(article) => {
                self.inflight = self.inflight.saturating_sub(1);
                let view = ArticleView::new(&article, &self.date_format);

                // Reading an article is what makes it read, unless the user has asked to
                // decide that themselves. The number comes from the article rather than
                // from the cursor: a fetch by message-id has no cursor position, and the
                // cursor may have moved while the fetch was in flight.
                if self.mark_read_on_open
                    && let Some(number) = view.number
                    && let Some(group) = self.group.as_ref().map(|summary| summary.name.to_string())
                {
                    self.read.mark_read(&group, number);
                }

                self.status = format!("{} — {}", view.date, view.subject);
                self.article = Some(view);
                self.body_scroll = 0;
                self.focus = Pane::Body;
            }

            Event::Posted { text } => {
                self.inflight = self.inflight.saturating_sub(1);
                // The draft file goes only now: the article exists somewhere other than
                // this machine, so the copy here is the one thing nobody needs any more.
                if let Some(path) = self.posting.take() {
                    crate::compose::discard(&path);
                }
                self.status = format!("posted: {text}");
                self.note(format!("posted: {text}"));

                let groups = std::mem::take(&mut self.posting_groups);
                // The article list is a snapshot of the last fetch, so the article just
                // written is the one article missing from it. Refetching is the difference
                // between "it says it posted" and seeing it in the group.
                if let Some(requests) = self.refresh_after_posting(&groups) {
                    return requests;
                }
            }

            Event::Progress(message) => self.status = message,

            Event::Cancelled { context } => {
                self.inflight = self.inflight.saturating_sub(1);
                // A status line, not the error line: the user asked for this. The note
                // about the connection is there because the next request will visibly
                // reconnect, and a reconnection nobody explained looks like a fault.
                self.status = format!("{context} cancelled");
                self.report_unsent_draft();
                self.note(format!(
                    "{context} cancelled; the connection was dropped and will be remade \
                     on the next request"
                ));
            }

            Event::Failed { context, message } => {
                self.inflight = self.inflight.saturating_sub(1);
                let text = format!("{context}: {message}");
                self.error = Some(text.clone());
                self.note(text);
                self.report_unsent_draft();
            }

            Event::Disconnected(reason) => {
                self.connected = false;
                self.inflight = 0;
                self.error = Some(format!("disconnected: {reason}"));
                self.note(format!("disconnected: {reason}"));
                self.report_unsent_draft();
            }

            Event::Stopped => {
                self.connected = false;
                self.inflight = 0;
                self.note("the worker stopped");
            }
        }

        Vec::new()
    }

    /// Refetches the group on screen when an article has just been posted to it.
    ///
    /// `None` when the article went somewhere else — a crosspost to groups the reader is
    /// not looking at changes nothing on screen, and refetching would be a round trip
    /// nobody asked for.
    fn refresh_after_posting(&mut self, groups: &[String]) -> Option<Vec<Request>> {
        let group = self.group.as_ref()?.name.clone();
        let name = group.to_string();
        if !groups.contains(&name) {
            return None;
        }

        self.status = format!("posted; reloading {name}…");
        self.inflight += 1;
        let token = self.begin_overview_fetch();

        // `OpenGroup` rather than a range: the article just posted is *past* the high
        // watermark this reader knows about, so a range built from what is on screen
        // cannot include it. Selecting the group again is what asks the server for the
        // watermarks it has now.
        Some(vec![Request::OpenGroup {
            group,
            count: self.initial_articles,
            token,
        }])
    }

    /// Applies a key press, returning any requests it produced.
    pub fn on_key(&mut self, key: KeyEvent) -> Vec<Request> {
        self.dirty = true;
        // Any keystroke means the user has seen the error.
        self.error = None;

        if self.editing_filter {
            return self.filter_key(key);
        }

        // Ctrl-C quits from anywhere, including an overlay.
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            return self.quit();
        }

        if self.overlay != Overlay::None {
            return self.overlay_key(key);
        }

        match key.code {
            KeyCode::Char('q') => return self.quit(),
            KeyCode::Char('?') | KeyCode::F(1) => self.overlay = Overlay::Help,
            KeyCode::Char('m') => self.overlay = Overlay::Messages,

            KeyCode::Tab => self.focus = self.focus.next(),
            KeyCode::BackTab => self.focus = self.focus.previous(),
            KeyCode::Left | KeyCode::Char('h') => self.focus = self.focus.previous(),
            KeyCode::Right | KeyCode::Char('l') => self.focus = self.focus.next(),

            KeyCode::Char('/') => {
                self.editing_filter = true;
                self.focus = Pane::Groups;
            }
            KeyCode::Esc => {
                // Stopping a long fetch is what a user reaches for Esc to do, so it comes
                // first — but only while something is actually outstanding, otherwise Esc
                // would stop clearing filters. Esc *while typing* a filter is handled in
                // `filter_key` and still belongs to the filter.
                if self.inflight > 0 {
                    self.request_cancellation();
                } else if self.filter.is_empty() {
                    self.focus = Pane::Groups;
                } else {
                    self.filter.clear();
                    self.group_cursor = 0;
                }
            }

            KeyCode::Char('r') => return self.refresh(),
            KeyCode::Enter => return self.activate(),

            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1),
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_cursor(self.page() as isize);
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_cursor(-(self.page() as isize));
            }
            KeyCode::PageDown => self.move_cursor(self.page() as isize),
            KeyCode::PageUp => self.move_cursor(-(self.page() as isize)),
            KeyCode::Home | KeyCode::Char('g') => self.move_to_start(),
            KeyCode::End | KeyCode::Char('G') => self.move_to_end(),

            KeyCode::Char('n') => return self.step_article(1),
            KeyCode::Char('p') => return self.step_article(-1),

            KeyCode::Char('u') => self.toggle_unread_only(),
            KeyCode::Char('M') => self.toggle_read_under_cursor(),
            KeyCode::Char('c') => self.catch_up(),
            KeyCode::Char('t') => self.toggle_threading(),
            KeyCode::Char('z') => self.toggle_fold_under_cursor(),
            KeyCode::Char('w') => self.start_article(),
            KeyCode::Char('f') => self.start_follow_up(),
            KeyCode::Char('S') => return self.search_groups(),

            _ => self.dirty = false,
        }

        Vec::new()
    }

    /// Records the size of the panes, so page keys move by a screenful.
    pub fn set_pane_heights(&mut self, list: usize, body: usize) {
        self.list_height = list.max(1);
        self.body_height = body.max(1);
    }

    fn page(&self) -> usize {
        match self.focus {
            Pane::Body => self.body_height.saturating_sub(1).max(1),
            _ => self.list_height.saturating_sub(1).max(1),
        }
    }

    fn quit(&mut self) -> Vec<Request> {
        self.should_quit = true;
        vec![Request::Shutdown]
    }

    fn overlay_key(&mut self, key: KeyEvent) -> Vec<Request> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => self.close_overlay(),
            KeyCode::Char('?') | KeyCode::F(1) => {
                if self.overlay == Overlay::Help {
                    self.close_overlay();
                } else {
                    self.overlay = Overlay::Help;
                    self.overlay_scroll = 0;
                }
            }
            KeyCode::Char('m') => {
                if self.overlay == Overlay::Messages {
                    self.close_overlay();
                } else {
                    self.overlay = Overlay::Messages;
                    self.overlay_scroll = 0;
                }
            }

            // The overlay scrolls with the same keys as everything else, because a list
            // that is taller than the terminal and does not scroll is a list with a
            // hidden bottom half.
            KeyCode::Down | KeyCode::Char('j') => self.scroll_overlay(1),
            KeyCode::Up | KeyCode::Char('k') => self.scroll_overlay(-1),
            KeyCode::PageDown => self.scroll_overlay(self.overlay_page()),
            KeyCode::PageUp => self.scroll_overlay(-self.overlay_page()),
            KeyCode::Home | KeyCode::Char('g') => self.overlay_scroll = 0,
            KeyCode::End | KeyCode::Char('G') => self.overlay_scroll = usize::MAX,

            _ => self.dirty = false,
        }
        Vec::new()
    }

    fn close_overlay(&mut self) {
        self.overlay = Overlay::None;
        self.overlay_scroll = 0;
    }

    fn overlay_page(&self) -> isize {
        // The overlay is a little shorter than the pane it covers; one line of overlap
        // keeps a reader's place across a page.
        self.body_height.saturating_sub(2).max(1) as isize
    }

    /// Scrolls the overlay, clamped by the renderer rather than here.
    ///
    /// The state machine does not know how many lines the overlay has — that depends on
    /// wrapping, which depends on the width. `usize::MAX` therefore means "the end", and
    /// [`crate::tui::ui`] brings it back inside the text it just laid out.
    fn scroll_overlay(&mut self, delta: isize) {
        if delta < 0 {
            self.overlay_scroll = self.overlay_scroll.saturating_sub(delta.unsigned_abs());
        } else {
            self.overlay_scroll = self.overlay_scroll.saturating_add(delta.unsigned_abs());
        }
    }

    /// Reports where the overlay actually ended up, once the renderer has clamped it.
    ///
    /// The only thing the renderer tells the state machine, and it exists so that `End`
    /// does not leave the scroll position at `usize::MAX` for the next key press to
    /// subtract from.
    pub fn set_overlay_scroll(&mut self, scroll: usize) {
        self.overlay_scroll = scroll;
    }

    fn filter_key(&mut self, key: KeyEvent) -> Vec<Request> {
        match key.code {
            KeyCode::Esc => {
                self.editing_filter = false;
                self.filter.clear();
                self.group_cursor = 0;
            }
            KeyCode::Enter => self.editing_filter = false,
            KeyCode::Backspace => {
                self.filter.pop();
                self.group_cursor = 0;
            }

            // Moving through what the filter left, without having to stop typing first.
            // That is the whole point of an incremental filter, and leaving it out meant
            // the arrow keys did nothing at all while the filter was open.
            //
            // The arrows and the page keys only: `j` and `k` are filter text here, and a
            // filter that cannot contain the letter `j` would be a worse bug than the one
            // this fixes.
            KeyCode::Down => self.move_group_cursor(1),
            KeyCode::Up => self.move_group_cursor(-1),
            KeyCode::PageDown => self.move_group_cursor(self.page() as isize),
            KeyCode::PageUp => self.move_group_cursor(-(self.page() as isize)),
            KeyCode::Home => self.group_cursor = 0,
            KeyCode::End => {
                self.group_cursor = self.visible_groups().len().saturating_sub(1);
            }

            KeyCode::Char(character) => {
                self.filter.push(character);
                self.group_cursor = 0;
            }
            _ => self.dirty = false,
        }
        Vec::new()
    }

    /// Asks the worker to abandon whatever it is reading.
    ///
    /// Raises the shared flag and says so. The worker answers with
    /// [`Event::Cancelled`] once it notices — within one line of a response that is still
    /// arriving — which is when `inflight` comes down. Pressing it twice is harmless.
    pub fn request_cancellation(&mut self) {
        if self.inflight == 0 {
            self.status = "nothing to cancel".to_owned();
            return;
        }

        self.cancel.cancel();
        self.status = "cancelling…".to_owned();
    }

    /// Moves the group cursor within the filtered list.
    ///
    /// Separate from [`Self::move_cursor`], which dispatches on the focused pane: while
    /// the filter is open the target is always the group list, and a worker event can move
    /// the focus — an overview reply arriving mid-typing sets it to the article pane — so
    /// dispatching on focus here would send the arrow keys somewhere the user is not
    /// looking.
    fn move_group_cursor(&mut self, delta: isize) {
        let count = self.visible_groups().len();
        self.group_cursor = shift(self.group_cursor, delta, count);
    }

    /// Enter: open the group under the cursor, or the article under the cursor.
    fn activate(&mut self) -> Vec<Request> {
        match self.focus {
            Pane::Groups => {
                let Some(group) = self.selected_group().cloned() else {
                    return Vec::new();
                };

                if group.is_empty() {
                    self.status = format!("{} is empty", group.name);
                    self.articles.clear();
                    self.threads.clear();
                    self.collapsed.clear();
                    self.article = None;
                    return Vec::new();
                }

                self.articles.clear();
                self.threads.clear();
                // Folds belong to the group they were made in: article numbers mean
                // something else in the next one.
                self.collapsed.clear();
                self.article = None;
                self.article_cursor = 0;
                self.status = format!("opening {}…", group.name);
                self.inflight += 1;
                let token = self.begin_overview_fetch();

                vec![Request::OpenGroup {
                    group: group.name,
                    count: self.initial_articles,
                    token,
                }]
            }

            Pane::Articles => self.load_selected_article(),

            // In the body pane, Enter scrolls: there is nothing to open.
            Pane::Body => {
                self.scroll_body(1);
                Vec::new()
            }
        }
    }

    /// `u`: show only unread articles, or everything again.
    ///
    /// The article under the cursor is kept under the cursor where it still passes the
    /// filter. Toggling a filter and finding yourself somewhere else in the list is the
    /// kind of small betrayal that makes a reader feel unpredictable.
    fn toggle_unread_only(&mut self) {
        let anchor = self.selected_article().map(|record| record.number);
        self.unread_only = !self.unread_only;

        let visible = self.visible_articles();
        self.article_cursor = anchor
            .and_then(|number| {
                visible.iter().position(|index| {
                    self.articles
                        .get(*index)
                        .is_some_and(|r| r.number == number)
                })
            })
            .unwrap_or_else(|| visible.len().saturating_sub(1));

        self.status = if self.unread_only {
            format!(
                "showing {} unread of {}",
                visible.len(),
                self.articles.len()
            )
        } else {
            format!("showing all {} articles", self.articles.len())
        };
    }

    /// `w`: write a new article in the selected group.
    fn start_article(&mut self) {
        let Some(from) = self.posting_identity() else {
            return;
        };
        let Some(group) = self.group.as_ref().map(|summary| summary.name.to_string()) else {
            self.status = "select a group first".to_owned();
            return;
        };

        let mut draft = nntp_proto::Draft::default();
        draft.set_header("From", from);
        draft.set_header("Newsgroups", group.clone());
        draft.set_header("Subject", "");

        self.compose = Some(ComposeRequest {
            template: template_for(&draft),
            what: "a new article",
        });
        self.status = format!("composing an article for {group}…");
    }

    /// `f`: follow up to the article on screen.
    fn start_follow_up(&mut self) {
        let Some(from) = self.posting_identity() else {
            return;
        };
        let Some(article) = self.article.as_ref() else {
            self.status = "open an article to follow up to".to_owned();
            return;
        };

        if article.followup_to_poster {
            // RFC 5536 §3.2.6. Posting anyway would put the reply exactly where the author
            // asked it not to go, and this reader cannot send the mail that was asked for.
            self.status = "Followup-To: poster — the author asked for a mail reply".to_owned();
            self.note(
                "this article asks for replies by mail rather than to the group; \
                 nothing was composed"
                    .to_owned(),
            );
            return;
        }

        let groups = if article.followup_groups.is_empty() {
            self.group
                .as_ref()
                .map(|summary| vec![summary.name.to_string()])
                .unwrap_or_default()
        } else {
            article.followup_groups.clone()
        };

        if groups.is_empty() {
            self.status = "no group to follow up in".to_owned();
            return;
        }

        let draft = nntp_proto::Draft::follow_up(
            &nntp_proto::FollowUp {
                message_id: article.message_id.as_ref(),
                references: &article.references,
                subject: &article.subject,
                author: &article.author,
                newsgroups: groups.clone(),
                body: &article.body.join("\n"),
            },
            &from,
        );

        self.compose = Some(ComposeRequest {
            template: template_for(&draft),
            what: "a follow-up",
        });
        self.status = format!("composing a follow-up to {}…", groups.join(", "));
    }

    /// Who to post as, or a message saying why nothing can be posted.
    fn posting_identity(&mut self) -> Option<String> {
        match self.from.as_ref().filter(|from| !from.trim().is_empty()) {
            Some(from) => Some(from.clone()),
            None => {
                // Naming the key is the difference between a dead end and a fix: there is
                // no sensible default for somebody's name and address.
                self.status = "set `from` for this server to post".to_owned();
                self.note(
                    "posting needs an identity: add `from = \"Your Name <you@example.org>\"` \
                     under this server in the configuration file"
                        .to_owned(),
                );
                None
            }
        }
    }

    /// Takes the composing session the run loop should start, if there is one.
    pub fn take_compose(&mut self) -> Option<ComposeRequest> {
        self.compose.take()
    }

    /// Applies whatever came back from the editor.
    ///
    /// `None` means the editor was abandoned or could not be run; the caller reports why.
    /// A draft with problems goes back to the user as messages rather than to the server
    /// as a `441`, and the draft file stays where it is.
    pub fn on_composed(&mut self, composed: Option<(String, PathBuf)>) -> Vec<Request> {
        self.dirty = true;

        let Some((text, path)) = composed else {
            self.status = "nothing was posted".to_owned();
            return Vec::new();
        };

        let draft = nntp_proto::Draft::parse(&text);
        let problems = draft.problems();
        if !problems.is_empty() {
            self.status = format!("the article was not sent: {} problem(s)", problems.len());
            for problem in &problems {
                self.note(format!("draft: {problem}"));
            }
            self.note(format!("the draft is at {}", path.display()));
            return Vec::new();
        }

        self.posting = Some(path);
        self.posting_groups = draft
            .newsgroups()
            .into_iter()
            .map(ToOwned::to_owned)
            .collect();
        self.status = format!("posting to {}…", draft.newsgroups().join(", "));
        self.inflight += 1;

        vec![Request::Post {
            draft: Box::new(draft),
        }]
    }

    /// Says where the draft is, when a posting did not go through.
    ///
    /// The file is left alone — it is only removed once the server has taken the article —
    /// so this is the part that makes it findable. Reported once: the path repeated on
    /// every subsequent failure would bury the reason.
    fn report_unsent_draft(&mut self) {
        if let Some(path) = self.posting.take() {
            self.note(format!(
                "the article was not posted; the draft is at {}",
                path.display()
            ));
        }
    }

    /// `t`: group the article list into conversations, or show it flat again.
    ///
    /// The article under the cursor stays under the cursor, because the two views order
    /// the same articles differently and landing somewhere else on a key press meant to
    /// change the *shape* of the list would be disorienting.
    fn toggle_threading(&mut self) {
        let anchor = self.selected_article().map(|record| record.number);
        self.threaded = !self.threaded;
        self.rebuild_threads();

        self.article_cursor = anchor
            .and_then(|number| self.row_of(number))
            .unwrap_or_else(|| self.visible_articles().len().saturating_sub(1));

        self.status = if self.threaded {
            format!("{} threads", self.threads.len())
        } else {
            format!("showing {} articles flat", self.articles.len())
        };
    }

    /// `z`: fold the replies under the cursor away, or bring them back.
    fn toggle_fold_under_cursor(&mut self) {
        if !self.threaded {
            self.status = "threading is off (t)".to_owned();
            return;
        }

        let Some(row) = self.article_rows().into_iter().nth(self.article_cursor) else {
            return;
        };
        let Some(number) = self.articles.get(row.index).map(|record| record.number) else {
            return;
        };

        if !row.has_replies {
            self.status = format!("article {number} has no replies");
            return;
        }

        if self.collapsed.remove(&number) {
            self.status = format!("article {number}: replies shown");
        } else {
            self.collapsed.insert(number);
            // Counted after folding, not before: the row only reports what it is hiding
            // once it is hiding it, and guessing from the rows below would get a nested
            // fold wrong.
            let hidden = self
                .article_rows()
                .into_iter()
                .find(|candidate| candidate.index == row.index)
                .map_or(0, |candidate| candidate.hidden);
            self.status = format!(
                "article {number}: {} folded",
                plural(hidden, "reply", "replies")
            );
        }

        // Folding removes rows below the cursor, so the cursor itself cannot move — but
        // unfolding can put the list somewhere the cursor is no longer inside.
        self.article_cursor = self.row_of(number).unwrap_or(self.article_cursor);
        self.clamp_article_cursor();
    }

    /// Which row an article number is on, if it is on one.
    fn row_of(&self, number: u64) -> Option<usize> {
        self.article_rows().into_iter().position(|row| {
            self.articles
                .get(row.index)
                .is_some_and(|record| record.number == number)
        })
    }

    /// `M`: mark the article under the cursor read, or unread if it already was read.
    fn toggle_read_under_cursor(&mut self) {
        let Some(number) = self.selected_article().map(|record| record.number) else {
            return;
        };
        let Some(group) = self.group.as_ref().map(|summary| summary.name.to_string()) else {
            return;
        };

        if self.is_unread(number) {
            self.read.mark_read(&group, number);
            self.status = format!("article {number} marked read");
        } else {
            self.read.mark_unread(&group, number);
            self.status = format!("article {number} marked unread");
        }

        // With the unread filter on, marking the article under the cursor read removes it
        // from the list, so the cursor has to be brought back inside it.
        self.clamp_article_cursor();
    }

    /// `c`: catch up — mark the whole selected group read.
    ///
    /// The range comes from the group's watermarks rather than from the article list,
    /// because the list holds only the newest few hundred records and "catch up" that
    /// leaves older articles unread would not be catching up.
    fn catch_up(&mut self) {
        let Some(summary) = &self.group else {
            self.status = "no group selected".to_owned();
            return;
        };
        let name = summary.name.to_string();
        let Some((low, high)) = summary.range() else {
            self.status = format!("{name} is empty");
            return;
        };

        self.read.mark_range_read(&name, low, high);
        self.status = format!("{name}: caught up to article {high}");
        self.clamp_article_cursor();
    }

    /// Starts a new overview fetch and returns the token that identifies it.
    ///
    /// Everything the previous fetch was accumulating is dropped here rather than when
    /// its last chunk arrives, because the user has already moved on and its remaining
    /// chunks are on their way.
    ///
    /// Public because anything that builds a [`Request::OpenGroup`] or
    /// [`Request::LoadOverview`] has to take the token from here. Records carrying a token
    /// this has not issued are treated as belonging to a superseded fetch and dropped,
    /// which is the whole point — but it does mean a request minted without one goes
    /// nowhere visible.
    pub fn begin_overview_fetch(&mut self) -> FetchToken {
        self.next_token += 1;
        let token = FetchToken::new(self.next_token);
        self.overview_token = Some(token);
        self.overview_chunks = 0;
        self.overview_skipped = 0;
        token
    }

    /// Whether records from this fetch are still wanted.
    ///
    /// Two conditions, and both matter. The token catches a superseded fetch of the group
    /// that is still on screen — a refresh while the previous one is arriving — which the
    /// group name cannot see. The group name catches records for a group the user has
    /// left, which is the older guarantee and stays true even if a token were ever reused.
    fn accepts_overview(&self, group: &nntp_proto::GroupName, token: FetchToken) -> bool {
        self.overview_token == Some(token)
            && self.group.as_ref().map(|summary| &summary.name) == Some(group)
    }

    /// Merges a chunk of overview records into the list on display.
    ///
    /// The cursor is the delicate part. A cursor sitting on the newest article follows the
    /// newest, because that is where a reader who has not moved expects to stay; a cursor
    /// the user has moved stays on the article it is on, even though arriving records
    /// change its index. Anything else would move somebody's place while they are reading.
    fn absorb_overview(&mut self, mut records: Vec<OverviewRecord>) {
        if records.is_empty() {
            return;
        }

        let visible = self.visible_articles();
        // An empty list counts as pinned: the first chunk should land on its newest
        // article rather than on its oldest.
        let pinned_to_newest = self.article_cursor + 1 >= visible.len();
        let anchor = self.selected_article().map(|record| record.number);

        match (self.articles.first(), records.last()) {
            (None, _) => self.articles = records,

            // The common case. Chunks arrive newest first, so each one is entirely older
            // than everything already held and goes straight on the front — no sort, no
            // comparison per record.
            (Some(oldest_held), Some(newest_arriving))
                if newest_arriving.number < oldest_held.number =>
            {
                records.append(&mut self.articles);
                self.articles = records;
            }

            // Overlapping or out of order: a refresh across a range already on screen.
            // The arriving copy wins, since it is the more recent view of the same
            // article — `sort_by_key` is stable and the arriving records are placed
            // first, so `dedup_by_key`, which keeps the first of each run, keeps them.
            _ => {
                records.append(&mut self.articles);
                records.sort_by_key(|record| record.number);
                records.dedup_by_key(|record| record.number);
                self.articles = records;
            }
        }

        self.rebuild_threads();

        let visible = self.visible_articles();
        self.article_cursor = if pinned_to_newest {
            visible.len().saturating_sub(1)
        } else {
            anchor
                .and_then(|number| {
                    visible.iter().position(|index| {
                        self.articles
                            .get(*index)
                            .is_some_and(|record| record.number == number)
                    })
                })
                .unwrap_or_else(|| self.article_cursor.min(visible.len().saturating_sub(1)))
        };
    }

    /// Brings the article cursor back inside the visible list.
    fn clamp_article_cursor(&mut self) {
        let count = self.visible_articles().len();
        self.article_cursor = self.article_cursor.min(count.saturating_sub(1));
    }

    fn load_selected_article(&mut self) -> Vec<Request> {
        let Some(record) = self.selected_article() else {
            return Vec::new();
        };
        let number = record.number;
        let Some(group) = self.group.as_ref().map(|summary| summary.name.clone()) else {
            return Vec::new();
        };

        self.status = format!("fetching article {number}…");
        self.inflight += 1;

        vec![Request::LoadArticle {
            group: Some(group),
            spec: ArticleSpec::Number(number),
        }]
    }

    /// `n` / `p`: move to the next or previous article and open it.
    fn step_article(&mut self, delta: isize) -> Vec<Request> {
        let count = self.visible_articles().len();
        if count == 0 {
            return Vec::new();
        }

        let next = self.article_cursor as isize + delta;
        if next < 0 || next as usize >= count {
            self.status = if delta > 0 {
                "already at the newest article".to_owned()
            } else {
                "already at the oldest article".to_owned()
            };
            return Vec::new();
        }

        self.article_cursor = next as usize;
        self.load_selected_article()
    }

    fn refresh(&mut self) -> Vec<Request> {
        match self.focus {
            Pane::Groups => {
                self.status = format!("reloading {}…", self.group_scope.describe());
                self.inflight += 1;
                vec![Request::LoadGroups {
                    scope: self.group_scope.clone(),
                }]
            }
            Pane::Articles | Pane::Body => {
                let Some(summary) = &self.group else {
                    return Vec::new();
                };
                let Some((low, high)) = summary.range() else {
                    return Vec::new();
                };

                let first = high
                    .saturating_sub(self.initial_articles.saturating_sub(1))
                    .max(low);
                let group = summary.name.clone();
                self.status = format!("reloading {group}…");
                self.inflight += 1;
                let token = self.begin_overview_fetch();

                vec![Request::LoadOverview {
                    group,
                    range: Range::between(first, high),
                    token,
                }]
            }
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        match self.focus {
            Pane::Groups => self.move_group_cursor(delta),
            Pane::Articles => {
                let count = self.visible_articles().len();
                self.article_cursor = shift(self.article_cursor, delta, count);
            }
            Pane::Body => self.scroll_body(delta),
        }
    }

    fn move_to_start(&mut self) {
        match self.focus {
            Pane::Groups => self.group_cursor = 0,
            Pane::Articles => self.article_cursor = 0,
            Pane::Body => self.body_scroll = 0,
        }
    }

    fn move_to_end(&mut self) {
        match self.focus {
            Pane::Groups => {
                self.group_cursor = self.visible_groups().len().saturating_sub(1);
            }
            Pane::Articles => {
                self.article_cursor = self.visible_articles().len().saturating_sub(1);
            }
            Pane::Body => {
                let lines = self.article.as_ref().map_or(0, |view| view.body.len());
                // Stop with the last line at the bottom of the pane rather than scrolling
                // past the end into an empty screen.
                self.body_scroll = lines.saturating_sub(self.body_height.max(1));
            }
        }
    }

    fn scroll_body(&mut self, delta: isize) {
        let lines = self.article.as_ref().map_or(0, |view| view.body.len());
        let max = lines.saturating_sub(1);
        self.body_scroll = shift(self.body_scroll, delta, max + 1);
    }
}

/// The text an editor opens on.
///
/// Headers, a blank line, then the body — the shape of an article, so that somebody who
/// knows what one looks like can edit it without being told anything. No instructions, no
/// comment lines to strip: a comment syntax would be one more thing to get wrong, and a
/// stray instruction line posted to a group is worse than a blank template.
fn template_for(draft: &nntp_proto::Draft) -> String {
    let mut text = String::new();
    for (name, value) in &draft.headers {
        text.push_str(&format!("{name}: {value}\n"));
    }
    text.push('\n');
    text.push_str(&draft.body);
    text
}

/// `n` with the right noun after it.
///
/// "1 replies folded" is the kind of detail that makes an interface feel unfinished.
fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// Moves a cursor by `delta` within `0..count`, clamping at both ends.
///
/// Clamping rather than wrapping: a list that jumps from the end back to the beginning
/// when you hold a key down is disorienting, and a news reader is mostly held-down keys.
fn shift(current: usize, delta: isize, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let last = count - 1;
    let next = current as isize + delta;
    next.clamp(0, last as isize) as usize
}

#[cfg(test)]
mod tests {
    use nntp_proto::{GroupName, OverviewFmt, PostingStatus, StatusLine};

    use crate::tui::protocol::FetchToken;

    use super::*;

    /// A group list as the worker delivers one: everything the server carries.
    fn groups_arrived(rows: Vec<GroupRow>) -> Event {
        Event::Groups {
            rows,
            scope: GroupScope::everything(),
            filtered_locally: false,
        }
    }

    /// A read-state store that is never saved: these tests exercise the state machine,
    /// which by design never writes the file (ADR-0008).
    fn store() -> ReadStore {
        ReadStore::empty(std::path::PathBuf::from("unused-by-the-state-machine"))
    }

    fn app() -> App {
        App::new(&UiConfig::default(), store())
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(character: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL)
    }

    fn group_row(name: &str, low: u64, high: u64) -> GroupRow {
        GroupRow {
            name: GroupName::parse(name).unwrap(),
            low,
            high,
            status: PostingStatus::Permitted,
            description: Some(format!("about {name}")),
        }
    }

    fn wildmat(pattern: &str) -> Wildmat {
        Wildmat::parse(pattern).expect("a valid pattern")
    }

    fn some_groups() -> Vec<GroupRow> {
        vec![
            group_row("comp.lang.rust", 1, 10),
            group_row("comp.lang.c", 1, 5),
            group_row("misc.test", 1, 3),
            group_row("empty.group", 1, 0),
        ]
    }

    fn summary(name: &str, low: u64, high: u64, count: u64) -> GroupSummary {
        let line =
            StatusLine::parse(format!("211 {count} {low} {high} {name}").as_bytes()).unwrap();
        GroupSummary::parse(&line, None).unwrap()
    }

    fn record(number: u64, subject: &str) -> OverviewRecord {
        let line = format!("{number}\t{subject}\ta@x\t\t<{number}@x>\t\t10\t1");
        OverviewRecord::parse(line.as_bytes(), &OverviewFmt::standard()).unwrap()
    }

    /// An article with the given header block and body.
    fn article_with_headers(headers: &str, body: &str) -> nntp_proto::Article {
        let block =
            nntp_proto::DataBlock::parse(format!("{headers}\r\n{body}\r\n.\r\n").as_bytes());
        nntp_proto::Article::from_block(&block)
    }

    /// A record that replies to the given article numbers.
    fn reply(number: u64, subject: &str, references: &[u64]) -> OverviewRecord {
        let refs = references
            .iter()
            .map(|n| format!("<{n}@x>"))
            .collect::<Vec<_>>()
            .join(" ");
        let line = format!("{number}\t{subject}\ta@x\t\t<{number}@x>\t{refs}\t10\t1");
        OverviewRecord::parse(line.as_bytes(), &OverviewFmt::standard()).unwrap()
    }

    /// Delivers a whole overview fetch the way the worker does: one chunk, then the
    /// completion.
    ///
    /// Most tests care that the records end up listed, not about how many pieces they
    /// arrived in; the tests that care about the pieces build the events themselves.
    fn deliver_overview(app: &mut App, group: &str, records: Vec<OverviewRecord>, skipped: usize) {
        let group = nntp_proto::GroupName::parse(group).unwrap();
        let token = app.begin_overview_fetch();
        app.on_event(Event::OverviewChunk {
            group: group.clone(),
            token,
            records,
            skipped,
        });
        app.on_event(Event::OverviewComplete { group, token });
    }

    /// Drives an app through to a group with articles listed.
    fn app_with_articles() -> App {
        let mut app = app();
        app.on_event(groups_arrived(some_groups()));
        app.on_event(Event::GroupOpened(Box::new(summary("misc.test", 1, 3, 3))));
        deliver_overview(
            &mut app,
            "misc.test",
            vec![
                record(1, "oldest"),
                record(2, "middle"),
                record(3, "newest"),
            ],
            0,
        );
        app
    }

    /// An article event for `number`, as the worker would deliver it.
    fn article_event(number: u64) -> Event {
        let block =
            nntp_proto::DataBlock::parse(b"From: a@b\r\nSubject: something\r\n\r\nbody\r\n.\r\n");
        Event::Article(Box::new(
            nntp_proto::Article::from_block(&block).with_number(number),
        ))
    }

    #[test]
    fn opening_an_article_marks_it_read() {
        let mut app = app_with_articles();
        assert!(app.is_unread(2));

        app.on_event(article_event(2));

        assert!(!app.is_unread(2), "the article the user just read is read");
        assert!(app.is_unread(1), "and nothing else is");
        assert!(app.read.is_dirty(), "so the run loop knows to save");
    }

    #[test]
    fn marking_read_on_open_can_be_turned_off() {
        let config = UiConfig {
            mark_read_on_open: false,
            ..UiConfig::default()
        };
        let mut app = App::new(&config, store());
        app.on_event(Event::GroupOpened(Box::new(summary("misc.test", 1, 3, 3))));

        app.on_event(article_event(2));

        assert!(app.is_unread(2), "the user asked to decide this themselves");
        assert!(!app.read.is_dirty(), "and nothing was recorded");
    }

    #[test]
    fn an_article_arriving_with_no_group_selected_is_not_recorded_anywhere() {
        // A fetch by message-id has no group. Guessing the current group would record
        // read state against numbers that belong to a different group entirely.
        let mut app = app();

        app.on_event(article_event(2));

        assert!(!app.read.is_dirty());
        assert_eq!(app.read.group_count(), 0);
    }

    #[test]
    fn the_unread_filter_hides_read_articles_and_keeps_the_cursor_where_it_was() {
        let mut app = app_with_articles();
        // Read the middle article, then put the cursor on the oldest.
        app.read.mark_read("misc.test", 2);
        app.article_cursor = 0;
        assert_eq!(app.selected_article().map(|r| r.number), Some(1));

        app.on_key(key(KeyCode::Char('u')));

        assert!(app.unread_only);
        assert_eq!(app.visible_articles(), vec![0, 2], "article 2 is hidden");
        assert_eq!(
            app.selected_article().map(|r| r.number),
            Some(1),
            "the article under the cursor stays under the cursor"
        );

        // And back again.
        app.on_key(key(KeyCode::Char('u')));
        assert!(!app.unread_only);
        assert_eq!(app.visible_articles(), vec![0, 1, 2]);
        assert_eq!(app.selected_article().map(|r| r.number), Some(1));
    }

    #[test]
    fn the_cursor_survives_the_article_under_it_being_filtered_away() {
        let mut app = app_with_articles();
        app.article_cursor = 1;
        app.on_key(key(KeyCode::Char('u')));

        // Marking the article under the cursor read removes it from a filtered list.
        app.on_key(key(KeyCode::Char('M')));

        assert!(!app.is_unread(2));
        let visible = app.visible_articles();
        assert_eq!(visible.len(), 2);
        assert!(
            app.article_cursor < visible.len(),
            "cursor {} is outside a list of {}",
            app.article_cursor,
            visible.len()
        );
    }

    #[test]
    fn everything_read_with_the_filter_on_shows_an_empty_list_rather_than_a_bad_cursor() {
        let mut app = app_with_articles();
        app.read.mark_range_read("misc.test", 1, 3);
        app.on_key(key(KeyCode::Char('u')));

        assert!(app.visible_articles().is_empty());
        assert_eq!(app.selected_article(), None);
        assert_eq!(app.article_cursor, 0);

        // Moving around an empty list must not panic or produce a request.
        assert!(app.on_key(key(KeyCode::Char('j'))).is_empty());
        assert!(app.on_key(key(KeyCode::Char('G'))).is_empty());
        assert!(app.on_key(key(KeyCode::Char('n'))).is_empty());
        assert!(app.on_key(key(KeyCode::Enter)).is_empty());
    }

    #[test]
    fn marking_toggles_both_ways() {
        let mut app = app_with_articles();
        app.article_cursor = 0;

        app.on_key(key(KeyCode::Char('M')));
        assert!(!app.is_unread(1));
        assert!(app.status.contains("read"), "{}", app.status);

        app.on_key(key(KeyCode::Char('M')));
        assert!(app.is_unread(1));
        assert!(app.status.contains("unread"), "{}", app.status);
    }

    #[test]
    fn catching_up_uses_the_watermarks_not_the_listed_articles() {
        // The article list holds the newest few hundred records; catching up has to mean
        // the whole group, or the next visit will show hundreds of older articles as
        // unread.
        let mut app = app();
        app.on_event(Event::GroupOpened(Box::new(summary(
            "misc.test",
            900,
            1_000,
            101,
        ))));
        deliver_overview(
            &mut app,
            "misc.test",
            vec![record(999, "one"), record(1_000, "two")],
            0,
        );

        app.on_key(key(KeyCode::Char('c')));

        assert_eq!(app.read.read_set("misc.test").to_string(), "900-1000");
        assert!(app.status.contains("caught up"), "{}", app.status);
    }

    #[test]
    fn catching_up_with_no_group_selected_says_so_rather_than_doing_nothing() {
        let mut app = app();
        app.on_key(key(KeyCode::Char('c')));

        assert_eq!(app.read.group_count(), 0);
        assert!(app.status.contains("no group"), "{}", app.status);
    }

    #[test]
    fn the_group_list_reports_unread_counts_from_the_read_state() {
        let mut app = app();
        app.read.mark_range_read("comp.lang.rust", 1, 7);
        app.on_event(groups_arrived(some_groups()));

        let groups = some_groups();
        // comp.lang.rust spans 1..10 with 7 read.
        assert_eq!(app.unread_in(&groups[0]), 3);
        // comp.lang.c has nothing recorded, so all five numbers are unread.
        assert_eq!(app.unread_in(&groups[1]), 5);
        // An empty group has nothing to be unread, rather than a nonsense count.
        assert_eq!(app.unread_in(&groups[3]), 0);
    }

    #[test]
    fn selecting_a_group_forgets_read_state_for_expired_articles() {
        let mut app = app();
        app.read.mark_range_read("misc.test", 1, 1_000);

        // The server now starts the group at 900: everything below that is gone.
        app.on_event(Event::GroupOpened(Box::new(summary(
            "misc.test",
            900,
            1_000,
            101,
        ))));

        assert_eq!(app.read.read_set("misc.test").to_string(), "900-1000");
        assert!(
            app.status.contains("unread"),
            "the status line should say what is left: {}",
            app.status
        );
    }

    #[test]
    fn asks_for_the_group_list_on_start_up() {
        let mut app = app();
        assert_eq!(
            app.initial_requests(),
            vec![Request::LoadGroups {
                scope: GroupScope::everything()
            }]
        );
        assert_eq!(app.inflight, 1);
    }

    #[test]
    fn the_first_fetch_asks_only_for_the_subscribed_groups() {
        let mut app = app();
        app.subscriptions = vec![wildmat("comp.lang.*"), wildmat("misc.test")];

        assert_eq!(
            app.initial_requests(),
            vec![Request::LoadGroups {
                scope: GroupScope::Subscribed(vec![wildmat("comp.lang.*"), wildmat("misc.test")])
            }]
        );
    }

    #[test]
    fn searching_the_server_turns_the_filter_into_a_wildmat() {
        // The `/` box is a substring match, so the pattern it becomes is the wildmat
        // spelling of the same thing. Anything else would find a different set of groups
        // from the one the user was just looking at.
        let mut app = app();
        app.subscriptions = vec![wildmat("misc.*")];
        app.filter = "rust".to_owned();

        let requests = app.on_key(key(KeyCode::Char('S')));

        assert_eq!(
            requests,
            vec![Request::LoadGroups {
                scope: GroupScope::Search(wildmat("*rust*"))
            }]
        );
        assert_eq!(app.inflight, 1);
        assert!(app.status.contains("*rust*"), "{}", app.status);
    }

    #[test]
    fn searching_with_an_empty_filter_asks_for_the_whole_catalogue() {
        // The way out of a subscription list that turned out to be too narrow, without
        // editing a configuration file and restarting.
        let mut app = app();
        app.subscriptions = vec![wildmat("misc.*")];

        assert_eq!(
            app.on_key(key(KeyCode::Char('S'))),
            vec![Request::LoadGroups {
                scope: GroupScope::everything()
            }]
        );
    }

    #[test]
    fn a_filter_that_cannot_be_a_wildmat_is_refused_rather_than_mangled() {
        // A filter box holds anything a keyboard can produce; a wildmat is printable
        // ASCII with no spaces. Sending a mangled pattern would quietly search for the
        // wrong thing.
        let mut app = app();
        app.filter = "two words".to_owned();

        assert!(app.on_key(key(KeyCode::Char('S'))).is_empty());
        assert_eq!(app.inflight, 0, "nothing was sent, so nothing is in flight");
        assert!(app.status.contains("two words"), "{}", app.status);
    }

    #[test]
    fn a_capital_s_while_typing_a_filter_is_filter_text() {
        // Every printable key belongs to the filter while it is open; a search that fired
        // mid-word would be worse than one that needs Enter first.
        let mut app = app();
        app.on_key(key(KeyCode::Char('/')));
        let requests = app.on_key(key(KeyCode::Char('S')));

        assert!(requests.is_empty());
        assert_eq!(app.filter, "S");
    }

    #[test]
    fn reloading_asks_for_what_is_on_screen_rather_than_the_subscriptions() {
        // Having searched past their subscriptions, a reader pressing `r` wants that
        // search again. Falling back to the subscriptions would make the list they are
        // looking at disappear under them.
        let mut app = app();
        app.subscriptions = vec![wildmat("misc.*")];
        app.filter = "rust".to_owned();
        app.on_key(key(KeyCode::Char('S')));
        // The worker echoes the scope it answered, and that is what the list on screen
        // was fetched with — which is why the interface takes the scope from the event
        // rather than from the request it sent.
        app.on_event(Event::Groups {
            rows: some_groups(),
            scope: GroupScope::Search(wildmat("*rust*")),
            filtered_locally: false,
        });

        assert_eq!(
            app.on_key(key(KeyCode::Char('r'))),
            vec![Request::LoadGroups {
                scope: GroupScope::Search(wildmat("*rust*"))
            }]
        );
    }

    #[test]
    fn subscriptions_that_match_nothing_say_how_to_look_further() {
        // The failure this whole feature can produce: a typo in a subscription, an empty
        // reader, and no hint that the groups are there but unasked for.
        let mut app = app();
        app.on_event(Event::Groups {
            rows: Vec::new(),
            scope: GroupScope::Subscribed(vec![wildmat("comp.lang.rst")]),
            filtered_locally: false,
        });

        assert!(app.groups.is_empty());
        assert!(app.status.contains('S'), "{}", app.status);
        assert!(app.status.contains("subscription"), "{}", app.status);
    }

    #[test]
    fn a_search_that_finds_nothing_says_what_it_looked_for() {
        let mut app = app();
        app.on_event(Event::Groups {
            rows: Vec::new(),
            scope: GroupScope::Search(wildmat("*rust*")),
            filtered_locally: false,
        });

        assert!(app.status.contains("*rust*"), "{}", app.status);
    }

    #[test]
    fn filtering_done_here_instead_of_at_the_server_is_said_out_loud() {
        // It means the whole catalogue crossed the network anyway, so the subscriptions
        // did not buy the thing they were configured to buy. Silence would leave a slow
        // start-up with no explanation.
        let mut app = app();
        app.on_event(Event::Groups {
            rows: some_groups(),
            scope: GroupScope::Subscribed(vec![wildmat("comp.lang.*")]),
            filtered_locally: true,
        });

        assert!(
            app.messages
                .iter()
                .any(|message| message.contains("filtered")),
            "{:?}",
            app.messages
        );
    }

    #[test]
    fn shows_the_groups_it_is_given() {
        let mut app = app();
        app.on_event(groups_arrived(some_groups()));

        assert_eq!(app.groups.len(), 4);
        assert_eq!(app.visible_groups().len(), 4);
        assert_eq!(
            app.selected_group().map(|g| g.name.as_str()),
            Some("comp.lang.rust")
        );
        assert_eq!(app.inflight, 0);
    }

    #[test]
    fn the_cursor_clamps_instead_of_wrapping() {
        // Holding a key down should stop at the end, not jump back to the top.
        let mut app = app();
        app.on_event(groups_arrived(some_groups()));

        for _ in 0..10 {
            app.on_key(key(KeyCode::Down));
        }
        assert_eq!(app.group_cursor, 3);

        for _ in 0..10 {
            app.on_key(key(KeyCode::Up));
        }
        assert_eq!(app.group_cursor, 0);
    }

    #[test]
    fn vim_and_arrow_keys_agree() {
        let mut app = app();
        app.on_event(groups_arrived(some_groups()));

        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.group_cursor, 1);
        app.on_key(key(KeyCode::Char('k')));
        assert_eq!(app.group_cursor, 0);
        app.on_key(key(KeyCode::Char('G')));
        assert_eq!(app.group_cursor, 3);
        app.on_key(key(KeyCode::Char('g')));
        assert_eq!(app.group_cursor, 0);
    }

    #[test]
    fn page_keys_move_by_a_screenful() {
        let mut app = app();
        app.on_event(groups_arrived(some_groups()));
        app.set_pane_heights(3, 10);

        app.on_key(key(KeyCode::PageDown));
        assert_eq!(app.group_cursor, 2);
        app.on_key(key(KeyCode::PageUp));
        assert_eq!(app.group_cursor, 0);

        app.on_key(ctrl('d'));
        assert_eq!(app.group_cursor, 2);
        app.on_key(ctrl('u'));
        assert_eq!(app.group_cursor, 0);
    }

    #[test]
    fn escape_stops_a_request_in_progress() {
        let mut app = app();
        app.initial_requests();
        assert_eq!(app.inflight, 1);
        assert!(!app.cancel.is_cancelled());

        app.on_key(key(KeyCode::Esc));

        assert!(app.cancel.is_cancelled(), "the worker's flag is raised");
        assert!(app.status.contains("cancelling"), "{}", app.status);
        // Still outstanding: the worker has not answered yet, and pretending otherwise
        // would stop the spinner while the socket is still being read.
        assert_eq!(app.inflight, 1);
    }

    #[test]
    fn the_cancelled_event_is_a_status_and_not_an_error() {
        let mut app = app();
        app.initial_requests();
        app.on_key(key(KeyCode::Esc));

        app.on_event(Event::Cancelled {
            context: "the group list".to_owned(),
        });

        assert_eq!(app.inflight, 0);
        assert!(app.status.contains("cancelled"), "{}", app.status);
        assert!(
            app.error.is_none(),
            "the user asked for this: {:?}",
            app.error
        );
        // And the message pane explains the reconnection that will follow, so it does not
        // look like a fault when it happens.
        assert!(
            app.messages
                .iter()
                .any(|message| message.contains("connection")),
            "{:?}",
            app.messages
        );
    }

    #[test]
    fn escape_with_nothing_outstanding_keeps_its_old_meaning() {
        // The filter must still be clearable, which is what Esc is for the rest of the
        // time.
        let mut app = app();
        app.on_event(groups_arrived(some_groups()));
        app.filter = "misc".to_owned();
        assert_eq!(app.inflight, 0);

        app.on_key(key(KeyCode::Esc));

        assert!(app.filter.is_empty());
        assert!(!app.cancel.is_cancelled(), "nothing was cancelled");
    }

    #[test]
    fn escape_while_typing_a_filter_still_belongs_to_the_filter() {
        // Even with a request outstanding: the user is typing, and Esc there means "never
        // mind this filter".
        let mut app = app();
        app.on_event(groups_arrived(some_groups()));
        app.initial_requests();
        app.on_key(key(KeyCode::Char('/')));
        app.on_key(key(KeyCode::Char('x')));

        app.on_key(key(KeyCode::Esc));

        assert!(!app.editing_filter);
        assert!(app.filter.is_empty());
        assert!(
            !app.cancel.is_cancelled(),
            "the load was not cancelled by a filter keystroke"
        );
    }

    #[test]
    fn cancelling_twice_is_harmless() {
        let mut app = app();
        app.initial_requests();

        app.on_key(key(KeyCode::Esc));
        app.on_key(key(KeyCode::Esc));

        assert!(app.cancel.is_cancelled());
        assert_eq!(app.inflight, 1);
    }

    #[test]
    fn cancelling_with_nothing_outstanding_says_so_rather_than_raising_the_flag() {
        // A raised flag with nothing reading would be taken by whatever ran next.
        let mut app = app();

        app.request_cancellation();

        assert!(!app.cancel.is_cancelled());
        assert!(app.status.contains("nothing to cancel"), "{}", app.status);
    }

    #[test]
    fn filtering_narrows_the_list_by_name_or_description() {
        let mut app = app();
        app.on_event(groups_arrived(some_groups()));

        app.on_key(key(KeyCode::Char('/')));
        assert!(app.editing_filter);
        for character in "lang".chars() {
            app.on_key(key(KeyCode::Char(character)));
        }
        app.on_key(key(KeyCode::Enter));

        assert!(!app.editing_filter);
        assert_eq!(app.filter, "lang");
        assert_eq!(app.visible_groups().len(), 2);
        assert_eq!(
            app.selected_group().map(|g| g.name.as_str()),
            Some("comp.lang.rust")
        );
    }

    #[test]
    fn the_arrows_move_through_the_filtered_list_while_still_typing() {
        // The reported bug: with the filter open, Up and Down did nothing at all, so a
        // filter that left several groups could not be used to pick one of them without
        // pressing Enter first.
        let mut app = app();
        app.on_event(groups_arrived(some_groups()));

        app.on_key(key(KeyCode::Char('/')));
        for character in "lang".chars() {
            app.on_key(key(KeyCode::Char(character)));
        }
        assert!(app.editing_filter, "still typing");
        assert_eq!(app.visible_groups().len(), 2);
        assert_eq!(
            app.selected_group().map(|g| g.name.as_str()),
            Some("comp.lang.rust")
        );

        app.on_key(key(KeyCode::Down));
        assert_eq!(
            app.selected_group().map(|g| g.name.as_str()),
            Some("comp.lang.c"),
            "Down should move within what the filter left"
        );

        app.on_key(key(KeyCode::Up));
        assert_eq!(
            app.selected_group().map(|g| g.name.as_str()),
            Some("comp.lang.rust")
        );

        // And the filter itself is untouched by the movement.
        assert_eq!(app.filter, "lang");
        assert!(app.editing_filter);
    }

    #[test]
    fn the_page_and_home_keys_work_while_typing_a_filter_too() {
        let mut app = app();
        app.on_event(groups_arrived(some_groups()));
        app.on_key(key(KeyCode::Char('/')));

        app.on_key(key(KeyCode::End));
        assert_eq!(app.group_cursor, 3);
        app.on_key(key(KeyCode::Home));
        assert_eq!(app.group_cursor, 0);

        app.on_key(key(KeyCode::PageDown));
        assert_eq!(app.group_cursor, 3, "clamped at the end of the list");
        app.on_key(key(KeyCode::PageUp));
        assert_eq!(app.group_cursor, 0);

        assert!(app.filter.is_empty(), "no movement key became filter text");
        assert!(app.editing_filter);
    }

    #[test]
    fn j_and_k_are_filter_text_rather_than_movement() {
        // The other half of the fix: a filter that could not contain the letter `j` would
        // be a worse bug than the one this replaced.
        let mut app = app();
        app.on_event(groups_arrived(some_groups()));
        app.on_key(key(KeyCode::Char('/')));
        app.on_key(key(KeyCode::Char('j')));
        app.on_key(key(KeyCode::Char('k')));

        assert_eq!(app.filter, "jk");
        assert_eq!(app.group_cursor, 0);
    }

    #[test]
    fn typing_more_of_the_filter_returns_the_cursor_to_the_top() {
        // Narrowing the list can leave the cursor pointing at a different group than the
        // one it was on, so it goes back to the top rather than somewhere arbitrary.
        let mut app = app();
        app.on_event(groups_arrived(some_groups()));
        app.on_key(key(KeyCode::Char('/')));
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.group_cursor, 1);

        app.on_key(key(KeyCode::Char('m')));
        assert_eq!(app.group_cursor, 0);
    }

    #[test]
    fn an_overview_reply_during_filtering_does_not_steal_the_arrow_keys() {
        // `move_cursor` dispatches on the focused pane, and an overview reply sets the
        // focus to the article list. With the filter open the arrows must keep moving the
        // group cursor regardless, or they would move something the user cannot see.
        let mut app = app();
        app.on_event(groups_arrived(some_groups()));
        app.on_key(key(KeyCode::Char('/')));

        app.on_event(Event::GroupOpened(Box::new(summary("misc.test", 1, 3, 3))));
        deliver_overview(
            &mut app,
            "misc.test",
            vec![record(1, "one"), record(2, "two")],
            0,
        );
        assert_eq!(app.focus, Pane::Articles, "the reply moved the focus");

        app.on_key(key(KeyCode::Down));

        assert_eq!(app.group_cursor, 1, "the group cursor moved");
        assert_eq!(app.article_cursor, 1, "and the article cursor did not");
    }

    #[test]
    fn filtering_is_case_insensitive() {
        let mut app = app();
        app.on_event(groups_arrived(some_groups()));
        app.filter = "RUST".to_owned();
        assert_eq!(app.visible_groups().len(), 1);
    }

    #[test]
    fn backspace_widens_the_filter_again() {
        let mut app = app();
        app.on_event(groups_arrived(some_groups()));
        app.on_key(key(KeyCode::Char('/')));
        for character in "misc".chars() {
            app.on_key(key(KeyCode::Char(character)));
        }
        assert_eq!(app.visible_groups().len(), 1);

        app.on_key(key(KeyCode::Backspace));
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(app.filter, "mi");
        assert_eq!(app.visible_groups().len(), 1);
    }

    #[test]
    fn escape_abandons_a_filter_being_typed() {
        let mut app = app();
        app.on_event(groups_arrived(some_groups()));
        app.on_key(key(KeyCode::Char('/')));
        app.on_key(key(KeyCode::Char('x')));
        app.on_key(key(KeyCode::Esc));

        assert!(!app.editing_filter);
        assert!(app.filter.is_empty());
        assert_eq!(app.visible_groups().len(), 4);
    }

    #[test]
    fn escape_clears_an_applied_filter() {
        let mut app = app();
        app.on_event(groups_arrived(some_groups()));
        app.filter = "misc".to_owned();
        app.on_key(key(KeyCode::Esc));
        assert!(app.filter.is_empty());
    }

    #[test]
    fn the_cursor_stays_in_range_when_a_filter_shrinks_the_list() {
        // Otherwise the cursor points past the end and the selected group is None.
        let mut app = app();
        app.on_event(groups_arrived(some_groups()));
        app.on_key(key(KeyCode::Char('G')));
        assert_eq!(app.group_cursor, 3);

        app.on_key(key(KeyCode::Char('/')));
        for character in "misc".chars() {
            app.on_key(key(KeyCode::Char(character)));
        }
        assert_eq!(app.group_cursor, 0);
        assert!(app.selected_group().is_some());
    }

    #[test]
    fn opening_a_group_asks_for_its_newest_articles() {
        let mut app = app();
        app.on_event(groups_arrived(some_groups()));
        let requests = app.on_key(key(KeyCode::Enter));

        assert_eq!(
            requests,
            vec![Request::OpenGroup {
                group: GroupName::parse("comp.lang.rust").unwrap(),
                count: UiConfig::default().initial_articles,
                // The first fetch of the run.
                token: FetchToken::new(1),
            }]
        );
        assert_eq!(app.inflight, 1);
    }

    #[test]
    fn opening_an_empty_group_sends_nothing() {
        let mut app = app();
        app.on_event(groups_arrived(some_groups()));
        app.on_key(key(KeyCode::Char('G')));
        assert_eq!(
            app.selected_group().map(|g| g.name.as_str()),
            Some("empty.group")
        );

        let requests = app.on_key(key(KeyCode::Enter));
        assert!(requests.is_empty());
        assert!(app.status.contains("empty"), "{}", app.status);
        assert_eq!(app.inflight, 0);
    }

    #[test]
    fn overview_selects_the_newest_article_and_moves_focus() {
        let app = app_with_articles();
        assert_eq!(app.articles.len(), 3);
        // Newest last in the list, and selected.
        assert_eq!(app.article_cursor, 2);
        assert_eq!(app.selected_article().map(|r| r.number), Some(3));
        assert_eq!(app.focus, Pane::Articles);
    }

    #[test]
    fn a_late_overview_reply_for_another_group_is_discarded() {
        // Otherwise the article list shows one group's articles under another's heading.
        let mut app = app_with_articles();
        deliver_overview(
            &mut app,
            "comp.lang.rust",
            vec![record(99, "from the wrong group")],
            0,
        );

        assert_eq!(app.articles.len(), 3);
        assert!(
            app.messages.iter().any(|m| m.contains("moved on")),
            "{:?}",
            app.messages
        );
    }

    #[test]
    fn records_are_listed_as_each_chunk_arrives() {
        // The point of #8: the pane fills while the rest is still on the wire, rather
        // than staying empty until the whole range has been fetched.
        let mut app = app();
        app.on_event(Event::GroupOpened(Box::new(summary("misc.test", 1, 4, 4))));
        let group = GroupName::parse("misc.test").unwrap();
        let token = app.begin_overview_fetch();

        app.on_event(Event::OverviewChunk {
            group: group.clone(),
            token,
            records: vec![record(3, "newest"), record(4, "newer still")],
            skipped: 0,
        });

        assert_eq!(app.articles.len(), 2, "listed before the fetch finished");
        assert_eq!(app.focus, Pane::Articles);
        assert_eq!(app.inflight, 0, "the completion is what ends the request");

        app.on_event(Event::OverviewChunk {
            group: group.clone(),
            token,
            records: vec![record(1, "oldest"), record(2, "older")],
            skipped: 0,
        });
        app.on_event(Event::OverviewComplete { group, token });

        // Chunks arrive newest first and are merged back into article-number order.
        assert_eq!(
            app.articles.iter().map(|r| r.number).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
    }

    #[test]
    fn a_cursor_on_the_newest_article_follows_the_newest() {
        let mut app = app();
        app.on_event(Event::GroupOpened(Box::new(summary("misc.test", 1, 4, 4))));
        let group = GroupName::parse("misc.test").unwrap();
        let token = app.begin_overview_fetch();

        app.on_event(Event::OverviewChunk {
            group: group.clone(),
            token,
            records: vec![record(3, "newest"), record(4, "newer still")],
            skipped: 0,
        });
        assert_eq!(app.selected_article().map(|r| r.number), Some(4));

        // Older records land in front of it. The cursor's index changes; the article it
        // is on must not.
        app.on_event(Event::OverviewChunk {
            group,
            token,
            records: vec![record(1, "oldest"), record(2, "older")],
            skipped: 0,
        });

        assert_eq!(app.article_cursor, 3);
        assert_eq!(app.selected_article().map(|r| r.number), Some(4));
    }

    #[test]
    fn a_cursor_the_user_moved_stays_on_its_article() {
        // Records arriving underneath a reader must not move their place.
        let mut app = app();
        app.on_event(Event::GroupOpened(Box::new(summary("misc.test", 1, 4, 4))));
        let group = GroupName::parse("misc.test").unwrap();
        let token = app.begin_overview_fetch();

        app.on_event(Event::OverviewChunk {
            group: group.clone(),
            token,
            records: vec![record(3, "newest"), record(4, "newer still")],
            skipped: 0,
        });

        app.focus = Pane::Articles;
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.selected_article().map(|r| r.number), Some(3));

        app.on_event(Event::OverviewChunk {
            group,
            token,
            records: vec![record(1, "oldest"), record(2, "older")],
            skipped: 0,
        });

        assert_eq!(
            app.selected_article().map(|r| r.number),
            Some(3),
            "the cursor stayed on article 3 at its new index"
        );
        assert_eq!(app.article_cursor, 2);
    }

    #[test]
    fn a_chunk_of_a_superseded_fetch_of_the_same_group_is_dropped() {
        // The group name cannot catch this one: both fetches are for the group on screen.
        // Without the token, a refresh started mid-fetch would have the abandoned fetch's
        // records merged into the new one's list.
        let mut app = app();
        app.on_event(Event::GroupOpened(Box::new(summary("misc.test", 1, 4, 4))));
        let group = GroupName::parse("misc.test").unwrap();

        let abandoned = app.begin_overview_fetch();
        let current = app.begin_overview_fetch();
        assert_ne!(abandoned, current);

        app.on_event(Event::OverviewChunk {
            group: group.clone(),
            token: abandoned,
            records: vec![record(1, "from the abandoned fetch")],
            skipped: 0,
        });
        assert!(app.articles.is_empty(), "{:?}", app.status);

        app.on_event(Event::OverviewChunk {
            group,
            token: current,
            records: vec![record(2, "from the current fetch")],
            skipped: 0,
        });
        assert_eq!(
            app.articles.iter().map(|r| r.number).collect::<Vec<_>>(),
            vec![2]
        );
    }

    #[test]
    fn a_record_that_arrives_twice_is_listed_once() {
        // A refresh re-fetches a range already on screen. Two copies of one article in
        // the list would be a visible bug, and the newer copy is the truer one.
        let mut app = app_with_articles();
        let group = GroupName::parse("misc.test").unwrap();
        let token = app.begin_overview_fetch();

        app.on_event(Event::OverviewChunk {
            group: group.clone(),
            token,
            records: vec![record(2, "edited subject"), record(4, "brand new")],
            skipped: 0,
        });
        app.on_event(Event::OverviewComplete { group, token });

        assert_eq!(
            app.articles.iter().map(|r| r.number).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        assert_eq!(
            app.articles
                .iter()
                .find(|r| r.number == 2)
                .map(|r| r.subject.as_str()),
            Some("edited subject"),
            "the copy that just arrived wins"
        );
    }

    #[test]
    fn an_empty_group_finishes_with_an_empty_list() {
        // "Nothing here" and "not arrived yet" have to look different, so the completion
        // is sent even when no chunk carried anything.
        let mut app = app_with_articles();
        app.on_event(Event::GroupOpened(Box::new(summary(
            "comp.lang.rust",
            1,
            0,
            0,
        ))));
        let group = GroupName::parse("comp.lang.rust").unwrap();
        let token = app.begin_overview_fetch();
        app.articles.clear();

        app.on_event(Event::OverviewComplete { group, token });

        assert!(app.articles.is_empty());
        assert!(app.status.contains("0 articles listed"), "{}", app.status);
    }

    #[test]
    fn unparseable_lines_are_counted_across_chunks_and_reported_once() {
        // One message per chunk would bury everything else in the message pane on a
        // group the server serves badly.
        let mut app = app();
        app.on_event(Event::GroupOpened(Box::new(summary("misc.test", 1, 4, 4))));
        let group = GroupName::parse("misc.test").unwrap();
        let token = app.begin_overview_fetch();

        for number in [3, 1] {
            app.on_event(Event::OverviewChunk {
                group: group.clone(),
                token,
                records: vec![record(number, "fine")],
                skipped: 2,
            });
        }
        assert!(
            !app.messages
                .iter()
                .any(|m| m.contains("could not be parsed")),
            "not reported per chunk: {:?}",
            app.messages
        );

        app.on_event(Event::OverviewComplete { group, token });

        let reported = app
            .messages
            .iter()
            .filter(|m| m.contains("could not be parsed"))
            .collect::<Vec<_>>();
        assert_eq!(reported.len(), 1, "{:?}", app.messages);
        assert!(reported[0].contains('4'), "{:?}", reported);
    }

    /// An app showing one thread: a root, two replies, and one unrelated article.
    fn app_with_a_thread() -> App {
        let mut app = app();
        app.on_event(Event::GroupOpened(Box::new(summary("misc.test", 1, 4, 4))));
        deliver_overview(
            &mut app,
            "misc.test",
            vec![
                reply(1, "the question", &[]),
                reply(2, "Re: the question", &[1]),
                reply(3, "Re: the question", &[1, 2]),
                reply(4, "something else", &[]),
            ],
            0,
        );
        app
    }

    #[test]
    fn replies_are_indented_under_what_they_answer() {
        let app = app_with_a_thread();
        assert!(app.threaded, "threading is the default");

        let rows = app.article_rows();
        let shape: Vec<(u64, usize)> = rows
            .iter()
            .filter_map(|row| app.articles.get(row.index).map(|r| (r.number, row.depth)))
            .collect();

        assert_eq!(shape, vec![(1, 0), (2, 1), (3, 2), (4, 0)]);
    }

    #[test]
    fn threading_reorders_the_list_but_not_the_cursor() {
        // `t` changes the shape of the list, so landing on a different article would be a
        // surprise. The article under the cursor stays under it.
        let mut app = app_with_a_thread();
        app.focus = Pane::Articles;
        app.article_cursor = 1;
        assert_eq!(app.selected_article().map(|r| r.number), Some(2));

        app.on_key(key(KeyCode::Char('t')));
        assert!(!app.threaded);
        assert_eq!(app.selected_article().map(|r| r.number), Some(2));

        app.on_key(key(KeyCode::Char('t')));
        assert!(app.threaded);
        assert_eq!(app.selected_article().map(|r| r.number), Some(2));
    }

    #[test]
    fn the_flat_view_is_article_number_order() {
        let mut app = app_with_a_thread();
        app.on_key(key(KeyCode::Char('t')));

        let numbers: Vec<u64> = app
            .article_rows()
            .iter()
            .filter_map(|row| app.articles.get(row.index).map(|r| r.number))
            .collect();

        assert_eq!(numbers, vec![1, 2, 3, 4]);
        assert!(app.article_rows().iter().all(|row| row.depth == 0));
    }

    #[test]
    fn folding_hides_the_replies_and_says_how_many() {
        let mut app = app_with_a_thread();
        app.focus = Pane::Articles;
        app.article_cursor = 0;

        app.on_key(key(KeyCode::Char('z')));

        let rows = app.article_rows();
        let numbers: Vec<u64> = rows
            .iter()
            .filter_map(|row| app.articles.get(row.index).map(|r| r.number))
            .collect();
        assert_eq!(numbers, vec![1, 4], "the two replies are folded away");
        assert_eq!(rows.first().map(|row| row.hidden), Some(2));
        assert!(app.status.contains('2'), "{}", app.status);

        // And back again.
        app.on_key(key(KeyCode::Char('z')));
        assert_eq!(app.article_rows().len(), 4);
    }

    #[test]
    fn folding_leaves_the_cursor_on_the_article_it_folded() {
        let mut app = app_with_a_thread();
        app.focus = Pane::Articles;
        app.article_cursor = 0;

        app.on_key(key(KeyCode::Char('z')));
        assert_eq!(app.selected_article().map(|r| r.number), Some(1));

        app.on_key(key(KeyCode::Char('z')));
        assert_eq!(app.selected_article().map(|r| r.number), Some(1));
    }

    #[test]
    fn folding_an_article_with_no_replies_says_so_rather_than_doing_nothing() {
        let mut app = app_with_a_thread();
        app.focus = Pane::Articles;
        app.article_cursor = 3;
        assert_eq!(app.selected_article().map(|r| r.number), Some(4));

        app.on_key(key(KeyCode::Char('z')));

        assert_eq!(app.article_rows().len(), 4);
        assert!(app.status.contains("no replies"), "{}", app.status);
    }

    #[test]
    fn a_fold_cannot_hide_an_unread_reply_from_the_unread_filter() {
        // The filter's job is to show what has not been read. A fold on an article the
        // filter itself hides must not take its unread replies with it.
        let mut app = app_with_a_thread();
        app.focus = Pane::Articles;
        app.article_cursor = 0;
        app.on_key(key(KeyCode::Char('z')));

        app.read.mark_read("misc.test", 1);
        app.unread_only = true;

        let numbers: Vec<u64> = app
            .article_rows()
            .iter()
            .filter_map(|row| app.articles.get(row.index).map(|r| r.number))
            .collect();

        assert_eq!(numbers, vec![2, 3, 4]);
    }

    #[test]
    fn folds_do_not_survive_moving_to_another_group() {
        // Article numbers mean something else in the next group, so a fold carried over
        // would land on an unrelated article.
        let mut app = app_with_a_thread();
        app.focus = Pane::Articles;
        app.article_cursor = 0;
        app.on_key(key(KeyCode::Char('z')));
        assert_eq!(app.article_rows().len(), 2);

        app.on_event(groups_arrived(some_groups()));
        app.focus = Pane::Groups;
        app.on_key(key(KeyCode::Enter));
        app.on_event(Event::GroupOpened(Box::new(summary("misc.test", 1, 4, 4))));
        deliver_overview(
            &mut app,
            "misc.test",
            vec![
                reply(1, "the question", &[]),
                reply(2, "Re: the question", &[1]),
            ],
            0,
        );

        assert_eq!(app.article_rows().len(), 2, "nothing is folded");
        assert_eq!(app.article_rows().get(1).map(|row| row.depth), Some(1));
    }

    #[test]
    fn folding_with_threading_off_says_what_to_press() {
        let mut app = app_with_a_thread();
        app.on_key(key(KeyCode::Char('t')));
        app.focus = Pane::Articles;

        app.on_key(key(KeyCode::Char('z')));

        assert!(app.status.contains('t'), "{}", app.status);
        assert_eq!(app.article_rows().len(), 4);
    }

    #[test]
    fn a_thread_assembled_from_two_chunks_is_still_a_thread() {
        // The reply can arrive before the article it answers, since chunks come newest
        // first. Threading is rebuilt as records land, so the order they arrive in must
        // not change the result.
        let mut app = app();
        app.on_event(Event::GroupOpened(Box::new(summary("misc.test", 1, 2, 2))));
        let group = GroupName::parse("misc.test").unwrap();
        let token = app.begin_overview_fetch();

        app.on_event(Event::OverviewChunk {
            group: group.clone(),
            token,
            records: vec![reply(2, "Re: the question", &[1])],
            skipped: 0,
        });
        assert_eq!(app.article_rows().first().map(|row| row.depth), Some(0));

        app.on_event(Event::OverviewChunk {
            group: group.clone(),
            token,
            records: vec![reply(1, "the question", &[])],
            skipped: 0,
        });
        app.on_event(Event::OverviewComplete { group, token });

        let shape: Vec<(u64, usize)> = app
            .article_rows()
            .iter()
            .filter_map(|row| app.articles.get(row.index).map(|r| (r.number, row.depth)))
            .collect();
        assert_eq!(shape, vec![(1, 0), (2, 1)]);
    }

    /// An app with an identity, so posting is possible at all.
    fn app_that_can_post() -> App {
        let mut app = app_with_a_thread();
        app.from = Some("A Poster <poster@example.org>".to_owned());
        app
    }

    #[test]
    fn writing_an_article_pre_fills_what_the_reader_already_knows() {
        let mut app = app_that_can_post();

        app.on_key(key(KeyCode::Char('w')));

        let request = app.take_compose().expect("a composing session");
        assert!(
            request
                .template
                .contains("From: A Poster <poster@example.org>")
        );
        assert!(request.template.contains("Newsgroups: misc.test"));
        assert!(request.template.contains("Subject:"));
        // Headers, a blank line, then the body: the shape of an article.
        assert!(request.template.contains("\n\n"), "{:?}", request.template);
    }

    #[test]
    fn posting_without_an_identity_says_which_key_to_set() {
        // A guessed From is how articles end up signed `user@localhost`.
        let mut app = app_with_a_thread();
        app.from = None;

        app.on_key(key(KeyCode::Char('w')));

        assert!(app.take_compose().is_none());
        assert!(app.status.contains("from"), "{}", app.status);
        assert!(
            app.messages.iter().any(|m| m.contains("configuration")),
            "{:?}",
            app.messages
        );
    }

    #[test]
    fn a_follow_up_quotes_the_article_and_carries_its_references() {
        let mut app = app_that_can_post();
        app.on_event(Event::Article(Box::new(article_with_headers(
            "Message-ID: <parent@example.org>\r\n\
             References: <root@example.org>\r\n\
             Newsgroups: misc.test\r\n\
             From: Original Poster <op@example.org>\r\n\
             Subject: the question\r\n",
            "What do you think?",
        ))));

        app.on_key(key(KeyCode::Char('f')));

        let request = app.take_compose().expect("a composing session");
        assert!(
            request.template.contains("Subject: Re: the question"),
            "{}",
            request.template
        );
        assert!(
            request
                .template
                .contains("References: <root@example.org> <parent@example.org>"),
            "{}",
            request.template
        );
        assert!(request.template.contains("Newsgroups: misc.test"));
        assert!(
            request.template.contains("> What do you think?"),
            "{}",
            request.template
        );
        assert!(request.template.contains("wrote:"), "{}", request.template);
    }

    #[test]
    fn a_follow_up_goes_where_followup_to_says() {
        let mut app = app_that_can_post();
        app.on_event(Event::Article(Box::new(article_with_headers(
            "Message-ID: <parent@example.org>\r\n\
             Newsgroups: misc.test,comp.lang.rust\r\n\
             Followup-To: misc.test\r\n\
             From: op@example.org\r\n\
             Subject: crossposted\r\n",
            "Body.",
        ))));

        app.on_key(key(KeyCode::Char('f')));

        let request = app.take_compose().expect("a composing session");
        assert!(
            request.template.contains("Newsgroups: misc.test\n"),
            "a crosspost must follow up only where the author asked: {}",
            request.template
        );
        assert!(
            !request.template.contains("comp.lang.rust"),
            "{}",
            request.template
        );
    }

    #[test]
    fn followup_to_poster_is_refused_rather_than_posted_to_the_group() {
        // RFC 5536 §3.2.6: the author asked for mail. This reader cannot send mail, and
        // posting anyway would put the reply exactly where it was asked not to go.
        let mut app = app_that_can_post();
        app.on_event(Event::Article(Box::new(article_with_headers(
            "Message-ID: <parent@example.org>\r\n\
             Newsgroups: misc.test\r\n\
             Followup-To: poster\r\n\
             From: op@example.org\r\n\
             Subject: mail me\r\n",
            "Body.",
        ))));

        app.on_key(key(KeyCode::Char('f')));

        assert!(app.take_compose().is_none());
        assert!(app.status.contains("poster"), "{}", app.status);
    }

    #[test]
    fn following_up_with_nothing_open_says_so() {
        let mut app = app_that_can_post();
        app.on_key(key(KeyCode::Char('f')));

        assert!(app.take_compose().is_none());
        assert!(app.status.contains("open an article"), "{}", app.status);
    }

    #[test]
    fn a_composed_article_becomes_a_post_request() {
        let mut app = app_that_can_post();

        let requests = app.on_composed(Some((
            "From: A Poster <poster@example.org>\n\
             Newsgroups: misc.test\n\
             Subject: Testing\n\
             \n\
             Hello.\n"
                .to_owned(),
            PathBuf::from("/tmp/draft.article"),
        )));

        assert_eq!(requests.len(), 1);
        assert!(matches!(requests.first(), Some(Request::Post { .. })));
        assert_eq!(app.inflight, 1);
    }

    #[test]
    fn a_draft_with_problems_is_reported_and_not_sent() {
        // The problems belong in front of the user, not in a 441 after a round trip.
        let mut app = app_that_can_post();

        let requests = app.on_composed(Some((
            "Subject: no From, no Newsgroups\n\nBody.\n".to_owned(),
            PathBuf::from("/tmp/draft.article"),
        )));

        assert!(requests.is_empty());
        assert_eq!(app.inflight, 0);
        assert!(
            app.messages.iter().any(|m| m.contains("From")),
            "{:?}",
            app.messages
        );
        assert!(
            app.messages.iter().any(|m| m.contains("draft.article")),
            "the draft's path must be findable: {:?}",
            app.messages
        );
    }

    #[test]
    fn a_failed_posting_says_where_the_draft_is() {
        let mut app = app_that_can_post();
        app.on_composed(Some((
            "From: a@x\nNewsgroups: misc.test\nSubject: s\n\nBody.\n".to_owned(),
            PathBuf::from("/tmp/draft.article"),
        )));

        app.on_event(Event::Failed {
            context: "posting to misc.test".to_owned(),
            message: "441 no colon-space in \"From\" header".to_owned(),
        });

        assert!(
            app.messages.iter().any(|m| m.contains("draft.article")),
            "{:?}",
            app.messages
        );
        assert_eq!(app.inflight, 0);
    }

    #[test]
    fn an_abandoned_composition_posts_nothing() {
        let mut app = app_that_can_post();
        assert!(app.on_composed(None).is_empty());
        assert_eq!(app.inflight, 0);
    }

    #[test]
    fn a_superseded_reload_does_not_read_as_data_loss() {
        // Routine now that posting reloads the group: post twice and the first reload is
        // overtaken by the second. "Discarded" for that would send somebody looking for
        // the articles it lost, and it lost none.
        let mut app = app_with_a_thread();
        let group = GroupName::parse("misc.test").unwrap();

        let overtaken = app.begin_overview_fetch();
        let _current = app.begin_overview_fetch();

        app.on_event(Event::OverviewComplete {
            group,
            token: overtaken,
        });

        let note = app
            .messages
            .front()
            .cloned()
            .unwrap_or_else(|| "no message".to_owned());
        assert!(note.contains("overtaken"), "{note}");
        assert!(!note.contains("discarded"), "{note}");
    }

    #[test]
    fn unparseable_overview_lines_are_reported_not_hidden() {
        let mut app = app();
        app.on_event(Event::GroupOpened(Box::new(summary("misc.test", 1, 3, 3))));
        deliver_overview(&mut app, "misc.test", vec![record(1, "fine")], 2);

        assert!(
            app.messages
                .iter()
                .any(|m| m.contains("could not be parsed")),
            "{:?}",
            app.messages
        );
    }

    #[test]
    fn opening_an_article_asks_for_it_by_number_in_the_selected_group() {
        let mut app = app_with_articles();
        let requests = app.on_key(key(KeyCode::Enter));

        assert_eq!(
            requests,
            vec![Request::LoadArticle {
                group: Some(GroupName::parse("misc.test").unwrap()),
                spec: ArticleSpec::Number(3),
            }]
        );
    }

    #[test]
    fn n_and_p_walk_the_article_list_and_stop_at_the_ends() {
        let mut app = app_with_articles();
        app.article_cursor = 1;

        let forward = app.on_key(key(KeyCode::Char('n')));
        assert_eq!(app.article_cursor, 2);
        assert_eq!(forward.len(), 1);

        // Already at the newest: no request, and the status says why.
        let blocked = app.on_key(key(KeyCode::Char('n')));
        assert!(blocked.is_empty());
        assert!(app.status.contains("newest"), "{}", app.status);

        app.article_cursor = 0;
        let back = app.on_key(key(KeyCode::Char('p')));
        assert!(back.is_empty());
        assert!(app.status.contains("oldest"), "{}", app.status);
    }

    #[test]
    fn an_article_event_fills_the_body_pane() {
        let mut app = app_with_articles();
        let block = nntp_proto::DataBlock::parse(
            b"From: a@b\r\nSubject: =?UTF-8?Q?caf=C3=A9?=\r\n\
              Date: Wed, 17 Sep 2026 08:00:00 +0000\r\n\r\nline one\r\nline two\r\n.\r\n",
        );
        let article = nntp_proto::Article::from_block(&block).with_number(3);
        app.on_event(Event::Article(Box::new(article)));

        let view = app.article.as_ref().expect("an article");
        assert_eq!(view.subject, "café");
        assert_eq!(view.number, Some(3));
        assert_eq!(view.body, ["line one", "line two"]);
        assert_eq!(view.date, "2026-09-17 08:00");
        assert_eq!(app.focus, Pane::Body);
        assert_eq!(app.body_scroll, 0);
    }

    #[test]
    fn an_article_with_an_unparseable_date_shows_the_raw_text() {
        let mut app = app();
        let block = nntp_proto::DataBlock::parse(
            b"From: a@b\r\nDate: yesterday afternoon\r\n\r\nbody\r\n.\r\n",
        );
        app.on_event(Event::Article(Box::new(nntp_proto::Article::from_block(
            &block,
        ))));

        assert_eq!(
            app.article.as_ref().map(|view| view.date.as_str()),
            Some("yesterday afternoon")
        );
    }

    #[test]
    fn the_body_scrolls_without_running_off_the_end() {
        let mut app = app();
        let body: String = (0..50).map(|n| format!("line {n}\r\n")).collect();
        let block =
            nntp_proto::DataBlock::parse(format!("From: a@b\r\n\r\n{body}.\r\n").as_bytes());
        app.on_event(Event::Article(Box::new(nntp_proto::Article::from_block(
            &block,
        ))));
        app.set_pane_heights(20, 10);
        app.focus = Pane::Body;

        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.body_scroll, 1);

        app.on_key(key(KeyCode::PageDown));
        assert_eq!(app.body_scroll, 10);

        app.on_key(key(KeyCode::Char('G')));
        // The last line sits at the bottom of the pane, not off the top of it.
        assert_eq!(app.body_scroll, 40);

        for _ in 0..100 {
            app.on_key(key(KeyCode::Char('j')));
        }
        assert_eq!(app.body_scroll, 49);

        app.on_key(key(KeyCode::Char('g')));
        assert_eq!(app.body_scroll, 0);
    }

    #[test]
    fn focus_cycles_in_both_directions() {
        let mut app = app();
        assert_eq!(app.focus, Pane::Groups);

        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.focus, Pane::Articles);
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.focus, Pane::Body);
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.focus, Pane::Groups);

        app.on_key(key(KeyCode::BackTab));
        assert_eq!(app.focus, Pane::Body);
        app.on_key(key(KeyCode::Char('l')));
        assert_eq!(app.focus, Pane::Groups);
        app.on_key(key(KeyCode::Char('h')));
        assert_eq!(app.focus, Pane::Body);
    }

    #[test]
    fn refresh_reloads_whichever_pane_has_focus() {
        let mut app = app_with_articles();

        app.focus = Pane::Groups;
        assert_eq!(
            app.on_key(key(KeyCode::Char('r'))),
            vec![Request::LoadGroups {
                scope: GroupScope::everything()
            }]
        );

        app.focus = Pane::Articles;
        let requests = app.on_key(key(KeyCode::Char('r')));
        assert_eq!(
            requests,
            vec![Request::LoadOverview {
                group: GroupName::parse("misc.test").unwrap(),
                range: Range::between(1, 3),
                // `app_with_articles` already delivered one fetch, so this is the second.
                token: FetchToken::new(2),
            }]
        );
    }

    #[test]
    fn help_and_message_overlays_open_and_close() {
        let mut app = app();

        app.on_key(key(KeyCode::Char('?')));
        assert_eq!(app.overlay, Overlay::Help);
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.overlay, Overlay::None);

        app.on_key(key(KeyCode::Char('m')));
        assert_eq!(app.overlay, Overlay::Messages);
        // The same key closes it again.
        app.on_key(key(KeyCode::Char('m')));
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn an_overlay_swallows_navigation_keys() {
        // Otherwise the list scrolls invisibly behind the help.
        let mut app = app();
        app.on_event(groups_arrived(some_groups()));
        app.on_key(key(KeyCode::Char('?')));

        app.on_key(key(KeyCode::Down));
        assert_eq!(app.group_cursor, 0);
        assert_eq!(app.overlay, Overlay::Help);
    }

    #[test]
    fn q_and_ctrl_c_both_quit_and_ask_the_worker_to_stop() {
        let mut with_q = app();
        assert_eq!(
            with_q.on_key(key(KeyCode::Char('q'))),
            vec![Request::Shutdown]
        );
        assert!(with_q.should_quit);

        let mut with_ctrl_c = app();
        assert_eq!(with_ctrl_c.on_key(ctrl('c')), vec![Request::Shutdown]);
        assert!(with_ctrl_c.should_quit);
    }

    #[test]
    fn ctrl_c_quits_even_from_an_overlay_or_the_filter() {
        let mut app = app();
        app.on_key(key(KeyCode::Char('?')));
        assert_eq!(app.on_key(ctrl('c')), vec![Request::Shutdown]);
        assert!(app.should_quit);
    }

    #[test]
    fn q_while_typing_a_filter_is_a_letter_not_a_command() {
        let mut app = app();
        app.on_key(key(KeyCode::Char('/')));
        app.on_key(key(KeyCode::Char('q')));

        assert!(!app.should_quit);
        assert_eq!(app.filter, "q");
    }

    #[test]
    fn errors_are_shown_then_dismissed_by_the_next_keystroke() {
        let mut app = app();
        app.inflight = 1;
        app.on_event(Event::Failed {
            context: "LIST".to_owned(),
            message: "timed out".to_owned(),
        });

        assert!(
            app.error
                .as_deref()
                .is_some_and(|e| e.contains("timed out"))
        );
        assert_eq!(app.inflight, 0);
        assert!(app.messages.iter().any(|m| m.contains("timed out")));

        app.on_key(key(KeyCode::Tab));
        assert!(app.error.is_none());
        // The message log keeps it, though.
        assert!(app.messages.iter().any(|m| m.contains("timed out")));
    }

    #[test]
    fn a_disconnection_clears_the_outstanding_count() {
        // Otherwise the spinner spins forever after the connection drops.
        let mut app = app();
        app.inflight = 3;
        app.on_event(Event::Disconnected("reset by peer".to_owned()));

        assert_eq!(app.inflight, 0);
        assert!(!app.connected);
        assert_eq!(app.spinner_frame(), ' ');
    }

    #[test]
    fn the_spinner_only_turns_while_something_is_outstanding() {
        let mut app = app();
        assert_eq!(app.spinner_frame(), ' ');

        app.inflight = 1;
        let first = app.spinner_frame();
        app.tick();
        assert_ne!(app.spinner_frame(), first);

        app.inflight = 0;
        app.tick();
        assert_eq!(app.spinner_frame(), ' ');
    }

    #[test]
    fn connection_details_reach_the_status_bar() {
        let mut app = app();
        app.on_event(Event::Connected {
            server: "news.example.org:563".to_owned(),
            greeting: "ready".to_owned(),
            encrypted: true,
        });

        assert!(app.connected);
        assert!(app.encrypted);
        assert_eq!(app.server, "news.example.org:563");
        assert_eq!(app.status, "ready");
    }

    #[test]
    fn the_message_log_is_bounded() {
        let mut app = app();
        for n in 0..LOG_CAPACITY + 50 {
            app.note(format!("message {n}"));
        }
        assert_eq!(app.messages.len(), LOG_CAPACITY);
        // Newest first.
        assert!(
            app.messages
                .front()
                .is_some_and(|m| m.contains(&(LOG_CAPACITY + 49).to_string()))
        );
    }

    #[test]
    fn an_unbound_key_does_not_force_a_redraw() {
        // Redrawing on every keystroke wastes a terminal's worth of output for nothing.
        let mut app = app();
        app.dirty = false;
        app.on_key(key(KeyCode::Char('Z')));
        assert!(!app.dirty);
    }

    #[test]
    fn acting_on_an_empty_list_is_harmless() {
        let mut app = app();
        assert!(app.on_key(key(KeyCode::Enter)).is_empty());
        assert!(app.on_key(key(KeyCode::Down)).is_empty());
        assert!(app.on_key(key(KeyCode::Char('n'))).is_empty());
        assert!(app.selected_group().is_none());
        assert!(app.selected_article().is_none());

        app.focus = Pane::Articles;
        assert!(app.on_key(key(KeyCode::Enter)).is_empty());
        app.focus = Pane::Body;
        assert!(app.on_key(key(KeyCode::Enter)).is_empty());
        assert!(app.on_key(key(KeyCode::Char('r'))).is_empty());
    }

    #[test]
    fn pane_titles_and_cycling_are_consistent() {
        assert_eq!(Pane::Groups.title(), "Groups");
        assert_eq!(Pane::Groups.next().previous(), Pane::Groups);
        assert_eq!(Pane::Body.next(), Pane::Groups);
        assert_eq!(Pane::Groups.previous(), Pane::Body);
    }

    #[test]
    fn shift_clamps_at_both_ends_and_copes_with_an_empty_list() {
        assert_eq!(shift(0, 1, 3), 1);
        assert_eq!(shift(2, 1, 3), 2);
        assert_eq!(shift(0, -1, 3), 0);
        assert_eq!(shift(0, 5, 3), 2);
        assert_eq!(shift(5, -10, 3), 0);
        assert_eq!(shift(0, 1, 0), 0);
    }
}
