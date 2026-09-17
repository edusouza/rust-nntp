# User guide

> Everything below is implemented and tested; `nntp-tui --help` is the authoritative
> reference for the command line.

## Installing

```sh
cargo install --git https://github.com/edusouza/rust-nntp nntp-tui
```

Or from a clone:

```sh
cargo build --release -p nntp-tui
./target/release/nntp-tui --help
```

## Trying it without a Usenet account

The workspace ships a fake server, so you can drive the reader offline:

```sh
cargo run -p nntp-testserver -- --port 1119          # terminal 1

# The reader:
cargo run -p nntp-tui -- --host 127.0.0.1 --port 1119 --no-tls          # terminal 2
# Or the command line:
cargo run -p nntp-tui -- groups --host 127.0.0.1 --port 1119 --no-tls
```

The fake server is deliberately awkward — sparse article numbers, encoded subjects, an
unlabelled Latin-1 header, a body line starting with a dot, a `Date` header no parser can
read. `--help` on it lists flags for misbehaving on purpose (`--profile legacy`,
`--reject-open-ranges`, `--tls`, and more).

## Start with `doctor`

Run this first against any new server. Every line is something that changes how the
reader behaves, so it is also the right thing to paste into a bug report:

```sh
nntp-tui doctor --host news.example.org --tls --group comp.lang.rust
```

```text
server:      news.example.org:563 (command line)
transport:   ImplicitTls
connected:   in 41ms
encrypted:   yes
greeting:    200 news.example.org InterNetNews ready
posting:     yes
implementation: INN 2.7.1
capabilities:
  VERSION 2
  READER
  OVER MSGID
  ...
reader mode: yes
overview:    OVER, including by message-id
HDR:         yes
NEWNEWS:     no
STARTTLS:    yes
AUTHINFO:    USER/PASS
COMPRESS:    no
auth:        accepted
server date: 2026-09-17T13:41:02+00:00 (clock differs from ours by 1s)
overview fmt: Subject, From, Date, Message-ID, References, bytes, lines, Xref
```

A probe that fails is reported rather than fatal: "this server has no `NEWNEWS`" is the
answer, not an error.

## Configuration

`nntp-tui config init` writes a commented example to the platform configuration
directory, and `nntp-tui config path` says where that is. A minimal file:

```toml
default_server = "eternal-september"

[servers.eternal-september]
host = "news.eternal-september.org"
security = "implicit-tls"          # implicit-tls | starttls | plain
username = "your-username"
password_command = "pass show news/eternal-september"
```

Notes worth reading once:

- **`security` defaults to `implicit-tls`.** Connecting to a plaintext server needs
  `security = "plain"` or `--no-tls`. That is deliberate: the default should be the safe
  one.
- **Do not put the password in the file.** A password in a configuration file is a
  password in every backup of that file. Two better options, and at most one may be set:
  `password_env = "NNTP_PASSWORD"` reads an environment variable, and
  `password_command = "pass show news"` runs a command and uses its first line. Prefer
  `password_env`: it has no shell in the path, whereas `password_command` goes through
  `sh -c` or `cmd /C` — and on Windows `cmd` re-parses what `%VAR%` expands to, so `&`,
  `|`, `<` and `>` in a password get interpreted rather than passed on. Either way, a
  source that fails is an error, never an empty password.
- **`allow_plaintext_auth` is off.** `AUTHINFO PASS` sends the password in clear text, so
  it has to be asked for, with `--allow-plaintext-auth` or the configuration key.
- **A private certificate authority** goes in `extra_ca_file`. There is no way to disable
  certificate verification; see
  [ADR-0007](adr/0007-rustls-for-tls.md) for why.

Command-line flags override the configured server field by field, so this uses the
credentials from `es` against a server on your own machine:

```sh
nntp-tui --server es groups --host 127.0.0.1 --port 1119 --no-tls
```

## Reading from the command line

```sh
# Every group the server carries, with watermarks and posting status.
nntp-tui groups

# Group descriptions, narrowed to a hierarchy.
nntp-tui groups --descriptions --pattern 'comp.lang.*'

# The twenty newest articles in a group.
nntp-tui overview comp.lang.rust -n 20

# One article, by number within a group or by message-id.
nntp-tui article 4242 --group comp.lang.rust
nntp-tui article '<abc123@example.org>'

# Just the headers, or just the body, or exactly what arrived.
nntp-tui article '<abc123@example.org>' --part headers
nntp-tui article '<abc123@example.org>' --part body
nntp-tui article '<abc123@example.org>' --raw
```

`groups` counts are shown as `≤6` on purpose: `LIST ACTIVE` reports watermarks, and
expiry and cancellation leave gaps, so the span is an upper bound rather than a count.

## Validating against a real server

Every automated test that CI runs uses the bundled fake server, which implements *our*
reading of the RFCs — so a misunderstanding shared by the client and the fake server would
be invisible to all of them. There is an opt-in suite that closes that gap against a real
server, and if you have a news account you can run it yourself:
[`docs/validating-against-a-real-server.md`](validating-against-a-real-server.md) is the
runbook, covering the `doctor` probe and then the suite, which checks real `LIST` output,
real overview records, real articles, and that `XOVER` agrees with `OVER`. It has been run
against INN 2.8.0 and passes;
[the results are recorded](protocol-coverage.md#verified-against-a-real-server).

## Logs

Nothing is logged above `warn` by default. To see the conversation with the server:

```sh
RUST_LOG=nntp_client=trace nntp-tui doctor --host news.example.org --tls
```

Passwords are redacted at the point of encoding, so no log level reveals one. Everything
else is shown verbatim, so redact host names and group names yourself before sharing a
log.

`--log-file <PATH>` writes to a file instead of standard error; the terminal UI uses that
by default, since it owns the terminal.

## The reader

`nntp-tui` with no subcommand opens the reader against the configured server;
`nntp-tui tui --host …` takes the same connection flags as everything else.

Three panes: groups, the article list for the selected group, and the article itself. The
focused pane has a thick border. Network work happens on a separate thread, so the
interface stays responsive while a large group loads — the spinner in the status bar turns
while something is outstanding.

### Key bindings

| Keys | Action |
| --- | --- |
| `Tab` / `Shift-Tab` | next / previous pane |
| `h` `l` or `←` `→` | move focus left / right |
| `j` `k` or `↓` `↑` | move down / up |
| `Ctrl-d` / `Ctrl-u` | page down / up |
| `PageDown` / `PageUp` | page down / up |
| `g` / `G` | first / last |
| `Enter` | open the group or article under the cursor |
| `n` / `p` | next / previous article, opening it |
| `/` | filter groups by name or description |
| `Esc` | clear the filter, or close an overlay |
| `r` | reload the focused pane |
| `m` | recent messages |
| `?` or `F1` | help |
| `q` or `Ctrl-C` | quit |

The cursor clamps at the ends of a list rather than wrapping: a list that jumps back to
the top when you hold a key down is disorienting, and a news reader is mostly held-down
keys.

### What the display tells you

- **`≤n` next to a group** is an upper bound, not a count. `LIST ACTIVE` reports only the
  watermarks, and expiry and cancellation leave gaps.
- **`›` before a subject** marks a follow-up. Replies are marked rather than indented:
  real threads arrive out of order and with missing parents, so an indent would be a lie
  until threading lands in v0.2.
- **The badge at the bottom left** is green for TLS and yellow for a plaintext connection,
  and red when the connection has dropped. The worker reconnects on the next request, so
  a dropped connection is a nuisance rather than the end of the session.
- **Quoted lines are dimmed.** On Usenet most of a follow-up is quotation.
- **An error takes over the status bar** until the next keystroke; `m` shows the ones that
  have scrolled past.

### What it does not do yet

Posting, persistent read/unread state, threading and a disk cache are all v0.2 or later;
see the [roadmap](https://github.com/edusouza/rust-nntp/issues/3). Read state is kept for
the session only, so closing the reader forgets what you have read — the single most
important gap, and the first thing in the v0.2 scope.
