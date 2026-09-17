---
title: "Validate the client against a real news server"
labels: [validation, blocked]
status: open
---

## Problem

The whole test suite runs against [`nntp-testserver`](../../crates/nntp-testserver), which
implements our *reading* of RFC 3977. A misunderstanding shared by client and fake server
is invisible to it. Real servers also differ from the RFC in ways worth knowing:

- INN and Diablo disagree on `LIST OVERVIEW.FMT` field naming (`:bytes` vs `Bytes:`).
- Some servers require `MODE READER` before anything else and answer `480`/`500` otherwise.
- Some advertise `OVER` but only accept the message-id form, or reject a range with an
  open upper bound (`low-`).
- Commercial providers frequently disable `NEWNEWS`, `XPAT` and `LIST ACTIVE.TIMES`.
- `Date:` headers in the wild include forms RFC 5322 does not permit.

## Why it is blocked

The development and CI environment has no outbound access on TCP 119 or 563 (see
[ADR-0004](../adr/0004-fake-server-for-tests.md)), so this cannot be automated here.

## What to do

1. Run `nntp-tui doctor --server <host> --port 563 --tls` against at least one public
   server (`news.eternal-september.org`, `news.php.net`) and one commercial provider.
2. Attach the `doctor` output and a `RUST_LOG=nntp_client=trace` wire log (credentials
   redacted) to this issue.
3. Turn each divergence found into a fixture in `nntp-testserver` plus a regression test,
   and a row in [`docs/protocol-coverage.md`](../protocol-coverage.md).
4. Add an `#[ignore]`d integration test that runs against a real server when
   `NNTP_TEST_SERVER` is set, so the check is at least available on demand.
