# Architecture

## Layers

```text
┌──────────────────────────────────────────────────────────────────────┐
│ nntp-tui            lib + bin                                        │
│   tui::app          state machine: keys and events in, requests out  │
│   tui::ui           drawing, and no decisions                        │
│   tui::worker       owns the client, blocks on its own thread         │
│   cli / commands    argument parsing, doctor, groups, overview, …    │
│   config            TOML (serde) + platform paths                     │
│   readstate         read/unread ranges, and the .newsrc file          │
│   session           config ⊕ flags → a connected client              │
└───────────────────────────┬──────────────────────────────────────────┘
                            │ Request / Event channels
┌───────────────────────────▼──────────────────────────────────────────┐
│ nntp-client         lib: the conversation                            │
│   connector         TCP, implicit TLS, STARTTLS, timeouts            │
│   tls               rustls configuration and handshake               │
│   connection        framing: status lines, blocks, size limits        │
│   client            one method per command, plus session state        │
│   limits / error    size bounds, and an error taxonomy by remedy      │
└───────────────────────────┬──────────────────────────────────────────┘
                            │ &[u8] in, parsed values out
┌───────────────────────────▼──────────────────────────────────────────┐
│ nntp-proto          lib: the grammar (no IO, no clock)               │
│   body              MIME part trees, format=flowed                    │
│   response          status codes and classification                   │
│   command           encoding + CRLF-injection refusal                 │
│   block             dot-stuffing both ways, terminator detection      │
│   capabilities      CAPABILITIES, exposed as questions                │
│   group / list      GROUP, LIST ACTIVE / NEWSGROUPS / ACTIVE.TIMES    │
│   overview          OVER / XOVER + LIST OVERVIEW.FMT                  │
│   headers           folding, case-insensitive lookup, raw values      │
│   article           bodies, Content-Type, transfer encodings          │
│   mime              RFC 2047, charsets, quoted-printable, base64      │
│   date              RFC 5322 dates, including the obsolete forms      │
│   message_id/spec   validated newtypes and article/range selectors    │
└──────────────────────────────────────────────────────────────────────┘

nntp-testserver      lib + bin: a fake server, for tests and for driving the UI offline
```

Dependencies point strictly downward, and `nntp-proto` has no knowledge of sockets. A
[golden-file test](../crates/nntp-proto/tests) can feed it any byte sequence, including the
ones a real server would never send.

## The decisions that shape this

Each of these is an ADR, and each was arrived at because the obvious alternative had a
cost worth avoiding.

| Decision | ADR | Why |
| --- | --- | --- |
| Layered workspace, not one crate | [0002](adr/0002-layered-workspace.md) | The bugs that matter are parsing bugs against hostile input, and those are only cheap to test when the parser is a pure function. |
| Blocking IO on a worker thread, not async | [0003](adr/0003-blocking-io-on-a-worker-thread.md) | NNTP is a serialised conversation over one connection. `tokio` would colour every API for one background worker. |
| Test against an in-repo fake server | [0004](adr/0004-fake-server-for-tests.md) | No outbound 119/563 here, and a real server cannot be asked to misbehave on demand. It found real bugs on first contact. |
| TOML config, in-memory cache | [0005](adr/0005-config-and-state-storage.md) | Ship a working reader before designing a schema. Its read-state half is superseded by 0009. |
| Read state in a `.newsrc` file, one per server | [0009](adr/0009-newsrc-file-for-read-state.md) | It is the one thing this program stores that other programs read. Small, written rarely — none of what makes SQLite right for a cache applies. |
| rustls with a bundled root set | [0007](adr/0007-rustls-for-tls.md) | No C toolchain, identical on three platforms. Verification cannot be disabled from anywhere. |
| UI state machine separate from rendering | [0008](adr/0008-ui-state-machine-separate-from-rendering.md) | Otherwise the interface's decisions are only testable by drawing them and having a human look. |

## Threading model

One render thread owns all UI state; one worker thread owns the connection. They exchange
`Request` and `Event` values over `std::sync::mpsc` channels. There is no shared mutable
state and no locking on the render path.

The render loop is a poll loop, not an event-driven callback graph:

```text
loop {
    drain worker events            → mutate App
    if App.dirty { render }
    poll terminal event (timeout)  → mutate App / send Request
    else                           → App.tick()   (spinner)
    if App.should_quit { break }
}
```

A timeout on the terminal poll gives the spinner a tick without busy-waiting, and means a
worker event arriving while the user is idle is picked up promptly. Events are drained in a
burst before drawing, so a flurry of them costs one redraw rather than one each.

The worker reconnects on demand: a dropped connection leaves the interface usable and the
next request re-establishes it. What it cannot yet do is abandon a request already in
flight — see the cancellation issue.

## Error handling

- `nntp-proto` returns `ProtoError`: a byte sequence that could not be interpreted, or a
  value that cannot be encoded. `is_peer_fault` distinguishes the two, which decides what
  to log and at what level.
- `nntp-client` returns `ClientError`, organised by *what the caller can do about it*
  rather than by where it came from: retry (`is_transient`), get credentials
  (`needs_authentication`), fall back to an older command (`CommandNotSupported`), pick a
  different article (`NoSuchArticle`), or give up on the connection
  (`is_connection_fatal`).
- `nntp-tui` turns a `ClientError` into a status-bar message plus a log line. Errors are
  never fatal to the interface; the worst case is a disconnected state that the next
  request retries.

Library crates use `thiserror` and expose concrete enums. The binary uses `anyhow`, and
prints with `{:#}` so the whole context chain is shown — `connecting to news.example.org:563:
transport error: …` is far more useful than its last link alone.

Parsing degrades per construct rather than all-or-nothing. A status line that cannot be
read is fatal to the response; an article with an unparseable `Date` is still worth
showing, so `OverviewRecord` keeps the raw date text alongside the parsed value. Where a
whole block is parsed, `ListResult` returns the lines that failed next to the ones that
succeeded, so one malformed line out of a hundred thousand `LIST` entries costs one group
rather than the listing.

## Size limits and timeouts

All configurable, with defaults generous for real Usenet traffic yet bounded. A server
that never terminates a line or a block is otherwise an out-of-memory condition.

| Limit | Default | Why |
| --- | --- | --- |
| response line | 64 KiB | RFC 3977 caps *command* lines at 512 octets and says nothing about responses; `Path` headers exceed 1 KiB. |
| multi-line block | 32 MiB | Enough for a large binary article, small enough not to be a weapon. |
| block line count | 4 000 000 | `LIST ACTIVE` on a full feed is around 120 000 lines. |
| connect timeout | 20 s | |
| read/write timeout | 60 s / 30 s | Per read, not per command: a `LIST` that streams for a minute is fine as long as no single gap exceeds it. |

A limit violation poisons the connection deliberately: the rest of the over-long line is
still on the wire, so the client no longer knows where it is in the stream, and reading on
would parse the tail of one response as the status line of the next.

## Testing

The suite is entirely offline and runs on Linux, macOS and Windows.

| Layer | How it is tested |
| --- | --- |
| `nntp-proto` | unit tests over byte slices, including malformed input, plus transcript tests over captured INN-shaped output through the public API |
| `nntp-client` | the conversation driven over scripted in-memory streams, then end to end over a real socket against the fake server, including its deliberate misbehaviour |
| TLS | a real handshake against a certificate generated at run time, with verification left on; the two failure cases — untrusted CA, wrong name — are asserted to fail |
| `nntp-tui` read state | unit tests, including 4 000 random operations against an oracle |
| `nntp-tui` state machine | unit tests over key events and worker events |
| `nntp-tui` rendering | `TestBackend` snapshots, including terminals too small to use |
| `nntp-tui` as a whole | state machine plus worker plus a real socket plus the fake server |
| the binary | run as a subprocess, covering argument parsing, the config merge and output formatting |
| the README screenshot | regenerated and compared by a test, so it cannot quietly stop describing the program |

What is not covered: the twenty-line terminal poll loop, and agreement with any real news
server. The second of those is the important one, and it is tracked as an issue.
