//! The interface state machine.
//!
//! Deliberately free of both terminal and network: it takes key events and worker events
//! in, and produces state changes and [`Request`]s out. Nothing here draws anything and
//! nothing here blocks. That is what makes the behaviour testable — the tests at the
//! bottom of this file drive the whole reader without a terminal or a socket.

use std::collections::VecDeque;

use nntp_proto::{ArticleSpec, GroupSummary, OverviewRecord, Range};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::UiConfig;
use crate::tui::protocol::{Event, GroupRow, Request};

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
    /// The body, split into lines.
    pub body: Vec<String>,
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

        Self {
            number: article.number,
            subject: article.subject(),
            author: article.author(),
            date,
            headers,
            body: article.body_text().lines().map(str::to_owned).collect(),
        }
    }
}

/// The whole interface state.
#[derive(Debug)]
pub struct App {
    /// Which pane has focus.
    pub focus: Pane,
    /// What is covering the layout.
    pub overlay: Overlay,

    /// Every group the server carries.
    pub groups: Vec<GroupRow>,
    /// Index into [`Self::visible_groups`], not into [`Self::groups`].
    pub group_cursor: usize,
    /// The substring groups are filtered by.
    pub filter: String,
    /// Whether the filter is being typed.
    pub editing_filter: bool,

    /// The selected group, once the server has confirmed it.
    pub group: Option<GroupSummary>,
    /// Overview records for the selected group, newest last.
    pub articles: Vec<OverviewRecord>,
    /// Index into [`Self::articles`].
    pub article_cursor: usize,

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

    /// How many requests are outstanding, for the spinner.
    pub inflight: usize,
    /// Spinner phase.
    pub spinner: usize,
    /// Whether the screen needs redrawing.
    pub dirty: bool,
    /// Whether to stop.
    pub should_quit: bool,

    /// How many of a group's newest articles to load.
    initial_articles: u64,
    /// `strftime` format for dates.
    date_format: String,
    /// Height of the article list, so page keys can move by a screenful.
    pub list_height: usize,
    /// Height of the body pane, for the same reason.
    pub body_height: usize,
}

impl App {
    /// A fresh interface, with the group list already requested.
    pub fn new(config: &UiConfig) -> Self {
        Self {
            focus: Pane::Groups,
            overlay: Overlay::None,
            groups: Vec::new(),
            group_cursor: 0,
            filter: String::new(),
            editing_filter: false,
            group: None,
            articles: Vec::new(),
            article_cursor: 0,
            article: None,
            body_scroll: 0,
            server: String::new(),
            encrypted: false,
            connected: false,
            status: "connecting…".to_owned(),
            error: None,
            messages: VecDeque::new(),
            inflight: 0,
            spinner: 0,
            dirty: true,
            should_quit: false,
            initial_articles: config.initial_articles,
            date_format: config.date_format.clone(),
            list_height: 20,
            body_height: 20,
        }
    }

    /// The requests to send when the interface starts.
    pub fn initial_requests(&mut self) -> Vec<Request> {
        self.inflight += 1;
        vec![Request::LoadGroups]
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

    /// The overview record under the cursor.
    pub fn selected_article(&self) -> Option<&OverviewRecord> {
        self.articles.get(self.article_cursor)
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
    fn note(&mut self, message: impl Into<String>) {
        let message = message.into();
        tracing::debug!(%message, "ui message");
        self.messages.push_front(message);
        self.messages.truncate(LOG_CAPACITY);
        self.dirty = true;
    }

    /// Applies an event from the worker.
    pub fn on_event(&mut self, event: Event) {
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

            Event::Groups(groups) => {
                self.inflight = self.inflight.saturating_sub(1);
                self.status = format!("{} groups", groups.len());
                self.groups = groups;
                self.group_cursor = 0;
            }

            Event::GroupOpened(summary) => {
                self.status = format!(
                    "{}: {} articles, {}..{}",
                    summary.name, summary.estimated_count, summary.low, summary.high
                );
                self.group = Some(*summary);
            }

            Event::Overview {
                group,
                records,
                skipped,
            } => {
                self.inflight = self.inflight.saturating_sub(1);

                // A reply for a group the user has navigated away from must not be shown
                // under the current group's heading.
                let current = self.group.as_ref().map(|summary| &summary.name);
                if current != Some(&group) {
                    self.note(format!("discarded a late overview reply for {group}"));
                    return;
                }

                if skipped > 0 {
                    self.note(format!(
                        "{group}: {skipped} overview line(s) could not be parsed"
                    ));
                }

                self.articles = records;
                // Newest last, and the newest is what a reader wants to see first.
                self.article_cursor = self.articles.len().saturating_sub(1);
                self.focus = Pane::Articles;
                self.status = format!("{}: {} articles listed", group, self.articles.len());
            }

            Event::Article(article) => {
                self.inflight = self.inflight.saturating_sub(1);
                let view = ArticleView::new(&article, &self.date_format);
                self.status = format!("{} — {}", view.date, view.subject);
                self.article = Some(view);
                self.body_scroll = 0;
                self.focus = Pane::Body;
            }

            Event::Progress(message) => self.status = message,

            Event::Failed { context, message } => {
                self.inflight = self.inflight.saturating_sub(1);
                let text = format!("{context}: {message}");
                self.error = Some(text.clone());
                self.note(text);
            }

            Event::Disconnected(reason) => {
                self.connected = false;
                self.inflight = 0;
                self.error = Some(format!("disconnected: {reason}"));
                self.note(format!("disconnected: {reason}"));
            }

            Event::Stopped => {
                self.connected = false;
                self.inflight = 0;
                self.note("the worker stopped");
            }
        }
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
                if self.filter.is_empty() {
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
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => self.overlay = Overlay::None,
            KeyCode::Char('?') | KeyCode::F(1) => {
                self.overlay = if self.overlay == Overlay::Help {
                    Overlay::None
                } else {
                    Overlay::Help
                };
            }
            KeyCode::Char('m') => {
                self.overlay = if self.overlay == Overlay::Messages {
                    Overlay::None
                } else {
                    Overlay::Messages
                };
            }
            _ => self.dirty = false,
        }
        Vec::new()
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
            KeyCode::Char(character) => {
                self.filter.push(character);
                self.group_cursor = 0;
            }
            _ => self.dirty = false,
        }
        Vec::new()
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
                    self.article = None;
                    return Vec::new();
                }

                self.articles.clear();
                self.article = None;
                self.article_cursor = 0;
                self.status = format!("opening {}…", group.name);
                self.inflight += 1;

                vec![Request::OpenGroup {
                    group: group.name,
                    count: self.initial_articles,
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
        if self.articles.is_empty() {
            return Vec::new();
        }

        let next = self.article_cursor as isize + delta;
        if next < 0 || next as usize >= self.articles.len() {
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
                self.status = "reloading groups…".to_owned();
                self.inflight += 1;
                vec![Request::LoadGroups]
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
                self.status = format!("reloading {}…", summary.name);
                self.inflight += 1;

                vec![Request::LoadOverview {
                    group: summary.name.clone(),
                    range: Range::between(first, high),
                }]
            }
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        match self.focus {
            Pane::Groups => {
                let count = self.visible_groups().len();
                self.group_cursor = shift(self.group_cursor, delta, count);
            }
            Pane::Articles => {
                self.article_cursor = shift(self.article_cursor, delta, self.articles.len());
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
            Pane::Articles => self.article_cursor = self.articles.len().saturating_sub(1),
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

    use super::*;

    fn app() -> App {
        App::new(&UiConfig::default())
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

    /// Drives an app through to a group with articles listed.
    fn app_with_articles() -> App {
        let mut app = app();
        app.on_event(Event::Groups(some_groups()));
        app.on_event(Event::GroupOpened(Box::new(summary("misc.test", 1, 3, 3))));
        app.on_event(Event::Overview {
            group: GroupName::parse("misc.test").unwrap(),
            records: vec![
                record(1, "oldest"),
                record(2, "middle"),
                record(3, "newest"),
            ],
            skipped: 0,
        });
        app
    }

    #[test]
    fn asks_for_the_group_list_on_start_up() {
        let mut app = app();
        assert_eq!(app.initial_requests(), vec![Request::LoadGroups]);
        assert_eq!(app.inflight, 1);
    }

    #[test]
    fn shows_the_groups_it_is_given() {
        let mut app = app();
        app.on_event(Event::Groups(some_groups()));

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
        app.on_event(Event::Groups(some_groups()));

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
        app.on_event(Event::Groups(some_groups()));

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
        app.on_event(Event::Groups(some_groups()));
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
    fn filtering_narrows_the_list_by_name_or_description() {
        let mut app = app();
        app.on_event(Event::Groups(some_groups()));

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
    fn filtering_is_case_insensitive() {
        let mut app = app();
        app.on_event(Event::Groups(some_groups()));
        app.filter = "RUST".to_owned();
        assert_eq!(app.visible_groups().len(), 1);
    }

    #[test]
    fn backspace_widens_the_filter_again() {
        let mut app = app();
        app.on_event(Event::Groups(some_groups()));
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
        app.on_event(Event::Groups(some_groups()));
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
        app.on_event(Event::Groups(some_groups()));
        app.filter = "misc".to_owned();
        app.on_key(key(KeyCode::Esc));
        assert!(app.filter.is_empty());
    }

    #[test]
    fn the_cursor_stays_in_range_when_a_filter_shrinks_the_list() {
        // Otherwise the cursor points past the end and the selected group is None.
        let mut app = app();
        app.on_event(Event::Groups(some_groups()));
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
        app.on_event(Event::Groups(some_groups()));
        let requests = app.on_key(key(KeyCode::Enter));

        assert_eq!(
            requests,
            vec![Request::OpenGroup {
                group: GroupName::parse("comp.lang.rust").unwrap(),
                count: UiConfig::default().initial_articles,
            }]
        );
        assert_eq!(app.inflight, 1);
    }

    #[test]
    fn opening_an_empty_group_sends_nothing() {
        let mut app = app();
        app.on_event(Event::Groups(some_groups()));
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
        app.on_event(Event::Overview {
            group: GroupName::parse("comp.lang.rust").unwrap(),
            records: vec![record(99, "from the wrong group")],
            skipped: 0,
        });

        assert_eq!(app.articles.len(), 3);
        assert!(
            app.messages.iter().any(|m| m.contains("discarded")),
            "{:?}",
            app.messages
        );
    }

    #[test]
    fn unparseable_overview_lines_are_reported_not_hidden() {
        let mut app = app();
        app.on_event(Event::GroupOpened(Box::new(summary("misc.test", 1, 3, 3))));
        app.on_event(Event::Overview {
            group: GroupName::parse("misc.test").unwrap(),
            records: vec![record(1, "fine")],
            skipped: 2,
        });

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
            vec![Request::LoadGroups]
        );

        app.focus = Pane::Articles;
        let requests = app.on_key(key(KeyCode::Char('r')));
        assert_eq!(
            requests,
            vec![Request::LoadOverview {
                group: GroupName::parse("misc.test").unwrap(),
                range: Range::between(1, 3),
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
        app.on_event(Event::Groups(some_groups()));
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
