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
- `nntp-client`, the blocking client:
  - `Connection<S>` framing over any `Read + Write`, with per-line and per-block size
    limits and a poisoned-connection flag so a truncated read can never be mistaken for
    the next response;
  - `Client<S>` with one method per command, holding the session state those commands
    depend on: capabilities, selected group, authentication, and which overview command
    this server actually accepts;
  - opening negotiation that copes with transit servers (`MODE READER`) and with servers
    that predate `CAPABILITIES`;
  - automatic `OVER` → `XOVER` fallback, decided from capabilities where possible and
    from a refusal where not, then remembered for the session;
  - streaming variants of `LIST` and `OVER` so a multi-megabyte response does not have to
    be buffered before the caller sees anything;
  - a `ClientError` taxonomy organised by what the caller can do about it, with
    `is_connection_fatal`, `is_transient` and `needs_authentication`;
  - a TCP connector with connect/read/write timeouts that tries every resolved address.
- `nntp-testserver`, a fake NNTP server:
  - an in-memory corpus, deliberately awkward: sparse article numbers, RFC 2047 subjects
    in both encodings, an unlabelled Latin-1 header, a body line beginning with `.`, a
    `Date` no parser can read, a moderated group and an empty group;
  - four capability profiles — modern, no-`OVER`, transit (`MODE READER` required) and
    pre-RFC-3977 — plus optional `AUTHINFO` credentials;
  - quirks that reproduce real misbehaviour on demand: refusing `LIST OVERVIEW.FMT`,
    refusing open-ended `OVER` ranges, advertising `OVER` but refusing it, truncating a
    block and hanging up, sending a line far past any limit, speaking bare LF, and
    disappearing mid-session;
  - a `TestServer` that binds an ephemeral loopback port so tests run in parallel, and a
    standalone binary for driving the reader offline.
- TLS, behind a default-on `tls` feature:
  - implicit TLS on port 563 and `STARTTLS` on 119 (RFC 4642), with the handshake driven
    to completion at connect time so a certificate problem is reported by the connection
    attempt rather than by whichever command happened to be first;
  - `TlsOptions` for a private certificate authority (`extra_ca_file`) and for verifying
    against a name other than the host connected to;
  - `Security::{Plain, ImplicitTls, StartTls}`, where choosing a mode also selects that
    mode's conventional port.
- TLS in `nntp-testserver`: a certificate authority and server certificate generated at
  start-up, exposed as PEM so a client can be told to trust them, plus `--tls`,
  `--starttls` and `--ca-out` on the binary.
- `nntp-tui`, the executable, with a command-line surface:
  - `doctor` — probes a server and reports greeting, capabilities, reader mode,
    authentication, overview command, clock skew and overview layout, optionally selecting
    a group and fetching an article from it. A probe that fails is reported rather than
    fatal. This is the only practical way to find out what a real server does, since the
    test suite runs against a fake one;
  - `groups`, `overview`, `article` — a usable reader before the terminal UI exists;
  - `config path | show | init` — writes a commented example configuration and reports
    where it and the log file live;
  - TOML configuration with named servers, `password_command` so the password can live in
    a password manager, per-server TLS settings, and field-by-field overrides from the
    command line;
  - logging to standard error for the command line and to a file for the terminal UI,
    which owns the terminal.
- End-to-end tests of the built binary as a subprocess, covering argument parsing, the
  configuration merge, connection set-up and output formatting together.
- End-to-end tests: the real client over a real socket against that server, covering the
  full reading session, each misbehaviour above, and the TLS paths — including the two
  failures that matter, an untrusted certificate and a certificate for the wrong name.
- Integration tests driving the public API with captured INN-shaped output.
- Cargo workspace skeleton with four crates (`nntp-proto`, `nntp-client`,
  `nntp-testserver`, `nntp-tui`), shared lint configuration and dual MIT/Apache-2.0
  licensing.
- GitHub Actions CI running `cargo fmt --check`, `cargo clippy -D warnings`,
  `cargo test` and a release build.
- Documentation scaffold: architecture notes, ADR directory, RFC 3977 coverage matrix.

### Security

- `AUTHINFO PASS` is refused on an unencrypted connection unless the caller explicitly
  opts in. Sending a password in clear text should be a decision, not a default.
- Passwords are redacted from command logs at the point of encoding, so no log level can
  reveal one.
- Certificate verification is always on. There is no `accept_invalid_certs` flag anywhere
  in the crate, and none can be reached from the configuration file; a private CA is
  supported instead.
- `STARTTLS` follows RFC 4642 §2.2 rather than treating it as an optional nicety: the
  upgrade is refused after authentication, the capability list learned in the clear is
  discarded once encrypted, and any data buffered between the `382` response and the
  handshake aborts the connection instead of being handed to the TLS layer.

### Fixed

- `GroupName`, `MessageId` and `HeaderName` now honour field width in `Display`
  (`f.pad` rather than `f.write_str`), so `{:<40}` in a caller's format string actually
  pads. Every aligned column built from these types was silently ragged; the group listing
  is what made it visible.
- A `501` reply is no longer read as "this server does not implement the command". Only
  `500` and `503` say anything about the command; `501` is a complaint about the
  *arguments*, and servers use it to refuse an `OVER` range with an open upper bound.
  Before this fix, one such request disabled overview fetching for the rest of the
  session, because the client remembered "no `OVER`, no `XOVER`" and never tried again.
  Found by pointing the client at a fake server configured to refuse open-ended ranges —
  which is exactly what `nntp-testserver` exists for.
- `quoted-printable` decoding no longer strips whitespace before a *soft* line break,
  which turned `this to =\r\nsay` into `this tosay`. Whitespace before a *hard* line
  break is stripped instead (RFC 2045 §6.7 rule 3), and whitespace written as an explicit
  `=20` escape is never stripped — that is what keeps a `-- ` signature separator intact.
  Found by the transcript integration test, not by a unit test.

[Unreleased]: https://github.com/edusouza/rust-nntp/commits/main
