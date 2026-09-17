# User guide

> The terminal UI lands in the next milestone. Everything below is implemented and
> tested; `nntp-tui --help` is the authoritative reference.

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
cargo run -p nntp-tui -- groups --host 127.0.0.1 --port 1119 --no-tls   # terminal 2
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
- **Prefer `password_command` to `password`.** A password in a configuration file is a
  password in every backup of that file. The command's first line of output is used, so
  any password manager works. A command that fails is an error, never an empty password.
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

## Key bindings

*(Written in milestone M6, with the terminal UI.)*
