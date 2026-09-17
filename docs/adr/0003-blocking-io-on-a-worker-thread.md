# ADR-0003: Blocking IO on a worker thread instead of an async runtime

- **Status**: Accepted
- **Date**: 2026-09-17

## Context

The TUI must never block on the network: a `LIST` against a full-feed server returns
hundreds of thousands of lines and can take tens of seconds. The usual Rust answer is
`tokio`, which would make `nntp-client` async and force every caller — including unit
tests — into an executor.

But NNTP is a strictly serialised, stateful conversation over a *single* connection:
`GROUP` selects state that the next `OVER` depends on, so commands cannot be pipelined
usefully by a reader client, and there is no fan-out to parallelise. The concurrency the UI
needs is exactly one background worker.

## Decision

`nntp-client` is blocking and synchronous, generic over `Read + Write`. `nntp-tui` runs the
client on a dedicated worker thread and communicates with the render loop over channels:

```text
 ┌─ main thread ────────────┐          ┌─ worker thread ──────────┐
 │ crossterm event poll     │  Request │ nntp_client::Client      │
 │ ratatui render loop      │ ───────▶ │ blocking socket IO       │
 │ App state (owned here)   │ ◀─────── │                          │
 └──────────────────────────┘   Event  └──────────────────────────┘
```

The render loop polls terminal events with a timeout and drains the event channel on every
iteration, so a slow command shows a spinner instead of freezing the terminal.
Cancellation is cooperative: the worker checks for a cancel flag between response lines.

## Consequences

### Positive

- `nntp-proto` and `nntp-client` have no runtime dependency at all, so unit tests are plain
  `#[test]` functions over `Cursor` and the crates stay usable from sync and async callers
  alike (an async caller can wrap them in `spawn_blocking`).
- Read/write timeouts are a socket option away (`set_read_timeout`), with no executor
  timer involved.
- Far less dependency weight and compile time than pulling `tokio` into every layer.

### Negative / accepted costs

- Multiple simultaneous connections (e.g. prefetching bodies while browsing) need a thread
  each rather than a task each. For a reader with one or two connections this is fine; a
  batch downloader with 20 connections would want revisiting.
- Cancellation is not instant: a command already blocked in `read()` only aborts when the
  read timeout expires or the socket is shut down. The worker exposes an explicit
  "abandon connection" path for that case.
- We hand-roll the request/response plumbing that an async runtime would provide.

## Alternatives considered

- **`tokio` throughout.** Rejected: it colours the entire API for one background worker,
  and `spawn_blocking` around a sync client gives async users the same thing anyway.
- **Blocking client called directly from the render loop.** Simplest, and genuinely
  unacceptable: any slow command freezes the UI, including the quit key.
- **`std::thread` plus shared `Mutex<AppState>`.** Rejected: lock contention with the
  render loop, and the UI would have to poll state rather than react to discrete events.
