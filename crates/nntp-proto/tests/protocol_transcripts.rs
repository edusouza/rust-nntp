//! Tests that drive `nntp-proto` through its public API using captured server output.
//!
//! The unit tests inside the crate can reach private helpers; these cannot, so they also
//! serve as a check that the public API is sufficient to do real work. The fixtures are
//! shaped like output from INN, which is what most of Usenet runs.

// Test code is allowed the conveniences the library denies itself.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use nntp_proto::block::DataBlock;
use nntp_proto::{
    Article, Capabilities, GroupSummary, MessageId, NewsgroupEntry, OverviewFmt, OverviewRecord,
    StatusLine, TransferEncoding,
};

fn fixture(name: &str) -> DataBlock {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("reading {path}: {e}"));
    DataBlock::parse(&bytes)
}

#[test]
fn reads_a_capability_list_from_inn() {
    let caps = Capabilities::parse(&fixture("capabilities-inn.txt"));

    assert_eq!(caps.versions(), [2]);
    assert!(caps.implementation().unwrap().starts_with("INN 2.7.1"));
    assert!(caps.has_reader());
    assert!(caps.has_over());
    assert!(caps.over_accepts_message_id());
    assert!(caps.has_hdr());
    assert!(caps.has_authinfo_user());
    assert!(caps.has_list_keyword("OVERVIEW.FMT"));

    // This server does not advertise POST, NEWNEWS, STARTTLS or compression, and a client
    // must not attempt them on the strength of the others.
    assert!(!caps.has_post());
    assert!(!caps.has_newnews());
    assert!(!caps.has_starttls());
    assert!(!caps.has_compress_deflate());

    // READER is present, so MODE READER is unnecessary even though MODE-READER is listed.
    assert!(!caps.needs_mode_reader());
}

#[test]
fn reads_a_newsgroups_listing_with_eight_bit_descriptions() {
    let result = NewsgroupEntry::parse_block(&fixture("list-newsgroups.txt"));

    assert_eq!(result.len(), 4);
    assert!(result.skipped.is_empty(), "skipped: {:?}", result.skipped);
    assert_eq!(result.entries[0].name.as_str(), "comp.lang.rust");
    assert_eq!(
        result.entries[0].description,
        "Discussion of the Rust programming language"
    );
    assert_eq!(
        result.entries[2].name.as_str(),
        "de.comp.os.unix.linux.misc"
    );
}

#[test]
fn reads_an_overview_response_using_the_advertised_format() {
    let fmt = OverviewFmt::parse(&fixture("overview-fmt-inn.txt"));
    assert!(fmt.is_standard_prefix());
    assert_eq!(fmt.fields().len(), 8);

    // One overview line as INN sends it, with the Xref field carrying its own name.
    let line = b"4242\t=?UTF-8?Q?caf=C3=A9?=\tbjorn@example.no\tWed, 17 Sep 2026 08:09:10 +0200\t<child@example.no>\t<root@example.com> <middle@example.net>\t1830\t6\tXref: news.example.org comp.lang.rust:4242";
    let record = OverviewRecord::parse(line, &fmt).unwrap();

    assert_eq!(record.number, 4242);
    assert_eq!(record.subject, "café");
    assert_eq!(record.bytes, Some(1830));
    assert_eq!(record.lines, Some(6));
    assert_eq!(
        record.date.unwrap().to_rfc3339(),
        "2026-09-17T08:09:10+02:00"
    );
    assert_eq!(
        record.extra("Xref"),
        Some("news.example.org comp.lang.rust:4242")
    );

    // Threading: the parent is the last reference.
    assert!(record.is_reply());
    assert_eq!(
        record.parent().map(MessageId::as_str),
        Some("<middle@example.net>")
    );
}

#[test]
fn reads_a_real_shaped_article_end_to_end() {
    let article = Article::from_block(&fixture("article.txt")).with_number(4242);

    assert_eq!(article.number, Some(4242));

    // Subject is base64 encoded, author is Q-encoded Latin-1.
    assert_eq!(article.subject(), "Re: café and crates");
    assert_eq!(article.author(), "Bjørn Nordmæl <bjorn@example.no>");

    assert_eq!(
        article.date().unwrap().to_rfc3339(),
        "2026-09-17T08:09:10+02:00"
    );
    assert_eq!(
        article.message_id().map(|id| id.as_str().to_owned()),
        Some("<child@example.no>".to_owned())
    );

    // The References header is folded across two lines.
    let references: Vec<String> = article
        .references()
        .iter()
        .map(|id| id.as_str().to_owned())
        .collect();
    assert_eq!(references, ["<root@example.com>", "<middle@example.net>"]);

    assert_eq!(
        article
            .newsgroups()
            .iter()
            .map(|g| g.as_str().to_owned())
            .collect::<Vec<_>>(),
        ["comp.lang.rust"]
    );

    assert_eq!(
        article.transfer_encoding(),
        TransferEncoding::QuotedPrintable
    );
    assert_eq!(article.content_type().charset(), Some("UTF-8"));

    let body = article.body_text();
    // Quoted-printable escapes decoded and the soft line break joined.
    assert!(
        body.contains("On the subject of café, I have this to say."),
        "body was: {body:?}"
    );
    // A body line that begins with a dot survives unstuffing.
    assert!(body.contains("\n.signature-like line that starts with a dot"));
    // The signature separator is "-- " with the trailing space encoded as =20.
    assert!(body.contains("\n-- \n"));
    assert!(body.ends_with("Bjørn"));
}

#[test]
fn a_group_response_and_an_overview_range_agree() {
    let status = StatusLine::parse(b"211 6 4237 4242 comp.lang.rust").unwrap();
    let summary = GroupSummary::parse(&status, None).unwrap();

    let (low, high) = summary.range().unwrap();
    assert_eq!((low, high), (4237, 4242));

    // The estimate and the watermarks need not agree, and the client must not assume
    // that every number in the range exists.
    let block = DataBlock::from_lines(vec![
        b"4237\tfirst\ta@x\t\t<1@x>\t\t10\t1".to_vec(),
        b"4242\tlast\tb@x\t\t<2@x>\t\t20\t2".to_vec(),
    ]);
    let result = OverviewRecord::parse_block(&block, &OverviewFmt::standard());
    assert_eq!(result.len(), 2);
    assert!(
        result
            .entries
            .iter()
            .all(|r| r.number >= low && r.number <= high)
    );
    assert!(result.len() < summary.estimated_count as usize + 1);
}
