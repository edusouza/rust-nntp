# ADR-0004: Test against an in-repo fake NNTP server

- **Status**: Accepted
- **Date**: 2026-09-17

## Context

Verifying the client needs a server. The options are a public news server, a locally
installed INN, or something we write ourselves.

Two constraints pushed the decision:

1. The development and CI environments have no outbound access on TCP 119 or 563 — the
   policy allows HTTPS through a proxy only. Connections to public news servers time out.
2. Even with access, public servers are shared, rate-limited, change their article numbers
   continuously, and cannot be asked to produce the malformed responses we most need to
   test.

## Decision

`nntp-testserver` is a fake NNTP server in the workspace. It listens on `127.0.0.1:0`,
serves an in-memory corpus of groups and articles, implements the RFC 3977 reader subset
the client uses, and can be configured to misbehave on purpose (refuse `MODE READER`,
require authentication, advertise no `OVER`, send `XOVER` only, drop the connection
mid-block, exceed line limits).

Integration tests start it in-process. The same crate ships a binary so the TUI can be
driven end to end locally:

```sh
cargo run -p nntp-testserver -- --port 1119
cargo run -p nntp-tui -- --server 127.0.0.1:1119 --no-tls
```

## Consequences

### Positive

- Tests are deterministic, offline, fast, and run on every platform in CI.
- Server quirks become test fixtures instead of anecdotes. Every bug found against a real
  server can be reproduced here and kept.
- Contributors can develop the UI without a Usenet account.

### Negative / accepted costs

- The fake is written against our reading of the RFCs, so it cannot catch a
  misunderstanding shared by both sides. Validation against a real server remains
  necessary and is tracked as a separate, `#[ignore]`d test plus a `doctor` subcommand.
- It is more code to maintain, and it must not drift into "whatever makes the tests pass".

## Alternatives considered

- **Recorded transcripts (golden files) only.** Cheap and useful — and used for
  `nntp-proto` unit tests — but they cannot exercise connect/timeout/TLS paths or a
  stateful `GROUP` → `OVER` sequence.
- **INN in a Docker container.** Most faithful, but unusable in this environment, slow in
  CI, and awkward on macOS and Windows runners. Worth adding later as an optional,
  opt-in CI job.
- **Mocking at the `Read + Write` boundary only.** Already covered by unit tests in
  `nntp-client`; does not exercise real sockets, so it would leave the connector untested.
