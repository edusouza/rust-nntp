//! End-to-end tests: the real client, over a real TCP socket, against the fake server.
//!
//! The unit tests in `nntp-client` drive the conversation over in-memory streams, which
//! proves the logic but not the connector, the socket options or the framing across
//! segment boundaries. These tests cover that, and they cover the server behaviours that
//! matter most: the ones that are wrong.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Duration;

use nntp_client::{ClientError, ConnectOptions, Limits, connector};
use nntp_proto::{ArticleSpec, GroupName, MessageId, Range};
use nntp_testserver::{CapabilityProfile, Corpus, GreetingMode, Quirks, ServerConfig, TestServer};

/// Connects to a server, with timeouts short enough that a hang fails the test instead of
/// stalling the suite.
fn connect(server: &TestServer) -> nntp_client::Client<nntp_client::Transport> {
    connect_with(server, Limits::default())
}

fn connect_with(
    server: &TestServer,
    limits: Limits,
) -> nntp_client::Client<nntp_client::Transport> {
    let options = ConnectOptions::new("127.0.0.1")
        .port(server.port())
        .connect_timeout(Duration::from_secs(5))
        .read_timeout(Duration::from_secs(5))
        .write_timeout(Duration::from_secs(5))
        .limits(limits);
    connector::connect(&options).expect("connect")
}

fn serve(config: ServerConfig) -> TestServer {
    TestServer::with(Corpus::sample(), config).expect("start server")
}

fn group(name: &str) -> GroupName {
    GroupName::parse(name).unwrap()
}

#[test]
fn a_whole_reading_session() {
    let server = serve(ServerConfig::new());
    let mut client = connect(&server);

    client.handshake().expect("handshake");
    assert!(client.capabilities().has_reader());
    assert!(client.capabilities().has_over());
    assert!(client.greeting().posting_allowed);

    // The group list, including the empty group and the moderated one.
    let groups = client.list_groups(None).expect("list");
    assert!(groups.skipped.is_empty(), "skipped: {:?}", groups.skipped);
    let names: Vec<&str> = groups.entries.iter().map(|g| g.name.as_str()).collect();
    assert!(names.contains(&"misc.test"));
    assert!(names.contains(&"empty.group"));

    let moderated = groups
        .entries
        .iter()
        .find(|g| g.name.as_str() == "de.comp.test")
        .unwrap();
    assert!(moderated.status.allows_posting());

    // Descriptions, including a non-ASCII one.
    let descriptions = client.list_group_descriptions(None).expect("descriptions");
    let german = descriptions
        .entries
        .iter()
        .find(|g| g.name.as_str() == "de.comp.test")
        .unwrap();
    assert!(german.description.contains("äöü"), "{}", german.description);

    // Select a group and read its overview.
    let summary = client.select_group(&group("misc.test")).expect("group");
    assert_eq!(summary.range(), Some((1, 3)));

    let overview = client.overview(Range::between(1, 3)).expect("overview");
    assert_eq!(overview.len(), 3);
    assert!(overview.skipped.is_empty());

    let reply = &overview.entries[1];
    assert_eq!(reply.subject, "Re: A plain test article");
    assert!(reply.is_reply());
    assert_eq!(
        reply.parent().map(MessageId::as_str),
        Some("<root@test.invalid>")
    );
    assert!(reply.bytes.is_some());
    // The Xref field is named because the server advertises it as Xref:full.
    assert!(reply.extra("Xref").unwrap().contains("misc.test:2"));

    // An article whose date cannot be parsed still arrives, with the raw text kept.
    let undated = &overview.entries[2];
    assert!(undated.date.is_none());
    assert_eq!(undated.date_raw, "yesterday afternoon");

    // Fetch the article whose body contains a line starting with a dot.
    let article = client.article(ArticleSpec::Number(2)).expect("article");
    assert_eq!(article.number, Some(2));
    let body = article.body_text();
    assert!(
        body.contains("\n.signature-like line that starts with a dot"),
        "dot-stuffing was mishandled: {body:?}"
    );
    // And the signature separator, whose trailing space is significant.
    assert!(body.contains("\n-- \n"), "body: {body:?}");

    let date = client.server_date().expect("date");
    assert_eq!(date.to_rfc3339(), "2026-09-17T08:09:10+00:00");

    client.quit().expect("quit");
}

#[test]
fn decodes_encoded_headers_and_a_quoted_printable_body_over_the_wire() {
    let server = serve(ServerConfig::new());
    let mut client = connect(&server);
    client.handshake().unwrap();
    client.select_group(&group("comp.lang.rust")).unwrap();

    let overview = client.overview(Range::between(4237, 4242)).unwrap();
    // A sparse group: two articles across a range of six numbers.
    assert_eq!(overview.len(), 2);

    assert_eq!(overview.entries[0].subject, "café and crates");
    assert_eq!(overview.entries[0].from, "Bjørn Nordmæl <bjorn@example.no>");
    assert_eq!(overview.entries[1].subject, "Re: café and crates");
    assert_eq!(overview.entries[1].from, "Åsa Lindqvist <asa@example.se>");

    let article = client.article(ArticleSpec::Number(4242)).unwrap();
    // An unlabelled Latin-1 header value.
    assert_eq!(
        article.headers.get_decoded("Organization").as_deref(),
        Some("Café Central")
    );
    // A quoted-printable body with a soft line break.
    let body = article.body_text();
    assert!(body.contains("café."), "body: {body:?}");
    assert!(
        body.contains("break follows here and this continues"),
        "soft line break not joined: {body:?}"
    );
}

#[test]
fn fetches_an_article_by_message_id_without_selecting_a_group() {
    let server = serve(ServerConfig::new());
    let mut client = connect(&server);
    client.handshake().unwrap();

    let id = MessageId::parse("<root@test.invalid>").unwrap();
    let article = client.article(ArticleSpec::MessageId(id.clone())).unwrap();
    assert_eq!(article.subject(), "A plain test article");
    // The server reported number 0, meaning "not applicable".
    assert_eq!(article.number, None);

    let found = client.stat(ArticleSpec::MessageId(id)).unwrap();
    assert_eq!(found.message_id.unwrap().as_str(), "<root@test.invalid>");
}

#[test]
fn works_against_a_server_that_predates_capabilities() {
    let server = serve(ServerConfig::new().capabilities(CapabilityProfile::Legacy));
    let mut client = connect(&server);

    // No CAPABILITIES and no MODE READER: both answer 500, and neither is fatal.
    client.handshake().expect("handshake");
    assert!(client.capabilities().is_empty());

    client.select_group(&group("misc.test")).unwrap();
    // With no capability list to go on, the client tries OVER anyway. This server happens
    // to implement it despite having no CAPABILITIES, which is a real combination; the
    // case where the attempt is refused is covered by
    // `falls_back_when_a_server_advertises_over_but_refuses_it`.
    let overview = client.overview(Range::between(1, 3)).expect("overview");
    assert_eq!(overview.len(), 3);
}

#[test]
fn enters_reader_mode_on_a_transit_server() {
    let server = serve(ServerConfig::new().capabilities(CapabilityProfile::Transit));
    let mut client = connect(&server);

    client.handshake().expect("handshake");
    // After MODE READER the server advertises READER and OVER.
    assert!(client.capabilities().has_reader());
    assert!(client.capabilities().has_over());
    assert!(!client.capabilities().needs_mode_reader());

    client.select_group(&group("misc.test")).unwrap();
    assert_eq!(client.overview(Range::From(1)).unwrap().len(), 3);
}

#[test]
fn uses_xover_when_the_server_does_not_advertise_over() {
    let server = serve(ServerConfig::new().capabilities(CapabilityProfile::NoOver));
    let mut client = connect(&server);
    client.handshake().unwrap();
    client.select_group(&group("misc.test")).unwrap();

    // The capability list omits OVER, so the client goes straight to XOVER; had it sent
    // OVER, this server would have answered 500 and the test would still pass, so the
    // assertion that matters is that the data arrives either way.
    let overview = client.overview(Range::between(1, 3)).expect("overview");
    assert_eq!(overview.len(), 3);
}

#[test]
fn falls_back_when_a_server_advertises_over_but_refuses_it() {
    // This combination is the reason the fallback cannot be driven by capabilities alone.
    let server = serve(ServerConfig::new().quirks(Quirks {
        over_advertised_but_missing: true,
        ..Quirks::default()
    }));
    let mut client = connect(&server);
    client.handshake().unwrap();
    assert!(client.capabilities().has_over(), "the server claims OVER");

    client.select_group(&group("misc.test")).unwrap();
    let overview = client.overview(Range::between(1, 3)).expect("overview");
    assert_eq!(overview.len(), 3);

    // And the choice is remembered, so the second call does not retry OVER.
    assert_eq!(client.overview(Range::Single(1)).unwrap().len(), 1);
}

#[test]
fn assumes_the_standard_overview_layout_when_the_server_will_not_say() {
    let server = serve(ServerConfig::new().quirks(Quirks {
        no_overview_fmt: true,
        ..Quirks::default()
    }));
    let mut client = connect(&server);
    client.handshake().unwrap();
    client.select_group(&group("misc.test")).unwrap();

    let overview = client.overview(Range::between(1, 1)).expect("overview");
    assert_eq!(overview.entries[0].subject, "A plain test article");
    // The eighth field is still delivered, just under a positional name.
    assert!(overview.entries[0].extra("field-8").is_some());
}

#[test]
fn reports_a_server_that_refuses_an_open_ended_range() {
    let server = serve(ServerConfig::new().quirks(Quirks {
        reject_open_ended_ranges: true,
        ..Quirks::default()
    }));
    let mut client = connect(&server);
    client.handshake().unwrap();
    client.select_group(&group("misc.test")).unwrap();

    // The open form is refused...
    let error = client.overview(Range::From(1)).unwrap_err();
    assert!(!error.is_connection_fatal(), "{error}");

    // ...but the explicit form, built from the group watermarks, works. This is why
    // GroupSummary::range exists.
    let summary = client.select_group(&group("misc.test")).unwrap();
    let (low, high) = summary.range().unwrap();
    assert_eq!(client.overview(Range::between(low, high)).unwrap().len(), 3);
}

#[test]
fn authenticates_when_the_server_demands_it() {
    let server = serve(ServerConfig::new().require_auth("bob", "hunter2"));
    let mut client = connect(&server);
    client.handshake().unwrap();
    assert!(client.capabilities().has_authinfo_user());

    // Reading is refused until authenticated, and the error says why.
    let error = client.select_group(&group("misc.test")).unwrap_err();
    assert!(error.needs_authentication(), "{error}");
    assert!(!error.is_connection_fatal());

    // The link is plaintext, so credentials are refused unless explicitly allowed.
    let refused = client
        .authenticate("bob", Some("hunter2"), false)
        .unwrap_err();
    assert!(matches!(
        refused,
        ClientError::PlaintextAuthenticationRefused
    ));

    client
        .authenticate("bob", Some("hunter2"), true)
        .expect("authenticate");
    assert!(client.is_authenticated());
    client.select_group(&group("misc.test")).expect("group");
}

#[test]
fn reports_rejected_credentials_without_killing_the_connection() {
    let server = serve(ServerConfig::new().require_auth("bob", "hunter2"));
    let mut client = connect(&server);
    client.handshake().unwrap();

    let error = client.authenticate("bob", Some("wrong"), true).unwrap_err();
    assert!(matches!(error, ClientError::AuthenticationRejected { .. }));
    assert!(!error.is_connection_fatal());
    assert!(!client.is_authenticated());

    // The connection is still usable, so a second attempt can be made.
    client
        .authenticate("bob", Some("hunter2"), true)
        .expect("second attempt");
}

#[test]
fn a_refusing_greeting_fails_the_connection() {
    let server = serve(ServerConfig::new().greeting(GreetingMode::Refuse {
        code: 502,
        text: "access denied".to_owned(),
    }));

    let options = ConnectOptions::new("127.0.0.1")
        .port(server.port())
        .connect_timeout(Duration::from_secs(5));
    let error = connector::connect(&options).unwrap_err();
    assert!(matches!(
        error,
        ClientError::Server { code, .. } if code.as_u16() == 502
    ));
}

#[test]
fn a_no_posting_greeting_is_reported() {
    let server = serve(ServerConfig::new().greeting(GreetingMode::NoPosting));
    let client = connect(&server);
    assert!(!client.greeting().posting_allowed);
}

#[test]
fn reports_a_block_the_server_truncated() {
    let server = serve(ServerConfig::new().quirks(Quirks {
        truncate_next_block: true,
        ..Quirks::default()
    }));
    let mut client = connect(&server);

    // The first block the server sends is cut short with no terminator.
    let error = client.list_groups(None).unwrap_err();
    assert!(
        matches!(error, ClientError::ConnectionClosed(_)),
        "expected a truncated-response error, got {error}"
    );
    assert!(error.is_connection_fatal());
}

#[test]
fn enforces_the_line_limit_against_a_server_that_sends_a_monster_line() {
    let server = serve(ServerConfig::new().quirks(Quirks {
        overlong_help_line: true,
        ..Quirks::default()
    }));
    let mut client = connect_with(&server, Limits::SMALL);

    let error = client.help().unwrap_err();
    assert!(
        matches!(error, ClientError::LineTooLong { .. }),
        "expected LineTooLong, got {error}"
    );
    // And the connection is poisoned, because the rest of the line is still on the wire.
    assert!(error.is_connection_fatal());
    assert!(client.connection_mut().is_desynchronised());
}

#[test]
fn reports_a_server_that_vanishes_mid_session() {
    let server = serve(ServerConfig::new().quirks(Quirks {
        // The handshake alone spends this budget, so the next command finds nothing.
        close_after_commands: Some(1),
        ..Quirks::default()
    }));
    let mut client = connect(&server);

    // The first command is answered; the server then disappears without a word.
    client.refresh_capabilities().expect("first command");
    let error = client.server_date().unwrap_err();
    assert!(error.is_connection_fatal(), "{error}");
    assert!(
        matches!(
            error,
            ClientError::ConnectionClosed(_) | ClientError::Io(_) | ClientError::Timeout
        ),
        "unexpected error: {error}"
    );
}

#[test]
fn copes_with_a_server_that_sends_bare_lf() {
    let server = serve(ServerConfig::new().quirks(Quirks {
        bare_lf: true,
        ..Quirks::default()
    }));
    let mut client = connect(&server);

    client.handshake().expect("handshake");
    client.select_group(&group("misc.test")).expect("group");
    assert_eq!(client.overview(Range::between(1, 3)).unwrap().len(), 3);
}

#[test]
fn reports_a_group_the_server_does_not_carry() {
    let server = serve(ServerConfig::new());
    let mut client = connect(&server);
    client.handshake().unwrap();

    let error = client.select_group(&group("no.such.group")).unwrap_err();
    match &error {
        // The server's 411 says only "No such newsgroup", as INN's does, so a correct
        // group name here proves the client substitutes the one it asked for rather than
        // reading a word out of the response.
        ClientError::NoSuchGroup { group, text } => {
            assert_eq!(group, "no.such.group");
            assert_eq!(text, "No such newsgroup");
        }
        other => panic!("expected NoSuchGroup, got {other}"),
    }
    assert!(!error.is_connection_fatal());

    // The connection survives, so the session can carry on.
    client.select_group(&group("misc.test")).expect("group");
}

#[test]
fn an_empty_group_yields_no_range_and_no_overview() {
    let server = serve(ServerConfig::new());
    let mut client = connect(&server);
    client.handshake().unwrap();

    let summary = client.select_group(&group("empty.group")).unwrap();
    assert!(summary.is_empty());
    assert_eq!(summary.range(), None);

    // Asking anyway is an error the caller can ignore, not a broken connection.
    let error = client.overview(Range::between(1, 10)).unwrap_err();
    assert!(!error.is_connection_fatal(), "{error}");
}

#[test]
fn streams_a_group_list_without_buffering_it() {
    let server = serve(ServerConfig::new());
    let mut client = connect(&server);
    client.handshake().unwrap();

    let mut seen = Vec::new();
    client
        .list_groups_streaming(None, |entry| {
            if let Ok(entry) = entry {
                seen.push(entry.name.as_str().to_owned());
            }
        })
        .expect("stream");

    assert_eq!(seen.len(), 4);
    assert_eq!(seen[0], "misc.test");
}

#[test]
fn fetches_head_and_body_independently() {
    let server = serve(ServerConfig::new());
    let mut client = connect(&server);
    client.handshake().unwrap();
    client.select_group(&group("misc.test")).unwrap();

    let head = client.head(ArticleSpec::Number(1)).unwrap();
    assert_eq!(head.subject(), "A plain test article");
    assert!(!head.has_body());

    let body = client.body(ArticleSpec::Number(1)).unwrap();
    assert!(body.to_text().starts_with("This is the first article."));
}

#[test]
fn a_connection_refused_is_an_error_not_a_hang() {
    // Nothing is listening on port 1 of the loopback interface.
    let options = ConnectOptions::new("127.0.0.1")
        .port(1)
        .connect_timeout(Duration::from_millis(500));
    let error = connector::connect(&options).unwrap_err();
    assert!(error.is_connection_fatal());
}

// -- posting ----------------------------------------------------------------------------

/// A draft that is complete enough to be accepted.
fn draft(body: &str) -> nntp_proto::Draft {
    nntp_proto::Draft::parse(&format!(
        "From: A Poster <poster@example.org>\n\
         Newsgroups: misc.test\n\
         Subject: A test posting\n\
         \n\
         {body}\n"
    ))
}

#[test]
fn posts_an_article_and_the_server_receives_what_was_sent() {
    let server = TestServer::start().unwrap();
    let mut client = connect(&server);

    let text = client.post(&draft("Hello from the test suite.")).unwrap();
    assert!(text.contains("received"), "{text}");

    let posted = server.posted();
    assert_eq!(posted.len(), 1);
    assert_eq!(
        posted[0].header("Subject").as_deref(),
        Some("A test posting")
    );
    assert_eq!(posted[0].header("Newsgroups").as_deref(), Some("misc.test"));
    assert_eq!(posted[0].body_text(), "Hello from the test suite.");
}

#[test]
fn a_body_line_beginning_with_a_dot_arrives_intact() {
    // The classic NNTP bug: without dot-stuffing the article is truncated at this line and
    // the sender cannot tell, because the server answers 240 either way.
    let server = TestServer::start().unwrap();
    let mut client = connect(&server);

    client
        .post(&draft(
            ".signature is not a terminator\nand this line follows it",
        ))
        .unwrap();

    let posted = server.posted();
    assert_eq!(
        posted[0].body_text(),
        ".signature is not a terminator\nand this line follows it"
    );
}

#[test]
fn a_refused_article_reports_the_servers_own_words_and_keeps_the_connection() {
    // 441 is usually the only explanation there will be, and the connection is still
    // usable: dropping it would cost a reconnection for a mistake about to be fixed.
    let server = TestServer::with(
        Corpus::sample(),
        ServerConfig::new().quirks(Quirks {
            refuse_post: Some("no colon-space in \"From\" header".to_owned()),
            ..Quirks::default()
        }),
    )
    .unwrap();
    let mut client = connect(&server);

    let error = client.post(&draft("Body.")).unwrap_err();

    match &error {
        ClientError::PostingRejected { code, text } => {
            assert_eq!(code.as_u16(), 441);
            assert!(text.contains("colon-space"), "{text}");
        }
        other => panic!("expected a rejection, got {other:?}"),
    }
    assert!(!error.is_connection_fatal(), "the connection is still fine");

    // And the proof that it is: the next command works.
    let group = client
        .select_group(&GroupName::parse("misc.test").unwrap())
        .unwrap();
    assert_eq!(group.name.as_str(), "misc.test");
}

#[test]
fn a_server_that_greeted_without_posting_is_not_offered_an_article() {
    // Refused here rather than after a round trip, and without a POST command: a server
    // that counts refused offers should not be given one for something already known.
    let server = TestServer::with(
        Corpus::sample(),
        ServerConfig::new().greeting(GreetingMode::NoPosting),
    )
    .unwrap();
    let mut client = connect(&server);

    let error = client.post(&draft("Body.")).unwrap_err();

    assert!(
        matches!(error, ClientError::PostingNotAllowed { .. }),
        "{error:?}"
    );
    assert!(server.posted().is_empty());
}

#[test]
fn an_incomplete_draft_never_reaches_the_server() {
    let server = TestServer::start().unwrap();
    let mut client = connect(&server);

    let incomplete = nntp_proto::Draft::parse("Subject: no From, no Newsgroups\n\nBody.\n");
    let error = client.post(&incomplete).unwrap_err();

    assert!(error.to_string().contains("From"), "{error}");
    assert!(server.posted().is_empty(), "nothing was offered");
}

#[test]
fn a_non_ascii_article_arrives_as_encoded_words_and_utf8() {
    let server = TestServer::start().unwrap();
    let mut client = connect(&server);

    let draft = nntp_proto::Draft::parse(
        "From: Åsa <asa@example.se>\n\
         Newsgroups: misc.test\n\
         Subject: Uma pergunta sobre ação\n\
         \n\
         Está tudo bem?\n",
    );
    client.post(&draft).unwrap();

    let posted = server.posted();
    let subject = posted[0].header("Subject").unwrap();
    assert!(subject.is_ascii(), "{subject}");
    assert_eq!(
        nntp_proto::mime::decode_header_value(subject.as_bytes()),
        "Uma pergunta sobre ação"
    );
    assert_eq!(
        posted[0].header("Content-Type").as_deref(),
        Some("text/plain; charset=utf-8"),
        "an 8-bit body without this is a guess at the other end"
    );
    assert_eq!(posted[0].body_text(), "Está tudo bem?");
}

#[test]
fn the_connection_is_still_usable_after_a_successful_posting() {
    // The gap the refusal test left: 441 keeps the connection, but nothing checked that
    // 240 does. Reported from a real session — the article was accepted and the very next
    // command died with "connection aborted by the software in your host machine".
    let server = TestServer::start().unwrap();
    let mut client = connect(&server);

    client.post(&draft("Body.")).unwrap();

    let group = client
        .select_group(&GroupName::parse("misc.test").unwrap())
        .expect("the connection should still work after posting");
    assert_eq!(group.name.as_str(), "misc.test");

    let records = client.overview(Range::between(1, 3)).expect("overview");
    assert!(!records.entries.is_empty());
}

#[test]
fn an_idle_connection_survives_a_pause_and_is_told_when_it_does_not() {
    // Reported from a real session against the standalone server: an article was posted,
    // the reader sat while its user read the screen, and the next command died with
    // "connection aborted by the software in your host machine". The fake server's socket
    // timeout is thirty seconds, which is fine for a test and far too short for a person.
    //
    // Both halves are asserted here: a pause well inside the limit changes nothing, and a
    // server that does give up says so first, rather than leaving the client to report
    // whatever its platform calls a dead socket.
    let server = TestServer::with(
        Corpus::sample(),
        ServerConfig::new().idle_timeout(Duration::from_millis(400)),
    )
    .unwrap();
    let mut client = connect(&server);

    std::thread::sleep(Duration::from_millis(100));
    client
        .select_group(&GroupName::parse("misc.test").unwrap())
        .expect("a short pause is not a disconnection");

    // Now outstay it. The server announces the close instead of vanishing.
    std::thread::sleep(Duration::from_millis(700));
    let error = client
        .select_group(&GroupName::parse("misc.test").unwrap())
        .unwrap_err();

    match &error {
        ClientError::Server { code, text, .. } => {
            assert_eq!(code.as_u16(), 400);
            assert!(text.contains("idle"), "{text}");
        }
        // A connection the operating system has already torn down is the other honest
        // outcome; what must not happen is a wrong answer.
        ClientError::ConnectionClosed(_) | ClientError::Io(_) => {}
        other => panic!("unexpected error: {other:?}"),
    }
}
