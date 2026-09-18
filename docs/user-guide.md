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

The `[ui]` table holds the reader's preferences, all optional:

```toml
[ui]
mark_read_on_open = true      # opening an article marks it read
unread_only = false           # start with the unread filter on
initial_articles = 300        # how many of a group's newest articles to load
overview_chunk = 500          # overview records per round trip
date_format = "%Y-%m-%d %H:%M"
```

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
| `u` | show only unread articles, or everything again |
| `M` | mark the article under the cursor read, or unread if it was read |
| `c` | catch up: mark the whole group read |
| `/` | filter groups by name or description |
| `↑` `↓` `PageUp` `PageDown` `Home` `End` while filtering | move through what the filter left, without leaving the filter |
| `Esc` | stop a request in progress; otherwise clear the filter or close an overlay |
| `r` | reload the focused pane |
| `m` | recent messages |
| `?` or `F1` | help |
| `q` or `Ctrl-C` | quit |

While a filter is being typed, the arrows and the page keys move through the groups it
left — `j` and `k` cannot, because they are filter text. `Enter` closes the filter and
keeps it applied; a second `Enter` opens the group under the cursor.

The cursor clamps at the ends of a list rather than wrapping: a list that jumps back to
the top when you hold a key down is disorienting, and a news reader is mostly held-down
keys.

### What the display tells you

- **`≤n` next to a group** is how many articles are **unread**, and an upper bound rather
  than a count: `LIST ACTIVE` reports only the watermarks, and expiry and cancellation
  leave gaps that nothing short of asking the server can tell apart from articles nobody
  has read. A group with nothing left says `read`; the total is in the status bar the
  moment you open the group. A group with something unread has its name in bold, so the
  shape of the list answers "where is there anything new" without reading any numbers.
- **`•` before a subject** marks an unread article, and its subject is bold. Read articles
  are dimmed and unmarked — the other way round would put a mark on nearly every line of
  a group you follow, which is no mark at all.
- **`›` before a subject** marks a follow-up. Replies are marked rather than indented:
  real threads arrive out of order and with missing parents, so an indent would be a lie
  until threading lands in v0.2.
- **The badge at the bottom left** is green for TLS and yellow for a plaintext connection,
  and red when the connection has dropped. The worker reconnects on the next request, so
  a dropped connection is a nuisance rather than the end of the session.
- **Quoted lines are dimmed.** On Usenet most of a follow-up is quotation.
- **"n other parts" above the body** lists what the article carried that is not on screen:
  attachments, and the HTML copy of a message that also arrived as plain text. A
  `multipart/alternative` from a mail-to-news gateway shows its plain-text half; anything
  the reader cannot render is named, with its type and size, rather than dumped into the
  pane. An article that is *only* an attachment says `(no text in this article)` instead of
  showing a blank pane.
- **Paragraphs wrapped by the sender are rejoined** when the article says
  `format=flowed` (RFC 3676), so a message written in a 70-column mail client does not
  arrive as a column of short lines. Quote depth is respected, so a reply never absorbs
  the text it is quoting, and a `-- ` signature separator stays a break.
- **`signed (signature not checked)`** appears for an article carrying a PGP or S/MIME
  signature — the detached kind, or inline clearsign armour. The signed text is shown
  without the armour, the `Hash:` header or the signature block, and dash-escaping is
  undone so a signed patch does not read `- --- a/file`. The wording is exact: **nothing
  here verifies a signature.** This project does no cryptography, and a reader that
  implied a signature had been checked would be worse than one that says nothing. Mailing-
  list gateways sign nearly everything they relay, which is why the signature is reported
  as a fact about the article rather than listed among its parts.
- **`nntp-tui article --raw`** shows the body exactly as it arrived — boundaries, base64
  and all — when you need to see what the sender actually sent.
- **An error takes over the status bar** until the next keystroke; `m` shows the ones that
  have scrolled past.

### Stopping a long request

A `LIST ACTIVE` against a full-feed server is tens of thousands of lines and takes tens of
seconds. **`Esc` abandons it**, and while anything is outstanding the status bar says so
(`Esc: stop`). The spinner keeps turning until the worker notices, which takes at most one
line of the response.

Two things worth knowing, because they are visible:

- **The connection is dropped when you cancel.** Stopping part-way through a response
  leaves it pointing into the middle of a reply, so it cannot be reused; the next request
  reconnects, which you see as `connecting to …` in the status bar. That is the honest
  cost, and the message pane (`m`) says so when it happens.
- **A server that has gone silent is a different problem.** Cancelling notices between
  lines of a response that is arriving; if the server has stopped sending altogether, what
  ends the wait is the read timeout (`read_timeout_secs`, 60 by default). Lower it if you
  are on a flaky link.

`Esc` keeps its other meanings when nothing is outstanding, and always belongs to the
filter while you are typing one.

### Watching a big group load

Opening a group fetches its newest articles in chunks, and **each chunk appears as it
arrives** rather than the pane staying empty until the whole range is in. The newest chunk
is fetched first, so the articles you came for are the ones that show up first; the status
bar counts them as they land (`comp.lang.c: 1500 articles listed…`) and says how many are
unread when the fetch finishes.

- `overview_chunk` under `[ui]` sets the chunk size — smaller means more round trips and a
  list that grows in smaller steps.
- **Your place is kept.** A cursor on the newest article follows the newest as records
  arrive; a cursor you have moved stays on the article it is on, even though older records
  landing in front of it change its position in the list.
- `Esc` still stops the fetch (above). What has already arrived stays listed — it is real
  data, and throwing it away would be its own surprise.

### Read and unread

What you have read is remembered between runs, in the `.newsrc` format every newsreader
since the 1980s has used:

```text
comp.lang.c: 1-4237,4240,4242-4250
misc.test! 1-100
```

- **One file per server**, under the platform data directory —
  `~/.local/share/nntp-tui/newsrc/news.example.org.newsrc` on Linux. Per server because
  article numbers are assigned by the server: the same group on two servers has two
  unrelated numberings, and one shared file would mark articles read on one server
  because their numbers happened to be read on another.
- **Interoperable on purpose.** `slrn`, `tin` and `nn` read and write the same format, so
  the file can be copied between readers. The `:` / `!` subscription flag is kept and
  written back even though this reader has no subscription list yet, because dropping a
  field another reader wrote would make that claim false.
- **Opening an article marks it read**, unless you set `mark_read_on_open = false` under
  `[ui]`, which leaves `M` as the only way an article becomes read. `unread_only = true`
  opens the reader with the unread filter already on; `u` toggles it either way.
- **Nothing here can stop the reader from starting.** A missing file is a first run, and
  a file that is unreadable, larger than 8 MiB, or partly garbled gives you whatever could
  be recovered — with the rest reported in `m` and in the log. Read state is a
  convenience, not something you typed. See
  [ADR-0009](adr/0009-newsrc-file-for-read-state.md).
- **Saving is atomic** — a temporary file renamed over the old one — so a crash or a full
  disk leaves either the old state or the new, never half a file. It is written when the
  reader exits.

Two instances of the reader against the same server will have the last one to exit win.

### What it does not do yet

Posting, threading and a disk cache are all v0.2 or later; see the
[roadmap](https://github.com/edusouza/rust-nntp/issues/3). There is no subscription list
yet, so the group list shows everything the server carries, and it is re-fetched on every
start.
