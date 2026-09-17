//! Opt-in tests against a *real* news server.
//!
//! Every other test in this workspace runs against `nntp-testserver`, which implements our
//! reading of the RFCs. A misunderstanding shared by client and fake server is invisible to
//! all of them. These tests are the only thing that can catch one, and they are
//! `#[ignore]`d because they need credentials and a network this project's CI does not have.
//!
//! # Running them
//!
//! ```sh
//! export NNTP_TEST_HOST=news.eternal-september.org
//! export NNTP_TEST_USER=your-username
//! read -rs -p 'password: ' NNTP_TEST_PASS; export NNTP_TEST_PASS; echo
//! export NNTP_TEST_GROUP=comp.lang.rust
//!
//! cargo test -p nntp-client --test real_server -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `--nocapture` matters: these tests print what they found, and the counts are the
//! evidence. `--test-threads=1` matters too — a public server will refuse a handful of
//! simultaneous connections from one address.
//!
//! | Variable | Default | Meaning |
//! | --- | --- | --- |
//! | `NNTP_TEST_HOST` | *required* | Server host name. |
//! | `NNTP_TEST_PORT` | 563, or 119 for `plain`/`starttls` | Port. |
//! | `NNTP_TEST_SECURITY` | `tls` | `tls`, `starttls` or `plain`. |
//! | `NNTP_TEST_USER` | none | Username for `AUTHINFO`. |
//! | `NNTP_TEST_PASS` | none | Password. Never written to a log: it is redacted at the point of encoding. |
//! | `NNTP_TEST_GROUP` | `misc.test` | A group the server carries, for the article tests. |
//! | `NNTP_TEST_SAMPLE` | 50 | How many of the newest articles to examine. |
//!
//! # What a failure means
//!
//! A failure here is a *finding*, not a broken test. Each one should become a fixture in
//! `nntp-testserver` plus a regression test, and a row in `docs/protocol-coverage.md`.
//! That is the whole point: turn one person's anecdote about one server into something the
//! offline suite checks forever.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::{Duration, Instant};

use nntp_client::{Client, ConnectOptions, Security, Transport, connector};
use nntp_proto::response::codes;
use nntp_proto::{ArticleSpec, Command, GroupName, MessageId, OverviewRecord, Range};

/// Connection details read from the environment.
struct Settings {
    host: String,
    port: u16,
    security: Security,
    username: Option<String>,
    password: Option<String>,
    group: String,
    sample: u64,
}

impl Settings {
    /// Reads the settings, explaining what is missing rather than panicking obscurely.
    fn from_env() -> Self {
        let host = std::env::var("NNTP_TEST_HOST").unwrap_or_else(|_| {
            panic!(
                "NNTP_TEST_HOST is not set.\n\
                 These tests talk to a real news server and are skipped unless you point \
                 them at one. See the module documentation at the top of \
                 crates/nntp-client/tests/real_server.rs for the full list of variables."
            )
        });

        let security = match std::env::var("NNTP_TEST_SECURITY")
            .unwrap_or_else(|_| "tls".to_owned())
            .to_ascii_lowercase()
            .as_str()
        {
            "tls" | "implicit-tls" => Security::ImplicitTls,
            "starttls" => Security::StartTls,
            "plain" | "none" => Security::Plain,
            other => panic!("NNTP_TEST_SECURITY={other:?}; expected tls, starttls or plain"),
        };

        let port = std::env::var("NNTP_TEST_PORT")
            .ok()
            .map(|raw| raw.parse().expect("NNTP_TEST_PORT must be a number"))
            .unwrap_or_else(|| security.default_port());

        Self {
            host,
            port,
            security,
            username: std::env::var("NNTP_TEST_USER").ok(),
            password: std::env::var("NNTP_TEST_PASS").ok(),
            group: std::env::var("NNTP_TEST_GROUP").unwrap_or_else(|_| "misc.test".to_owned()),
            sample: std::env::var("NNTP_TEST_SAMPLE")
                .ok()
                .and_then(|raw| raw.parse().ok())
                .unwrap_or(50),
        }
    }

    fn options(&self) -> ConnectOptions {
        ConnectOptions::new(&self.host)
            .security(self.security)
            .port(self.port)
            // Generous: a public server under load can take a while over `LIST`.
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(120))
            .write_timeout(Duration::from_secs(30))
    }
}

/// Connects, negotiates and authenticates, reporting each step.
fn connect() -> (Settings, Client<Transport>) {
    let settings = Settings::from_env();

    eprintln!(
        "\n=== {}:{} ({:?}) ===",
        settings.host, settings.port, settings.security
    );

    let started = Instant::now();
    let mut client = connector::connect(&settings.options()).unwrap_or_else(|error| {
        panic!("connecting to {}:{}: {error}", settings.host, settings.port)
    });
    eprintln!("connected in {:?}", started.elapsed());
    eprintln!(
        "greeting: {} {}",
        client.greeting().code,
        client.greeting().text
    );
    eprintln!("encrypted: {}", client.is_encrypted());

    client.handshake().expect("handshake");

    if client.capabilities().is_empty() {
        eprintln!("capabilities: none advertised (pre-RFC-3977 server)");
    } else {
        eprintln!(
            "implementation: {}",
            client
                .capabilities()
                .implementation()
                .unwrap_or_else(|| "(not reported)".to_owned())
        );
        eprintln!(
            "capabilities: {}",
            client
                .capabilities()
                .entries()
                .iter()
                .map(|entry| entry.label.clone())
                .collect::<Vec<_>>()
                .join(" ")
        );
    }

    if let Some(username) = &settings.username {
        // The link is encrypted for tls/starttls, so no plaintext opt-in is needed there.
        let allow_plaintext = !client.is_encrypted();
        if allow_plaintext {
            eprintln!(
                "WARNING: authenticating over an unencrypted connection because \
                 NNTP_TEST_SECURITY asked for it"
            );
        }
        client
            .authenticate(username, settings.password.as_deref(), allow_plaintext)
            .expect("authenticate");
        eprintln!("authenticated as {username}");
    } else {
        eprintln!("no NNTP_TEST_USER set; continuing unauthenticated");
    }

    (settings, client)
}

fn group_name(settings: &Settings) -> GroupName {
    GroupName::parse(&settings.group)
        .unwrap_or_else(|error| panic!("NNTP_TEST_GROUP={:?}: {error}", settings.group))
}

/// Selects the configured group, failing with something actionable if the server does not
/// carry it.
///
/// The first run of these tests used `comp.lang.rust`, which Eternal September does not
/// carry, and four tests failed with `NoSuchGroup`. That is a fixture problem rather than a
/// finding, and the failure should say so and name groups that *do* exist rather than
/// leaving the reader to guess.
fn select_group(client: &mut Client<Transport>, settings: &Settings) -> nntp_proto::GroupSummary {
    let group = group_name(settings);

    match client.select_group(&group) {
        Ok(summary) => {
            eprintln!(
                "{}: ~{} articles, numbers {}..{}",
                summary.name, summary.estimated_count, summary.low, summary.high
            );
            summary
        }
        Err(nntp_client::ClientError::NoSuchGroup { .. }) => {
            let suggestions = suggest_groups(client)
                .iter()
                .map(|line| format!("  {line}"))
                .collect::<Vec<_>>()
                .join("\n");
            panic!(
                "this server does not carry {:?}.\n\
                 Set NNTP_TEST_GROUP to a group it does carry. Some with articles:\n{suggestions}\n\
                 `nntp-tui groups` lists them all.",
                settings.group
            );
        }
        Err(error) => panic!("GROUP {}: {error}", settings.group),
    }
}

/// A few groups the server carries that have enough articles to test with.
fn suggest_groups(client: &mut Client<Transport>) -> Vec<String> {
    // Narrow wildmats rather than the whole active file: this runs inside a failure path,
    // and a multi-megabyte LIST would make a bad error message slow as well.
    for pattern in ["comp.lang.*", "misc.*", "news.*", "*"] {
        let Ok(wildmat) = nntp_proto::Wildmat::parse(pattern) else {
            continue;
        };
        let Ok(result) = client.list_groups(Some(&wildmat)) else {
            continue;
        };

        let found: Vec<String> = result
            .entries
            .iter()
            .filter(|entry| entry.estimated_count() >= 20)
            .take(8)
            .map(|entry| format!("{} ({} articles)", entry.name, entry.estimated_count()))
            .collect();

        if !found.is_empty() {
            return found;
        }
    }

    vec!["(could not list any; the LIST command failed too)".to_owned()]
}

#[test]
#[ignore = "needs a real news server; see the module documentation"]
fn the_server_clock_parses_and_is_close_to_ours() {
    let (_, mut client) = connect();

    let date = client.server_date().expect("DATE");
    let skew = chrono::Utc::now().signed_duration_since(date);
    eprintln!(
        "server date: {} (skew {}s)",
        date.to_rfc3339(),
        skew.num_seconds()
    );

    // NEWGROUPS and NEWNEWS are relative to the server's clock, so a large skew is a
    // finding worth knowing about even though it is not our bug.
    assert!(
        skew.num_seconds().abs() < 600,
        "the server's clock differs from ours by {}s, which would make any NEWGROUPS or \
         NEWNEWS query wrong",
        skew.num_seconds()
    );

    let _ = client.quit();
}

#[test]
#[ignore = "needs a real news server; see the module documentation"]
fn every_line_of_the_group_list_parses() {
    // The headline test. A real full-feed LIST ACTIVE is over a hundred thousand lines
    // written by decades of different software; if our parser disagrees with reality, this
    // is where it shows.
    let (_, mut client) = connect();

    let started = Instant::now();
    let result = client.list_groups(None).expect("LIST ACTIVE");
    eprintln!(
        "LIST ACTIVE: {} groups in {:?}, {} unparseable line(s)",
        result.len(),
        started.elapsed(),
        result.skipped.len()
    );

    for line in result.skipped.iter().take(20) {
        eprintln!("  could not parse: {line:?}");
    }

    assert!(!result.entries.is_empty(), "the server carries no groups?");
    assert!(
        result.skipped.is_empty(),
        "{} of {} LIST ACTIVE lines did not parse. Each one is a finding: add it to the \
         nntp-testserver corpus with a regression test.",
        result.skipped.len(),
        result.len() + result.skipped.len()
    );

    let _ = client.quit();
}

#[test]
#[ignore = "needs a real news server; see the module documentation"]
fn every_line_of_the_group_descriptions_parses() {
    let (_, mut client) = connect();

    let result = match client.list_group_descriptions(None) {
        Ok(result) => result,
        Err(error) if !error.is_connection_fatal() => {
            // Plenty of servers do not offer this, which is not a failure.
            eprintln!("LIST NEWSGROUPS unavailable: {error}");
            return;
        }
        Err(error) => panic!("LIST NEWSGROUPS: {error}"),
    };

    eprintln!(
        "LIST NEWSGROUPS: {} descriptions, {} unparseable line(s)",
        result.len(),
        result.skipped.len()
    );
    for line in result.skipped.iter().take(20) {
        eprintln!("  could not parse: {line:?}");
    }

    // Descriptions are frequently 8-bit and unlabelled, which is the interesting part.
    let non_ascii = result
        .entries
        .iter()
        .filter(|entry| !entry.description.is_ascii())
        .count();
    eprintln!("  {non_ascii} description(s) contain non-ASCII text");

    assert!(
        result.skipped.is_empty(),
        "{} LIST NEWSGROUPS lines did not parse",
        result.skipped.len()
    );

    let _ = client.quit();
}

#[test]
#[ignore = "needs a real news server; see the module documentation"]
fn the_overview_format_starts_with_the_seven_required_fields() {
    let (_, mut client) = connect();

    let fmt = client.overview_format().expect("LIST OVERVIEW.FMT");
    eprintln!(
        "OVERVIEW.FMT: {}",
        fmt.fields()
            .iter()
            .map(|field| {
                let suffix = if field.full { ":full" } else { "" };
                format!("{}{suffix}", field.name)
            })
            .collect::<Vec<_>>()
            .join(" ")
    );

    // RFC 3977 §8.3 fixes the first seven. A server that disagrees is why fields are
    // mapped by name rather than by position — this asserts our assumption, and a failure
    // tells us the name-based mapping is doing real work.
    assert!(
        fmt.is_standard_prefix(),
        "this server's OVERVIEW.FMT does not begin with the seven RFC 3977 fields. That is \
         allowed for and handled, but it is worth a fixture."
    );

    let _ = client.quit();
}

#[test]
#[ignore = "needs a real news server; see the module documentation"]
fn overview_records_for_real_articles_parse_completely() {
    let (settings, mut client) = connect();
    let summary = select_group(&mut client, &settings);

    let Some((low, high)) = summary.range() else {
        panic!(
            "{} is empty; set NNTP_TEST_GROUP to a group with articles",
            settings.group
        );
    };

    let first = high
        .saturating_sub(settings.sample.saturating_sub(1))
        .max(low);
    let range = Range::between(first, high);

    let result = client.overview(range).expect("OVER");
    eprintln!(
        "OVER {}: {} record(s), {} unparseable line(s)",
        range.to_argument(),
        result.len(),
        result.skipped.len()
    );
    for line in result.skipped.iter().take(10) {
        eprintln!("  could not parse: {line:?}");
    }

    // Statistics, because these are the numbers that reveal a disagreement.
    let mut no_date = 0usize;
    let mut no_message_id = 0usize;
    let mut encoded_subjects = 0usize;
    let mut replies = 0usize;

    for record in &result.entries {
        if record.date.is_none() {
            no_date += 1;
            eprintln!(
                "  unparseable date on {}: {:?}",
                record.number, record.date_raw
            );
        }
        if record.message_id.is_none() {
            no_message_id += 1;
        }
        if record.subject.contains("=?") {
            // A subject that still contains an encoded word means the decoder failed.
            encoded_subjects += 1;
            eprintln!(
                "  undecoded subject on {}: {:?}",
                record.number, record.subject
            );
        }
        if record.is_reply() {
            replies += 1;
        }
    }

    eprintln!(
        "  {replies} reply/replies, {no_date} unparseable date(s), \
         {no_message_id} missing message-id(s), {encoded_subjects} undecoded subject(s)"
    );

    assert!(
        !result.entries.is_empty(),
        "OVER returned nothing for a non-empty group"
    );
    assert!(
        result.skipped.is_empty(),
        "{} OVER lines did not parse",
        result.skipped.len()
    );
    assert_eq!(
        encoded_subjects, 0,
        "{encoded_subjects} subject(s) still contain an RFC 2047 encoded word after \
         decoding, which means the decoder rejected a form this server sends"
    );
    assert_eq!(
        no_message_id, 0,
        "{no_message_id} record(s) had no usable message-id, which breaks threading and \
         fetching by id"
    );
    // A malformed Date is the article author's fault, not ours, so it is reported rather
    // than asserted — but a *lot* of them means our parser is too strict.
    assert!(
        no_date * 4 <= result.len(),
        "{no_date} of {} dates did not parse; the parser is probably rejecting a form this \
         server's users produce",
        result.len()
    );

    let _ = client.quit();
}

#[test]
#[ignore = "needs a real news server; see the module documentation"]
fn xover_agrees_with_over() {
    // The fallback path has to produce the same answers as the primary one, or a server
    // that lacks OVER would quietly show different data. Nothing but a real server can
    // check that our two code paths agree on real input.
    let (settings, mut client) = connect();

    if !client.capabilities().has_over() {
        eprintln!("this server does not advertise OVER; nothing to compare");
        return;
    }

    let group = group_name(&settings);
    let summary = select_group(&mut client, &settings);
    let Some((low, high)) = summary.range() else {
        panic!("{} is empty", settings.group);
    };

    let first = high
        .saturating_sub(settings.sample.min(20).saturating_sub(1))
        .max(low);
    let range = Range::between(first, high);

    let over = client.overview(range).expect("OVER");

    // Force the XOVER path on a second connection, since the client remembers which
    // command worked and will not go back.
    let mut second = connector::connect(&settings.options()).expect("second connection");
    second.handshake().expect("handshake");
    if let Some(username) = &settings.username {
        let allow_plaintext = !second.is_encrypted();
        second
            .authenticate(username, settings.password.as_deref(), allow_plaintext)
            .expect("authenticate");
    }
    second.select_group(&group).expect("GROUP");

    let fmt = second.overview_format().expect("OVERVIEW.FMT");

    // Driven through the framing layer rather than `Client::overview`, because the client
    // deliberately remembers which command worked and will not go back to the other one.
    // Using the raw command needs no new public API and is what a diagnostic should do.
    let status = second
        .connection_mut()
        .command(&Command::XOver(range))
        .expect("send XOVER");
    if status.code != codes::OVERVIEW_FOLLOWS {
        eprintln!(
            "XOVER unavailable on this server: {} {}",
            status.code, status.text
        );
        return;
    }
    let block = second.connection_mut().read_block().expect("XOVER block");
    let xover_result = OverviewRecord::parse_block(&block, &fmt);
    let xover = xover_result.entries;
    let skipped = xover_result.skipped.len();

    eprintln!(
        "OVER gave {} record(s), XOVER gave {} ({} unparseable)",
        over.len(),
        xover.len(),
        skipped
    );

    assert_eq!(
        skipped, 0,
        "{skipped} XOVER line(s) did not parse, though the OVER equivalents did"
    );
    assert_eq!(
        over.entries.len(),
        xover.len(),
        "OVER and XOVER returned different numbers of records for the same range"
    );
    for (from_over, from_xover) in over.entries.iter().zip(xover.iter()) {
        assert_eq!(
            (from_over.number, &from_over.subject, &from_over.message_id),
            (
                from_xover.number,
                &from_xover.subject,
                &from_xover.message_id
            ),
            "OVER and XOVER disagree about article {}",
            from_over.number
        );
    }

    let _ = client.quit();
    let _ = second.quit();
}

#[test]
#[ignore = "needs a real news server; see the module documentation"]
fn real_articles_parse_and_head_agrees_with_article() {
    let (settings, mut client) = connect();
    let summary = select_group(&mut client, &settings);
    let Some((low, high)) = summary.range() else {
        panic!("{} is empty", settings.group);
    };

    // A handful of the newest, and stop at the first that exists — expiry leaves gaps.
    let mut examined = 0usize;
    let mut malformed_headers = 0usize;

    for number in (low..=high).rev().take(settings.sample as usize) {
        let article = match client.article(ArticleSpec::Number(number)) {
            Ok(article) => article,
            // A gap in the numbering is normal, not a failure.
            Err(error) if !error.is_connection_fatal() => continue,
            Err(error) => panic!("ARTICLE {number}: {error}"),
        };

        examined += 1;
        let malformed = article.headers.malformed().len();
        malformed_headers += malformed;

        if malformed > 0 {
            eprintln!("  article {number}: {malformed} unparseable header line(s)");
            for line in article.headers.malformed().iter().take(3) {
                eprintln!("    {:?}", String::from_utf8_lossy(line));
            }
        }

        // Anything we claim to decode, we should decode.
        let subject = article.subject();
        assert!(
            !subject.contains("=?"),
            "article {number}: subject still contains an encoded word: {subject:?}"
        );

        if let Some(id) = article.message_id() {
            assert!(
                MessageId::parse(id.as_str()).is_ok(),
                "article {number}: round-tripping its own message-id failed"
            );
        }

        // HEAD must return the same headers as ARTICLE. If it does not, one of the two
        // response parsers is wrong, and overview-driven reading would disagree with
        // article-driven reading.
        if examined <= 5 {
            let head = client.head(ArticleSpec::Number(number)).expect("HEAD");
            assert_eq!(
                head.subject(),
                article.subject(),
                "article {number}: HEAD and ARTICLE disagree about the subject"
            );
            assert_eq!(
                head.message_id(),
                article.message_id(),
                "article {number}: HEAD and ARTICLE disagree about the message-id"
            );
            assert!(!head.has_body(), "HEAD returned a body");
        }

        if examined >= 10 {
            break;
        }
    }

    eprintln!("examined {examined} article(s), {malformed_headers} unparseable header line(s)");
    assert!(
        examined > 0,
        "no articles could be fetched from {}",
        settings.group
    );
    assert_eq!(
        malformed_headers, 0,
        "{malformed_headers} header line(s) across {examined} real articles did not parse. \
         Each is a finding: add it to the nntp-testserver corpus."
    );

    let _ = client.quit();
}

#[test]
#[ignore = "needs a real news server; see the module documentation"]
fn fetching_by_message_id_works_without_a_selected_group() {
    let (settings, mut client) = connect();
    let summary = select_group(&mut client, &settings);
    let Some((low, high)) = summary.range() else {
        panic!("{} is empty", settings.group);
    };

    let first = high.saturating_sub(9).max(low);
    let records = client.overview(Range::between(first, high)).expect("OVER");
    let Some(id) = records
        .entries
        .iter()
        .rev()
        .find_map(|record| record.message_id.clone())
    else {
        panic!("no usable message-id among the newest articles");
    };

    eprintln!("fetching {id} by message-id on a fresh connection");

    // A fresh connection, so nothing is selected: this is the path the reader uses to
    // follow a References chain into another group.
    let mut fresh = connector::connect(&settings.options()).expect("connect");
    fresh.handshake().expect("handshake");
    if let Some(username) = &settings.username {
        let allow_plaintext = !fresh.is_encrypted();
        fresh
            .authenticate(username, settings.password.as_deref(), allow_plaintext)
            .expect("authenticate");
    }

    let article = fresh
        .article(ArticleSpec::MessageId(id.clone()))
        .expect("ARTICLE <message-id>");
    assert_eq!(
        article.message_id().as_ref(),
        Some(&id),
        "the server returned a different article than the one asked for"
    );
    eprintln!("  subject: {:?}", article.subject());

    let _ = client.quit();
    let _ = fresh.quit();
}
