# ADR-0002: Split the client into a layered workspace

- **Status**: Accepted
- **Date**: 2026-09-17

## Context

An NNTP reader is three separable concerns: a wire grammar, a stateful conversation over a
socket, and a user interface. Bundling them into a single binary crate makes the grammar
untestable without a socket, and makes the interesting parsing bugs reachable only through
the UI.

## Decision

Four crates in one Cargo workspace:

- `nntp-proto` — the wire grammar as pure functions over bytes. No `std::net`, no sockets,
  no clock. Parses status lines, multi-line blocks, `CAPABILITIES`, `LIST` variants,
  `GROUP`, `OVER`/`XOVER`, headers, RFC 2047 encoded words.
- `nntp-client` — the conversation. Owns a transport (`Read + Write`), tracks connection
  state (greeting, posting allowed, authenticated, selected group), enforces size limits
  and timeouts, and exposes one method per NNTP command with typed results.
- `nntp-testserver` — a fake server driven by an in-memory corpus, used as a dev-dependency
  by integration tests and as a binary for manual UI work.
- `nntp-tui` — the executable: terminal UI plus a small CLI.

Dependencies point strictly downward: `nntp-tui` → `nntp-client` → `nntp-proto`.

## Consequences

### Positive

- The grammar is tested with byte-slice inputs, including the malformed ones that a network
  test cannot easily produce.
- `nntp-client` is generic over its transport, so the same code path is exercised by a
  `Cursor<Vec<u8>>` in unit tests, a TCP socket, and a TLS stream.
- `nntp-proto` is publishable on its own and useful to other clients and to server code.

### Negative / accepted costs

- More manifests, more `pub` surface, and some types have to be re-exported from
  `nntp-client` so callers do not have to depend on `nntp-proto` directly.
- Workspace-wide `missing_docs` means every public item in the lower crates needs a doc
  comment.

## Alternatives considered

- **Single crate with modules.** Less ceremony, but nothing prevents the parser from
  reaching for the socket, and that boundary erodes quietly.
- **Two crates (`nntp` + `nntp-tui`).** Reasonable; rejected because the size limits and
  timeout policy in the client layer are exactly the kind of thing that should not be
  entangled with grammar tests.
