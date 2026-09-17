# rust-nntp

A terminal news reader for Usenet, written in Rust.

`rust-nntp` is an [RFC 3977](https://www.rfc-editor.org/info/rfc3977/) NNTP client split
into reusable layers: a dependency-light protocol crate, a blocking client crate, and a
[ratatui](https://ratatui.rs) terminal UI on top.

```text
┌ Groups — 5 ────────────┐┌ Articles — comp.lang.rust (2) ─┐┏ Article — 1–6 of 6 ━━━━━━━━━━━━━━━━━━┓
│comp.lang.c  ≤18342     ││  café and crates               │┃From: Åsa Lindqvist <asa@example.se>  ┃
│comp.lang.rust  ≤6      ││› Re: café and crates           │┃Newsgroups: comp.lang.rust            ┃
│de.comp.test  ≤97       ││                                │┃Subject: Re: café and crates          ┃
│misc.test  ≤3           ││                                │┃Date: Wed, 17 Sep 2026 10:11:12 +0200 ┃
│news.announce.newgroups ││                                │┃References: <4237@example.org>        ┃
│                        ││                                │┃                                      ┃
│                        ││                                │┃> On the subject of café, I have this ┃
│                        ││                                │┃to say.                               ┃
│                        ││                                │┃Agreed. The encoded-word handling is  ┃
│                        ││                                │┃what makes this readable              ┃
│                        ││                                │┃at all: without it the subject above  ┃
│                        ││                                │┃would be mojibake.                    ┃
│                        ││                                │┃                                      ┃
│                        ││                                │┃--                                    ┃
│                        ││                                │┃Åsa                                   ┃
│                        ││                                │┃                                      ┃
│                        ││                                │┃                                      ┃
│                        ││                                │┃                                      ┃
│                        ││                                │┃                                      ┃
└────────────────────────┘└────────────────────────────────┘┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛
 news.example.org:563 [TLS]   2026-09-17 10:11 — Re: café and crates   ?: help  m: messages  q: quit
```

> **Status: pre-release.** See [CHANGELOG.md](CHANGELOG.md) for what works and the
> [issue tracker](https://github.com/edusouza/rust-nntp/issues) for what does not. The
> one thing to know before trusting it: every test runs against a fake server included in
> this repository, so nothing yet proves the client agrees with a real one
> ([#4](https://github.com/edusouza/rust-nntp/issues/4)).

## Try it in thirty seconds, without a Usenet account

```sh
cargo run -p nntp-testserver -- --port 1119      # terminal 1
cargo run -p nntp-tui -- --host 127.0.0.1 --port 1119 --no-tls   # terminal 2
```

The bundled server is deliberately awkward — sparse article numbers, RFC 2047 encoded
subjects, an unlabelled Latin-1 header, a body line beginning with a dot, a `Date` header
no parser can read — because those are the cases that break news readers.

## With a real server

```sh
nntp-tui config init                                  # write an example configuration
nntp-tui doctor --host news.example.org --tls         # check what the server supports
nntp-tui                                              # open the reader
```

Start with `doctor`. It reports the greeting, the capability list, whether reader mode or
authentication is needed, which overview command works, the clock difference and the
overview layout — every input that changes how the reader behaves. It is also what belongs
in a bug report.

There is a command-line mode too, which is useful on its own and scriptable:

```sh
nntp-tui groups --descriptions --pattern 'comp.lang.*'
nntp-tui overview comp.lang.rust -n 20
nntp-tui article '<abc123@example.org>'
```

The [user guide](docs/user-guide.md) covers configuration, key bindings and logging.

## Workspace layout

| Crate | Kind | Purpose |
| --- | --- | --- |
| [`nntp-proto`](crates/nntp-proto) | library | IO-free protocol layer: response/command grammar, multi-line blocks, overview and header parsing, RFC 2047, charsets, dates. No sockets, fully unit-testable. |
| [`nntp-client`](crates/nntp-client) | library | Blocking NNTP client over any `Read + Write` transport, with TCP/TLS connectors, timeouts, size limits and a typed command API. |
| [`nntp-testserver`](crates/nntp-testserver) | library + bin | A fake, corpus-driven NNTP server used by the test suite and for driving the UI without a real news server. |
| [`nntp-tui`](crates/nntp-tui) | library + bin | The `nntp-tui` executable: terminal UI plus a small CLI (`doctor`, `groups`, `overview`, `article`, `config`). |

## Design goals

1. **The protocol layer never touches IO.** Everything in `nntp-proto` is a pure function
   over bytes, so the grammar is testable without a network and reusable by other clients.
2. **Malformed input from the network must not panic.** `clippy::indexing_slicing`,
   `unwrap_used` and `expect_used` are denied workspace-wide; parsers return errors, and
   degrade per construct — an article with an unreadable `Date` is still shown.
3. **The UI thread never blocks on a socket,** and the UI's *decisions* are separable from
   its pixels, so both can be tested ([ADR-0003](docs/adr/0003-blocking-io-on-a-worker-thread.md),
   [ADR-0008](docs/adr/0008-ui-state-machine-separate-from-rendering.md)).
4. **Credentials and certificates are not negotiable.** `AUTHINFO PASS` is refused on an
   unencrypted link unless explicitly allowed, passwords are redacted from logs at the
   point of encoding, and certificate verification cannot be disabled from anywhere in the
   configuration.
5. **No `unsafe`.** `unsafe_code` is `forbid`den across the workspace.

## Documentation

- [Architecture](docs/architecture.md)
- [Architecture Decision Records](docs/adr/) — including the ones that turned out to
  matter: why there is no async runtime, why the tests use a fake server, and why the UI
  is split in three
- [RFC 3977 coverage matrix](docs/protocol-coverage.md)
- [User guide](docs/user-guide.md)
- [Contributing](CONTRIBUTING.md)

## Licence

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
