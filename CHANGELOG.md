# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `nntp-proto`, the IO-free protocol layer:
  - status-line parsing with response-code classification, and named codes (RFC 3977 §3.2);
  - typed commands with validated encoding — arguments containing control characters are
    refused rather than written to the socket, and the 512-octet line limit is enforced
    (RFC 3977 §3.1);
  - multi-line data blocks with dot-stuffing in both directions (RFC 3977 §3.1.1);
  - `CAPABILITIES` parsing exposed as questions (`has_over`, `over_accepts_message_id`,
    `needs_mode_reader`) rather than a set of strings;
  - `GROUP`, `LIST ACTIVE`, `LIST NEWSGROUPS`, `LIST ACTIVE.TIMES` and
    `LIST OVERVIEW.FMT` responses;
  - `OVER`/`XOVER` records, including `:full` fields and servers that reorder them;
  - RFC 5322 headers with folding, case-insensitive lookup and repeated fields;
  - articles with `Content-Type`/`Content-Transfer-Encoding` handling:
    `quoted-printable`, base64, declared charsets via `encoding_rs`, and a Windows-1252
    fallback for unlabelled 8-bit text;
  - RFC 2047 encoded words, including adjacent-word whitespace elision and words split
    across a fold;
  - `Date` parsing covering the RFC 5322 §4.3 obsolete forms (two-digit years, named
    zones, missing seconds, nested comments).
- Validated newtypes (`MessageId`, `GroupName`, `HeaderName`, `Wildmat`) so that anything
  reaching a command line has already been checked for CRLF injection.
- Integration tests driving the public API with captured INN-shaped output.
- Cargo workspace skeleton with four crates (`nntp-proto`, `nntp-client`,
  `nntp-testserver`, `nntp-tui`), shared lint configuration and dual MIT/Apache-2.0
  licensing.
- GitHub Actions CI running `cargo fmt --check`, `cargo clippy -D warnings`,
  `cargo test` and a release build.
- Documentation scaffold: architecture notes, ADR directory, RFC 3977 coverage matrix.

### Fixed

- `quoted-printable` decoding no longer strips whitespace before a *soft* line break,
  which turned `this to =\r\nsay` into `this tosay`. Whitespace before a *hard* line
  break is stripped instead (RFC 2045 §6.7 rule 3), and whitespace written as an explicit
  `=20` escape is never stripped — that is what keeps a `-- ` signature separator intact.
  Found by the transcript integration test, not by a unit test.

[Unreleased]: https://github.com/edusouza/rust-nntp/commits/main
