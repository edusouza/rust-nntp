//! Renders a representative screen and checks it against the copy in the documentation.
//!
//! A screenshot in a README rots quietly: the layout changes, nobody notices, and the
//! documentation ends up describing a program that no longer exists. This test renders
//! the same state the documentation shows and fails when they diverge.
//!
//! To update the documentation after a deliberate layout change:
//!
//! ```sh
//! UPDATE_SCREENSHOT=1 cargo test -p nntp-tui --test screenshot
//! ```

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use nntp_proto::{
    Article, DataBlock, GroupName, OverviewFmt, OverviewRecord, PostingStatus, StatusLine,
};
use nntp_tui::config::UiConfig;
use nntp_tui::tui::app::App;
use nntp_tui::tui::protocol::{Event, GroupRow};
use nntp_tui::tui::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// The file the documentation includes.
const SCREENSHOT: &str = "docs/reader-screen.txt";

const WIDTH: u16 = 100;
const HEIGHT: u16 = 22;

fn group(name: &str, low: u64, high: u64, description: &str) -> GroupRow {
    GroupRow {
        name: GroupName::parse(name).unwrap(),
        low,
        high,
        status: PostingStatus::Permitted,
        description: Some(description.to_owned()),
    }
}

fn record(number: u64, subject: &str, from: &str, references: &str) -> OverviewRecord {
    let line = format!(
        "{number}\t{subject}\t{from}\tWed, 17 Sep 2026 08:00:00 +0000\t<{number}@example.org>\t{references}\t1830\t12"
    );
    OverviewRecord::parse(line.as_bytes(), &OverviewFmt::standard()).unwrap()
}

/// The state the screenshot shows: a group open, an article being read.
fn representative_app() -> App {
    let mut app = App::new(&UiConfig::default());

    app.on_event(Event::Connected {
        server: "news.example.org:563".to_owned(),
        greeting: "InterNetNews ready".to_owned(),
        encrypted: true,
    });

    app.on_event(Event::Groups(vec![
        group("comp.lang.c", 1, 18342, "Discussion about C"),
        group(
            "comp.lang.rust",
            4237,
            4242,
            "The Rust programming language",
        ),
        group("de.comp.test", 1, 97, "Deutschsprachige Testgruppe"),
        group("misc.test", 1, 3, "For testing purposes only"),
        group("news.announce.newgroups", 1, 0, "Calls for votes"),
    ]));
    app.group_cursor = 1;

    let summary = StatusLine::parse(b"211 2 4237 4242 comp.lang.rust").expect("status line");
    app.on_event(Event::GroupOpened(Box::new(
        nntp_proto::GroupSummary::parse(&summary, None).unwrap(),
    )));

    app.on_event(Event::Overview {
        group: GroupName::parse("comp.lang.rust").unwrap(),
        records: vec![
            record(4237, "café and crates", "bjorn@example.no", ""),
            record(
                4242,
                "Re: café and crates",
                "asa@example.se",
                "<4237@example.org>",
            ),
        ],
        skipped: 0,
    });

    let block = DataBlock::parse(
        b"From: =?UTF-8?B?w4VzYSBMaW5kcXZpc3Q=?= <asa@example.se>\r\n\
          Newsgroups: comp.lang.rust\r\n\
          Subject: =?UTF-8?B?UmU6IGNhZsOpIGFuZCBjcmF0ZXM=?=\r\n\
          Date: Wed, 17 Sep 2026 10:11:12 +0200\r\n\
          References: <4237@example.org>\r\n\
          \r\n\
          > On the subject of caf\xc3\xa9, I have this to say.\r\n\
          Agreed. The encoded-word handling is what makes this readable\r\n\
          at all: without it the subject above would be mojibake.\r\n\
          \r\n\
          -- \r\n\
          \xc3\x85sa\r\n.\r\n",
    );
    app.on_event(Event::Article(Box::new(
        Article::from_block(&block).with_number(4242),
    )));

    app
}

fn render(app: &mut App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("terminal");
    terminal.draw(|frame| ui::draw(frame, app)).expect("draw");

    terminal
        .backend()
        .buffer()
        .content()
        .chunks(usize::from(WIDTH))
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn screenshot_path() -> std::path::PathBuf {
    // CARGO_MANIFEST_DIR is crates/nntp-tui; the documentation lives at the workspace
    // root.
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join(SCREENSHOT)
}

#[test]
fn the_documented_screen_matches_what_the_reader_draws() {
    let rendered = format!("{}\n", render(&mut representative_app()));
    let path = screenshot_path();

    if std::env::var_os("UPDATE_SCREENSHOT").is_some() {
        std::fs::write(&path, &rendered).expect("write the screenshot");
        return;
    }

    let documented = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "cannot read {}: {error}\nrun with UPDATE_SCREENSHOT=1 to create it",
            path.display()
        )
    });

    assert_eq!(
        documented.replace("\r\n", "\n"),
        rendered,
        "\nthe reader's layout no longer matches {SCREENSHOT}.\n\
         If the change was deliberate, refresh it with:\n\
         \n    UPDATE_SCREENSHOT=1 cargo test -p nntp-tui --test screenshot\n"
    );
}

#[test]
fn the_representative_screen_shows_what_it_claims_to() {
    // Guards against a screenshot that is technically up to date and shows nothing
    // useful, which would make the test above pass while the documentation misleads.
    let screen = render(&mut representative_app());

    for expected in [
        "comp.lang.rust",
        "Re: café and crates",
        "Åsa Lindqvist",
        "news.example.org:563",
        "TLS",
        "≤",
        "q: quit",
    ] {
        assert!(
            screen.contains(expected),
            "{expected:?} missing from:\n{screen}"
        );
    }
}
