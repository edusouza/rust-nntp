//! The terminal reader, driven end to end against a real socket and a fake news server —
//! everything except the terminal itself.
//!
//! The state machine and the network worker are exercised together here: requests really
//! go over TCP, real responses come back, and the resulting state is what a user would be
//! looking at. Rendering is covered separately by the `TestBackend` tests inside
//! `tui::ui`; between the two, the only untested part of the reader is the twenty-line
//! poll loop.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use nntp_testserver::{CapabilityProfile, Corpus, Quirks, ServerConfig, TestServer};
use nntp_tui::cli::ServerArgs;
use nntp_tui::config::{Config, SecurityConfig};
use nntp_tui::session::{self, Target};
use nntp_tui::tui::app::{App, Overlay, Pane};
use nntp_tui::tui::protocol::{Event, Request};
use nntp_tui::tui::worker;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// How long to wait for the worker before declaring a test failed.
const TIMEOUT: Duration = Duration::from_secs(10);

/// A reader wired up to a server, minus the terminal.
struct Harness {
    app: App,
    requests: Sender<Request>,
    events: Receiver<Event>,
    _server: TestServer,
}

impl Harness {
    fn new(server: TestServer) -> Self {
        let target = target_for(&server);
        let (request_tx, request_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();

        worker::spawn(target, 500, request_rx, event_tx).expect("spawn the worker");

        let mut app = App::new(&Config::default().ui);
        let initial = app.initial_requests();
        for request in initial {
            request_tx.send(request).expect("send");
        }

        Self {
            app,
            requests: request_tx,
            events: event_rx,
            _server: server,
        }
    }

    /// Feeds worker events to the app until `done` is satisfied, or the timeout expires.
    fn settle(&mut self, what: &str, done: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + TIMEOUT;

        while Instant::now() < deadline {
            if done(&self.app) {
                return;
            }
            match self.events.recv_timeout(Duration::from_millis(100)) {
                Ok(event) => self.app.on_event(event),
                Err(RecvTimeoutError::Timeout) => self.app.tick(),
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        assert!(
            done(&self.app),
            "timed out waiting for {what}\n  status: {}\n  error: {:?}\n  messages: {:?}",
            self.app.status,
            self.app.error,
            self.app.messages
        );
    }

    /// Presses a key and forwards whatever requests it produced.
    fn press(&mut self, code: KeyCode) {
        for request in self.app.on_key(KeyEvent::new(code, KeyModifiers::NONE)) {
            self.requests.send(request).expect("send");
        }
    }

    fn type_text(&mut self, text: &str) {
        for character in text.chars() {
            self.press(KeyCode::Char(character));
        }
    }
}

fn target_for(server: &TestServer) -> Target {
    let args = ServerArgs {
        host: Some("127.0.0.1".to_owned()),
        port: Some(server.port()),
        no_tls: true,
        ..ServerArgs::default()
    };
    let target = session::resolve(&Config::default(), &args).expect("resolve");
    assert_eq!(target.server.security, SecurityConfig::Plain);
    target
}

fn serve(config: ServerConfig) -> TestServer {
    TestServer::with(Corpus::sample(), config).expect("start the server")
}

#[test]
fn loads_the_group_list_on_start_up() {
    let mut harness = Harness::new(serve(ServerConfig::new()));
    harness.settle("the group list", |app| !app.groups.is_empty());

    assert!(harness.app.connected);
    assert!(!harness.app.encrypted);
    assert_eq!(harness.app.groups.len(), 4);
    assert_eq!(harness.app.inflight, 0);

    // Descriptions come from a second command and are merged into the same rows.
    let rust = harness
        .app
        .groups
        .iter()
        .find(|group| group.name.as_str() == "comp.lang.rust")
        .expect("comp.lang.rust");
    assert_eq!(
        rust.description.as_deref(),
        Some("Discussion of the Rust programming language")
    );
    // Sparse numbering: the watermarks span six numbers for two articles.
    assert_eq!(rust.article_bound(), 6);
}

#[test]
fn opens_a_group_and_then_an_article() {
    let mut harness = Harness::new(serve(ServerConfig::new()));
    harness.settle("the group list", |app| !app.groups.is_empty());

    // Filter down to one group rather than relying on the server's ordering.
    harness.press(KeyCode::Char('/'));
    harness.type_text("misc.test");
    harness.press(KeyCode::Enter);
    assert_eq!(harness.app.visible_groups().len(), 1);

    harness.press(KeyCode::Enter);
    harness.settle("the article list", |app| !app.articles.is_empty());

    assert_eq!(harness.app.focus, Pane::Articles);
    assert_eq!(harness.app.articles.len(), 3);
    // The newest article is selected, because that is what a reader wants first.
    assert_eq!(harness.app.selected_article().map(|r| r.number), Some(3));
    assert_eq!(
        harness.app.group.as_ref().map(|g| g.name.as_str()),
        Some("misc.test")
    );

    // Open the article under the cursor.
    harness.press(KeyCode::Enter);
    harness.settle("the article", |app| app.article.is_some());

    let view = harness.app.article.as_ref().expect("an article");
    assert_eq!(view.number, Some(3));
    assert_eq!(view.subject, "An article with an unreadable date");
    // The Date header is unparseable, so the raw text is shown rather than nothing.
    assert_eq!(view.date, "yesterday afternoon");
    assert_eq!(harness.app.focus, Pane::Body);
}

#[test]
fn walks_the_article_list_with_n_and_p() {
    let mut harness = Harness::new(serve(ServerConfig::new()));
    harness.settle("the group list", |app| !app.groups.is_empty());

    harness.press(KeyCode::Char('/'));
    harness.type_text("misc.test");
    harness.press(KeyCode::Enter);
    harness.press(KeyCode::Enter);
    harness.settle("the article list", |app| !app.articles.is_empty());

    // Start at the oldest, then walk forwards.
    harness.press(KeyCode::Char('g'));
    harness.press(KeyCode::Enter);
    harness.settle("the first article", |app| {
        app.article
            .as_ref()
            .is_some_and(|view| view.number == Some(1))
    });
    assert_eq!(
        harness.app.article.as_ref().map(|v| v.subject.clone()),
        Some("A plain test article".to_owned())
    );

    harness.press(KeyCode::Char('n'));
    harness.settle("the second article", |app| {
        app.article
            .as_ref()
            .is_some_and(|view| view.number == Some(2))
    });

    let view = harness.app.article.as_ref().expect("an article");
    // The reply's body contains a dot-stuffed line and a signature separator.
    assert!(
        view.body
            .iter()
            .any(|line| line == ".signature-like line that starts with a dot"),
        "{:?}",
        view.body
    );
    assert!(
        view.body.iter().any(|line| line == "-- "),
        "{:?}",
        view.body
    );
    assert!(
        view.body.iter().any(|line| line.starts_with('>')),
        "{:?}",
        view.body
    );
}

#[test]
fn decodes_encoded_subjects_and_a_quoted_printable_body() {
    let mut harness = Harness::new(serve(ServerConfig::new()));
    harness.settle("the group list", |app| !app.groups.is_empty());

    harness.press(KeyCode::Char('/'));
    harness.type_text("comp.lang.rust");
    harness.press(KeyCode::Enter);
    harness.press(KeyCode::Enter);
    harness.settle("the article list", |app| !app.articles.is_empty());

    assert_eq!(harness.app.articles.len(), 2);
    assert_eq!(harness.app.articles[0].subject, "café and crates");
    assert_eq!(harness.app.articles[1].subject, "Re: café and crates");
    assert!(harness.app.articles[1].is_reply());

    harness.press(KeyCode::Enter);
    harness.settle("the article", |app| app.article.is_some());

    let view = harness.app.article.as_ref().expect("an article");
    assert_eq!(view.author, "Åsa Lindqvist <asa@example.se>");
    assert!(
        view.body.iter().any(|line| line.contains("café.")),
        "{:?}",
        view.body
    );
    // A soft line break was joined, so the two source lines became one.
    assert!(
        view.body
            .iter()
            .any(|line| line.contains("break follows here and this continues")),
        "{:?}",
        view.body
    );
}

#[test]
fn an_empty_group_is_reported_rather_than_opened() {
    let mut harness = Harness::new(serve(ServerConfig::new()));
    harness.settle("the group list", |app| !app.groups.is_empty());

    harness.press(KeyCode::Char('/'));
    harness.type_text("empty");
    harness.press(KeyCode::Enter);
    harness.press(KeyCode::Enter);

    assert!(
        harness.app.status.contains("empty"),
        "{}",
        harness.app.status
    );
    assert!(harness.app.articles.is_empty());
    assert_eq!(harness.app.inflight, 0);
}

#[test]
fn refreshing_the_article_list_reloads_it_from_the_server() {
    let mut harness = Harness::new(serve(ServerConfig::new()));
    harness.settle("the group list", |app| !app.groups.is_empty());

    harness.press(KeyCode::Char('/'));
    harness.type_text("misc.test");
    harness.press(KeyCode::Enter);
    harness.press(KeyCode::Enter);
    harness.settle("the article list", |app| !app.articles.is_empty());

    harness.app.articles.clear();
    harness.press(KeyCode::Char('r'));
    harness.settle("the reloaded list", |app| app.articles.len() == 3);
}

#[test]
fn works_against_a_server_that_predates_capabilities() {
    let mut harness = Harness::new(serve(
        ServerConfig::new().capabilities(CapabilityProfile::Legacy),
    ));
    harness.settle("the group list", |app| !app.groups.is_empty());

    harness.press(KeyCode::Char('/'));
    harness.type_text("misc.test");
    harness.press(KeyCode::Enter);
    harness.press(KeyCode::Enter);
    harness.settle("the article list", |app| !app.articles.is_empty());

    assert_eq!(harness.app.articles.len(), 3);
}

#[test]
fn works_against_a_server_with_no_over_command() {
    let mut harness = Harness::new(serve(
        ServerConfig::new().capabilities(CapabilityProfile::NoOver),
    ));
    harness.settle("the group list", |app| !app.groups.is_empty());

    harness.press(KeyCode::Char('/'));
    harness.type_text("misc.test");
    harness.press(KeyCode::Enter);
    harness.press(KeyCode::Enter);
    harness.settle("the article list", |app| !app.articles.is_empty());

    assert_eq!(harness.app.articles.len(), 3);
}

#[test]
fn a_failure_is_shown_in_the_interface_rather_than_ending_the_session() {
    // The server truncates its first block and hangs up, which is the worst case short
    // of the process dying: the group list never arrives.
    let mut harness = Harness::new(serve(ServerConfig::new().quirks(Quirks {
        truncate_next_block: true,
        ..Quirks::default()
    })));

    harness.settle("an error", |app| app.error.is_some());

    assert!(harness.app.groups.is_empty());
    assert!(!harness.app.should_quit, "the interface must stay usable");
    // The spinner stops, rather than turning forever on a request that will never finish.
    assert_eq!(harness.app.inflight, 0);
    assert!(
        harness
            .app
            .messages
            .iter()
            .any(|m| m.contains("group list")),
        "{:?}",
        harness.app.messages
    );

    // And the interface still responds: help opens, keys work, the error clears.
    harness.press(KeyCode::Char('?'));
    assert_eq!(harness.app.overlay, Overlay::Help);
    assert!(harness.app.error.is_none());
}

#[test]
fn the_worker_reconnects_after_the_connection_drops() {
    // The server serves a few commands and then vanishes without a word. Loading the
    // group list again must work, because the worker reconnects instead of giving up.
    let mut harness = Harness::new(serve(ServerConfig::new().quirks(Quirks {
        close_after_commands: Some(4),
        ..Quirks::default()
    })));

    harness.settle("the first group list", |app| !app.groups.is_empty());

    // Force a second load, which the server will not survive.
    harness.press(KeyCode::Char('r'));
    harness.settle("a disconnection", |app| !app.connected);

    // Now ask again: the worker opens a new connection, and this one has a fresh command
    // budget.
    harness.app.groups.clear();
    harness.press(KeyCode::Char('r'));
    harness.settle("a reconnection and a fresh group list", |app| {
        app.connected && !app.groups.is_empty()
    });

    assert_eq!(harness.app.groups.len(), 4);
}

#[test]
fn authenticates_before_reading_when_the_server_demands_it() {
    let mut harness = Harness::new(serve(ServerConfig::new().require_auth("bob", "hunter2")));

    // No credentials are configured, so reading is refused and the interface says so
    // rather than showing an empty list with no explanation.
    harness.settle("an authentication error", |app| app.error.is_some());
    assert!(
        harness
            .app
            .messages
            .iter()
            .any(|m| m.to_lowercase().contains("auth")),
        "{:?}",
        harness.app.messages
    );
}

#[test]
fn credentials_from_the_configuration_are_used() {
    let server = serve(ServerConfig::new().require_auth("bob", "hunter2"));

    let args = ServerArgs {
        host: Some("127.0.0.1".to_owned()),
        port: Some(server.port()),
        no_tls: true,
        username: Some("bob".to_owned()),
        password_command: Some(if cfg!(windows) {
            "echo hunter2".to_owned()
        } else {
            "printf hunter2".to_owned()
        }),
        allow_plaintext_auth: true,
        ..ServerArgs::default()
    };
    let target = session::resolve(&Config::default(), &args).expect("resolve");

    let (request_tx, request_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    worker::spawn(target, 500, request_rx, event_tx).expect("spawn");

    let mut app = App::new(&Config::default().ui);
    for request in app.initial_requests() {
        request_tx.send(request).expect("send");
    }

    let mut harness = Harness {
        app,
        requests: request_tx,
        events: event_rx,
        _server: server,
    };

    harness.settle("the group list", |app| !app.groups.is_empty());
    assert!(harness.app.error.is_none(), "{:?}", harness.app.error);
    assert_eq!(harness.app.groups.len(), 4);
}

#[test]
fn quitting_stops_the_worker() {
    let mut harness = Harness::new(serve(ServerConfig::new()));
    harness.settle("the group list", |app| !app.groups.is_empty());

    harness.press(KeyCode::Char('q'));
    assert!(harness.app.should_quit);

    // The worker acknowledges by closing the event channel.
    let deadline = Instant::now() + TIMEOUT;
    let mut stopped = false;
    while Instant::now() < deadline && !stopped {
        match harness.events.recv_timeout(Duration::from_millis(100)) {
            Ok(Event::Stopped) => stopped = true,
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                stopped = true;
            }
        }
    }
    assert!(stopped, "the worker did not stop");
}
