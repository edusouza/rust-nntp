# rust-nntp

A terminal news reader for Usenet, written in Rust.

`rust-nntp` is an [RFC 3977](https://www.rfc-editor.org/info/rfc3977/) NNTP client split
into reusable layers: a dependency-light protocol crate, a blocking client crate, and a
[ratatui](https://ratatui.rs) terminal UI on top.

> **Status: pre-release.** The workspace is being built up milestone by milestone; see
> [CHANGELOG.md](CHANGELOG.md) for what already works and the
> [issue tracker](https://github.com/edusouza/rust-nntp/issues) for what does not.

## Workspace layout

| Crate | Kind | Purpose |
| --- | --- | --- |
| [`nntp-proto`](crates/nntp-proto) | library | IO-free protocol layer: response/command grammar, multi-line blocks, overview and header parsing. No sockets, fully unit-testable. |
| [`nntp-client`](crates/nntp-client) | library | Blocking NNTP client over any `Read + Write` transport, with TCP/TLS connectors, timeouts and a typed command API. |
| [`nntp-testserver`](crates/nntp-testserver) | library + bin | A fake, corpus-driven NNTP server used by integration tests and for driving the UI without a real news server. |
| [`nntp-tui`](crates/nntp-tui) | bin | The `nntp-tui` executable: terminal UI plus a small CLI (`doctor`, `groups`, `article`). |

## Design goals

1. **The protocol layer never touches IO.** Everything in `nntp-proto` is a pure function
   over bytes, so the grammar is testable without a network and reusable by other clients.
2. **Malformed input from the network must not panic.** `clippy::indexing_slicing`,
   `unwrap_used` and `expect_used` are denied workspace-wide; parsers return errors instead.
3. **The UI thread never blocks on a socket.** Network work lives on a worker thread and
   communicates through channels.
4. **No `unsafe`.** `unsafe_code` is `forbid`den across the workspace.

## Documentation

- [Architecture](docs/architecture.md)
- [Architecture Decision Records](docs/adr/)
- [RFC 3977 coverage matrix](docs/protocol-coverage.md)
- [User guide](docs/user-guide.md)
- [Contributing](CONTRIBUTING.md)

## Licence

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
