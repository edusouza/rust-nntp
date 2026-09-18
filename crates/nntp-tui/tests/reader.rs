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

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use nntp_testserver::{CapabilityProfile, Corpus, Quirks, ServerConfig, TestServer};
use nntp_tui::cli::ServerArgs;
use nntp_tui::config::{Config, SecurityConfig};
use nntp_tui::readstate::ReadStore;
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
        Self::with_read_state(server, ReadStore::empty(PathBuf::from("unused")))
    }

    fn with_read_state(server: TestServer, read: ReadStore) -> Self {
        let target = target_for(&server);
        let (request_tx, request_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();

        let mut app = App::new(&Config::default().ui, read);

        // The harness shares the app's cancel flag with the worker, exactly as the run
        // loop does, so a test can cancel the way a keystroke would.
        worker::spawn(target, 500, request_rx, event_tx, app.cancel.clone())
            .expect("spawn the worker");

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

/// A server whose corpus also carries the MIME traffic a reader has to survive.
fn serve_mime() -> TestServer {
    TestServer::with(Corpus::sample_with_mime(), ServerConfig::new()).expect("start the server")
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
fn read_state_survives_a_restart() {
    // The acceptance criterion of #7, end to end: read an article through a real socket,
    // save, start a second reader against the same server, and find the article still
    // read and the unread count one lower.
    let directory = std::env::temp_dir().join(format!("nntp-tui-reader-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    let path = directory.join("127.0.0.1.newsrc");

    let mut harness =
        Harness::with_read_state(serve(ServerConfig::new()), ReadStore::empty(path.clone()));
    harness.settle("the group list", |app| !app.groups.is_empty());

    harness.press(KeyCode::Char('/'));
    harness.type_text("misc.test");
    harness.press(KeyCode::Enter);
    harness.press(KeyCode::Enter);
    harness.settle("the article list", |app| !app.articles.is_empty());

    let unread_before = harness
        .app
        .articles
        .iter()
        .filter(|record| harness.app.is_unread(record.number))
        .count();
    assert_eq!(unread_before, 3, "nothing has been read yet");

    harness.press(KeyCode::Enter);
    harness.settle("the article", |app| app.article.is_some());
    let number = harness
        .app
        .article
        .as_ref()
        .and_then(|view| view.number)
        .expect("the article has a number");
    assert!(!harness.app.is_unread(number), "reading it marked it read");

    // What the run loop does on the way out.
    harness.app.read.save_if_dirty().expect("save read state");
    assert!(path.exists(), "the store was written to {}", path.display());

    // A second reader, as if the program had been restarted.
    let (restored, problems) = ReadStore::load(path.clone());
    assert!(problems.is_empty(), "{problems:?}");

    let mut second = Harness::with_read_state(serve(ServerConfig::new()), restored);
    second.settle("the group list", |app| !app.groups.is_empty());
    second.press(KeyCode::Char('/'));
    second.type_text("misc.test");
    second.press(KeyCode::Enter);
    second.press(KeyCode::Enter);
    second.settle("the article list", |app| !app.articles.is_empty());

    assert!(
        !second.app.is_unread(number),
        "article {number} should still be read after a restart"
    );
    let unread_after = second
        .app
        .articles
        .iter()
        .filter(|record| second.app.is_unread(record.number))
        .count();
    assert_eq!(unread_after, unread_before - 1);

    // And the unread filter now has something to hide.
    second.press(KeyCode::Char('u'));
    assert_eq!(second.app.visible_articles().len(), unread_after);

    let _ = std::fs::remove_dir_all(&directory);
}

/// Opens `group`, selects the article whose subject contains `subject`, and returns the
/// harness with that article on display.
fn reader_showing(server: TestServer, group: &str, subject: &str) -> Harness {
    let mut harness = Harness::new(server);
    harness.settle("the group list", |app| !app.groups.is_empty());

    harness.press(KeyCode::Char('/'));
    harness.type_text(group);
    harness.press(KeyCode::Enter);
    harness.press(KeyCode::Enter);
    harness.settle("the article list", |app| !app.articles.is_empty());

    let index = harness
        .app
        .articles
        .iter()
        .position(|record| record.subject.contains(subject))
        .unwrap_or_else(|| {
            panic!(
                "no article matching {subject:?} in {:?}",
                harness
                    .app
                    .articles
                    .iter()
                    .map(|r| r.subject.clone())
                    .collect::<Vec<_>>()
            )
        });
    harness.app.article_cursor = index;

    harness.press(KeyCode::Enter);
    harness.settle("the article", |app| app.article.is_some());
    harness
}

#[test]
fn a_long_request_can_be_cancelled_and_the_reader_carries_on() {
    // The whole of #9, end to end: a response that is still arriving, a keystroke, and a
    // reader that is usable immediately afterwards.
    //
    // The delay is the fixture. A real `LIST ACTIVE` takes tens of seconds, which is why
    // cancelling matters and why no corpus small enough to ship reproduces it; 40ms a line
    // makes the group list take long enough to interrupt on purpose.
    let server = TestServer::with(
        Corpus::sample(),
        ServerConfig::new().quirks(Quirks {
            line_delay: Some(Duration::from_millis(40)),
            ..Quirks::default()
        }),
    )
    .expect("start the server");

    let mut harness = Harness::new(server);

    // The group list is in flight; stop it.
    harness.settle("the request to be outstanding", |app| app.inflight > 0);
    harness.press(KeyCode::Esc);
    harness.settle("the cancellation", |app| app.inflight == 0);

    assert!(
        harness.app.groups.is_empty(),
        "the group list was abandoned, not delivered"
    );
    assert!(
        harness.app.status.contains("cancelled"),
        "status: {}",
        harness.app.status
    );
    assert!(
        harness.app.error.is_none(),
        "cancelling is not a failure: {:?}",
        harness.app.error
    );

    // And the reader still works. The cancelled connection was dropped — stopping
    // mid-response leaves it desynchronised — so this reconnects, which the user sees as
    // a progress line rather than as an error.
    harness.press(KeyCode::Char('r'));
    harness.settle("the group list on a fresh connection", |app| {
        !app.groups.is_empty()
    });

    assert_eq!(harness.app.groups.len(), 4);
    assert!(harness.app.connected);
    assert!(
        harness.app.error.is_none(),
        "the reconnection was clean: {:?}",
        harness.app.error
    );
}

#[test]
fn cancelling_takes_effect_while_the_response_is_still_arriving() {
    // Not "eventually": the flag is checked once per line, so the abandonment has to
    // happen while the server is still sending rather than after the last line.
    let server = TestServer::with(
        Corpus::sample(),
        ServerConfig::new().quirks(Quirks {
            // 26 groups and descriptions at 120ms each would be seconds of response.
            line_delay: Some(Duration::from_millis(120)),
            ..Quirks::default()
        }),
    )
    .expect("start the server");

    let mut harness = Harness::new(server);
    harness.settle("the request to be outstanding", |app| app.inflight > 0);

    let asked_at = Instant::now();
    harness.press(KeyCode::Esc);
    harness.settle("the cancellation", |app| app.inflight == 0);
    let took = asked_at.elapsed();

    assert!(
        took < Duration::from_secs(2),
        "cancellation took {took:?}, which is long enough that it waited for the response"
    );
    assert!(harness.app.groups.is_empty());
}

#[test]
fn a_multipart_article_shows_its_text_and_names_its_other_parts() {
    let harness = reader_showing(serve_mime(), "news.software.readers", "multipart article");
    let view = harness.app.article.as_ref().expect("an article");
    let body = view.body.join("\n");

    // The readable alternative, decoded.
    assert!(body.contains("The readable version"), "{body}");
    assert!(body.contains("café"), "{body}");

    // And none of what a raw body would have shown.
    assert!(
        !body.contains("--outer"),
        "boundary line on screen:\n{body}"
    );
    assert!(
        !body.contains("--inner"),
        "boundary line on screen:\n{body}"
    );
    assert!(!body.contains("<html>"), "html tags on screen:\n{body}");
    assert!(
        !body.contains("Content-Type"),
        "part headers on screen:\n{body}"
    );
    // The preamble and epilogue belong to no part (RFC 2046 §5.1.1).
    assert!(!body.contains("preamble"), "{body}");
    assert!(!body.contains("epilogue"), "{body}");

    // The parts it passed over are named rather than hidden: the HTML alternative and
    // the patch.
    let named = view.attachments.join("\n");
    assert!(named.contains("text/html"), "{named}");
    assert!(named.contains("fix.patch"), "{named}");
    assert!(named.contains("octets"), "{named}");
}

#[test]
fn a_flowed_article_is_shown_as_paragraphs() {
    let harness = reader_showing(serve_mime(), "news.software.readers", "format=flowed");
    let view = harness.app.article.as_ref().expect("an article");
    let body = view.body.join("\n");

    // The sender's soft wraps are gone: one line, not three.
    assert!(
        body.contains(
            "wrapped by the sender at a narrow width, and should be shown as one paragraph"
        ),
        "{body}"
    );
    // The quoted paragraph flowed too, and did not absorb the reply.
    assert!(
        body.contains(
            "> The quoted part was wrapped too, and must not be joined to the reply below it."
        ),
        "{body}"
    );
    // `-- ` ends in a space and is still a hard break (RFC 3676 §4.3).
    assert!(
        view.body.iter().any(|line| line == "-- "),
        "the signature separator was flowed away:\n{body}"
    );
}

#[test]
fn a_signed_gateway_article_shows_the_content_and_not_the_signature_machinery() {
    // The shape a Debian `Accepted …` announcement arrives in, found by pointing the
    // reader at linux.debian.changes on a real server: multipart/signed with a detached
    // signature, and the content clearsigned inside the text part.
    let harness = reader_showing(serve_mime(), "news.software.readers", "Accepted nginx");
    let view = harness.app.article.as_ref().expect("an article");
    let body = view.body.join("\n");

    // The content.
    assert!(body.contains("Source: nginx"), "{body}");
    assert!(body.contains("CVE-2026-56434"), "{body}");
    // Dash-escaping undone, so a signed patch does not read `- --- a/file`.
    assert!(
        body.contains("--- a/src/http/ngx_http_ssi_module.c"),
        "{body}"
    );
    assert!(!body.contains("- --- a/src"), "{body}");

    // None of the machinery: not the armour, not the `Hash:` header, not the base64.
    assert!(!body.contains("BEGIN PGP"), "armour on screen:\n{body}");
    assert!(!body.contains("END PGP"), "armour on screen:\n{body}");
    assert!(
        !body.contains("Hash: SHA512"),
        "armour header on screen:\n{body}"
    );
    assert!(
        !body.contains("iQIzBAAB"),
        "signature base64 on screen:\n{body}"
    );

    // The fact is reported once, and the signature is not listed as an attachment: every
    // article from every signing gateway would otherwise carry that line.
    assert!(view.signed, "the article carries a signature");
    assert!(
        view.attachments.is_empty(),
        "the signature was named as an attachment: {:?}",
        view.attachments
    );
}

#[test]
fn an_article_that_is_only_an_attachment_says_so_rather_than_showing_nothing() {
    let harness = reader_showing(serve_mime(), "news.software.readers", "only an attachment");
    let view = harness.app.article.as_ref().expect("an article");

    // A blank pane reads as a bug; this reads as a fact about the article.
    assert_eq!(view.body, ["(no text in this article)"]);
    let named = view.attachments.join("\n");
    assert!(named.contains("application/octet-stream"), "{named}");
    // RFC 2231's `filename*`, decoded.
    assert!(named.contains("relatório.bin"), "{named}");
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

    let mut app = App::new(
        &Config::default().ui,
        ReadStore::empty(PathBuf::from("unused")),
    );
    worker::spawn(target, 500, request_rx, event_tx, app.cancel.clone()).expect("spawn");
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
