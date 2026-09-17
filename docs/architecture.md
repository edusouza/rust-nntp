# Architecture

## Layers

```text
┌──────────────────────────────────────────────────────────────┐
│ nntp-tui            bin: terminal UI + CLI                   │
│   app/              state machine, key handling, layout      │
│   worker            owns the client, serialises requests      │
│   config            TOML (serde) + platform paths             │
└───────────────────────────┬──────────────────────────────────┘
                            │ Request / Event channels
┌───────────────────────────▼──────────────────────────────────┐
│ nntp-client         lib: the conversation                    │
│   connector         TCP, implicit TLS, STARTTLS, timeouts     │
│   connection        framing: status lines, multi-line blocks  │
│   client            one method per NNTP command, typed        │
└───────────────────────────┬──────────────────────────────────┘
                            │ &[u8] in, parsed values out
┌───────────────────────────▼──────────────────────────────────┐
│ nntp-proto          lib: the grammar (no IO, no clock)       │
│   response          status codes and classification           │
│   command           encoding + CRLF-injection validation      │
│   block             dot-unstuffing, terminator detection      │
│   capabilities      CAPABILITIES parsing                      │
│   group / list      LIST ACTIVE, LIST NEWSGROUPS, GROUP       │
│   overview          OVER / XOVER + LIST OVERVIEW.FMT          │
│   headers           unfolding, case-insensitive lookup        │
│   mime              RFC 2047 encoded words, charset decoding  │
└──────────────────────────────────────────────────────────────┘
```

Dependencies point strictly downward. `nntp-proto` has no knowledge of sockets; a
[golden-file test](../crates/nntp-proto/tests) can feed it any byte sequence, including the
ones a real server would never send.

## Why a dedicated `nntp-proto` crate

See [ADR-0002](adr/0002-layered-workspace.md). In short: the bugs that matter in an NNTP
client are parsing bugs against hostile or merely eccentric input, and those are only cheap
to test when the parser is a pure function.

## Threading model

See [ADR-0003](adr/0003-blocking-io-on-a-worker-thread.md). One render thread owns all UI
state; one worker thread owns the connection. They exchange `Request` and `Event` values
over `std::sync::mpsc` channels. There is no shared mutable state and no locking on the
render path.

The render loop is a poll loop, not an event-driven callback graph:

```text
loop {
    drain worker events            → mutate App
    poll terminal event (timeout)  → mutate App / send Request
    if App.dirty { render }
    if App.should_quit { break }
}
```

A timeout on the terminal poll gives the spinner and any timed UI state a tick without
busy-waiting.

## Error handling

- `nntp-proto` returns `ProtoError`: a malformed or unparseable byte sequence.
- `nntp-client` returns `ClientError`, which wraps `ProtoError` and `io::Error` and adds
  the protocol-level failures that are not grammar errors: an unexpected but well-formed
  response code, a server error reply (4xx/5xx), authentication required or rejected,
  a size limit exceeded.
- `nntp-tui` converts `ClientError` into a status-bar message plus a log line. Errors are
  never fatal to the UI unless the connection is unusable, in which case the app returns to
  a disconnected state that can be retried.

Library crates use `thiserror` and expose concrete error enums. The binary uses `anyhow`
for start-up paths where a human-readable chain is what matters.

## Size limits and timeouts

All configurable on the client, with defaults chosen to be generous for real Usenet traffic
yet bounded:

| Limit | Default | Why |
| --- | --- | --- |
| response line | 64 KiB | RFC 3977 caps command lines at 512 octets but not response lines; headers in the wild exceed 1 KiB. |
| multi-line block | 32 MiB | Enough for a large binary article, small enough not to be an OOM vector. |
| overview lines per request | unbounded, streamed | `OVER` over a wide range is the common case; the client yields records rather than buffering all of them. |
| connect timeout | 20 s | |
| read/write timeout | 60 s | Some servers take a long time to answer the first `LIST`. |
