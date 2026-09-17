# Validating against a real news server

Every automated test in this workspace runs against
[`nntp-testserver`](../crates/nntp-testserver), which implements *our reading* of the RFCs.
A misunderstanding shared by the client and the fake server is invisible to all of them.

Closing that gap needs a real server, real credentials and a network — none of which the
project's CI has. So it is a deliberate, opt-in, run-it-yourself step, and this is the
runbook. It is the outstanding item in
[issue #4](https://github.com/edusouza/rust-nntp/issues/4).

## What you need

- A news account. [Eternal September](https://www.eternal-september.org/) is free, carries
  the text hierarchies, and speaks TLS on 563 and `STARTTLS` on 119 — it exercises both
  transports.
- A Rust toolchain, 1.88 or newer.
- A clone of this repository.

## Step 1 — the `doctor` probe

Start here. It is one round of commands and it reports every input that changes how the
reader behaves.

The password goes in an environment variable and is read straight out of it with
`--password-env`, which has no shell in the path. `--password-command` runs through
`sh -c` or `cmd /C`, and on Windows `cmd` expands `%VAR%` during parsing and then keeps
parsing the result — so a password containing `&`, `|`, `<` or `>` gets interpreted rather
than passed on. (A POSIX shell does not re-parse an expansion, so
`sh -c 'printf %s "$VAR"'` is fine there; the hazard on Unix is only a password written
literally into the command string.)

### Windows (PowerShell)

The `Read-Host`/`Marshal` dance below works on both Windows PowerShell 5.1 — the one
Windows ships — and PowerShell 7. It keeps the password out of your command history and
out of the process argument list.

```powershell
cargo build --release -p nntp-tui

$secure = Read-Host 'password' -AsSecureString
$env:NNTP_PASSWORD = [Runtime.InteropServices.Marshal]::PtrToStringBSTR(
    [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure))

.\target\release\nntp-tui.exe doctor `
  --host news.eternal-september.org --tls `
  --username YOUR_USERNAME `
  --password-env NNTP_PASSWORD `
  --group comp.lang.rust
```

On PowerShell 7 the first two lines can be the shorter
`$env:NNTP_PASSWORD = Read-Host 'password' -MaskInput`.

The variable lives only in that PowerShell session; closing the window clears it. To clear
it sooner: `Remove-Item Env:\NNTP_PASSWORD`.

### Linux and macOS

```sh
cargo build --release -p nntp-tui

read -rs -p 'password: ' NNTP_PASSWORD; export NNTP_PASSWORD; echo

./target/release/nntp-tui doctor \
  --host news.eternal-september.org --tls \
  --username YOUR_USERNAME \
  --password-env NNTP_PASSWORD \
  --group comp.lang.rust
```

`read -rs` keeps it off the terminal and out of the shell history.

### Reading the output

| Line | What it should say | If it does not |
| --- | --- | --- |
| `encrypted` | `yes` | The `--tls` flag did not take effect. Stop and investigate before sending a password. |
| `reader mode` | `yes` | The server wants `MODE READER`; the handshake should have sent it. |
| `overview` | `OVER, including by message-id` or `OVER, by range only` | `not advertised` means the reader will fall back to `XOVER` — fine, but note it. |
| `AUTHINFO` | `USER/PASS` | `SASL only` is not supported yet; that is a known gap. |
| `auth` | `accepted` | The credentials are wrong, or `NNTP_PASSWORD` is not set in the shell that launched the program. |
| `server date` | a skew of a few seconds | A large skew breaks `NEWGROUPS`, which the disk cache will rely on. |
| `overview fmt` | begins `Subject, From, Date, Message-ID, References, bytes, lines` | A different order is handled by name-based mapping, but it is worth a fixture. |
| the group probe | records listed, an article fetched | Any `failed` line here is the interesting part. |

## Step 2 — the automated suite

This is the part worth doing, because it turns "it looked fine" into pass or fail. The
tests read the password from `NNTP_TEST_PASS` directly, so again no shell touches it.

### Windows (PowerShell)

```powershell
$env:NNTP_TEST_HOST     = 'news.eternal-september.org'
$env:NNTP_TEST_SECURITY = 'tls'            # or 'starttls'
$env:NNTP_TEST_USER     = 'YOUR_USERNAME'
$env:NNTP_TEST_GROUP    = 'comp.lang.rust'
$env:NNTP_TEST_SAMPLE   = '50'

$secure = Read-Host 'password' -AsSecureString
$env:NNTP_TEST_PASS = [Runtime.InteropServices.Marshal]::PtrToStringBSTR(
    [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure))

cargo test -p nntp-client --test real_server -- --ignored --nocapture --test-threads=1
```

To keep a copy of the output — the counts are the evidence — append
`2>&1 | Tee-Object -FilePath real-server.log` to the `cargo test` line. PowerShell's `>`
writes UTF-16 by default, which is why `Tee-Object` is the better choice here.

### Linux and macOS

```sh
export NNTP_TEST_HOST=news.eternal-september.org
export NNTP_TEST_SECURITY=tls          # or starttls
export NNTP_TEST_USER=YOUR_USERNAME
export NNTP_TEST_GROUP=comp.lang.rust
export NNTP_TEST_SAMPLE=50
read -rs -p 'password: ' NNTP_TEST_PASS; export NNTP_TEST_PASS; echo

cargo test -p nntp-client --test real_server -- \
  --ignored --nocapture --test-threads=1 2>&1 | tee real-server.log
```

### Why those flags

`--ignored` is what runs them at all; they are `#[ignore]`d so that CI stays offline.
`--nocapture` matters because the tests print the counts, and the counts are the evidence.
`--test-threads=1` matters too — a public server will refuse a handful of simultaneous
connections from one address.

Eight tests, each asserting something the offline suite cannot:

1. **the server clock parses** and is close to ours.
2. **every line of `LIST ACTIVE` parses** — tens of thousands of lines written by decades
   of different software. This is the headline test.
3. **every line of `LIST NEWSGROUPS` parses**, and reports how many descriptions are
   non-ASCII.
4. **`OVERVIEW.FMT` starts with the seven required fields.**
5. **real overview records parse completely** — no undecoded RFC 2047 subjects, no missing
   message-ids, and a report of any unparseable dates.
6. **`XOVER` agrees with `OVER`** record for record. The fallback path has to produce the
   same answers as the primary one, and nothing but a real server can check that our two
   code paths agree on real input.
7. **real articles parse**, with no unparseable header lines, and **`HEAD` agrees with
   `ARTICLE`** about the subject and message-id.
8. **fetching by message-id works with no group selected** — the path the reader uses to
   follow a `References` chain into another group.

## Step 3 — drive the reader

```powershell
# Windows
.\target\release\nntp-tui.exe config init     # prints where it wrote the file
.\target\release\nntp-tui.exe                 # opens the reader
```

```sh
# Linux and macOS
./target/release/nntp-tui config init      # prints where it wrote the file
./target/release/nntp-tui                  # opens the reader
```

In the configuration file, use `password_env = "NNTP_PASSWORD"` for the same reason as
above. Windows Terminal or any modern terminal handles the box-drawing characters and the
colours; the old `conhost` console will look rough.

Worth trying deliberately: a group with a hundred thousand articles (`comp.lang.c`), an
article with an attachment, a thread with a missing parent, a non-Latin hierarchy
(`fido7.*`, `japan.*`) to exercise charset handling.

## Step 4 — what to do with a failure

**A failure here is a finding, not a broken test.** For each one:

1. Capture the wire conversation:

   ```powershell
   # Windows
   $env:RUST_LOG = 'nntp_client=trace'
   .\target\release\nntp-tui.exe doctor `
     --host news.eternal-september.org --tls `
     --username YOUR_USERNAME --password-env NNTP_PASSWORD `
     2>&1 | Tee-Object -FilePath trace.log
   ```

   ```sh
   # Linux and macOS
   RUST_LOG=nntp_client=trace ./target/release/nntp-tui doctor \
     --host news.eternal-september.org --tls \
     --username YOUR_USERNAME --password-env NNTP_PASSWORD 2> trace.log
   ```

   Passwords are redacted at the point of encoding, so no log level reveals one. The
   **username is not** redacted, and neither are group names — glance over the log before
   sharing it.

2. Add the offending input to the corpus in
   [`crates/nntp-testserver/src/corpus.rs`](../crates/nntp-testserver/src/corpus.rs), or as
   a quirk in [`config.rs`](../crates/nntp-testserver/src/config.rs) if it is a server
   behaviour rather than a piece of data.

3. Write the regression test, then fix the client. In that order: a fix without a failing
   test first is a fix nobody can prove.

4. Update the row in [`docs/protocol-coverage.md`](protocol-coverage.md), and add a note to
   the ADR if the finding contradicts one.

That is the whole point of this exercise: turn one person's anecdote about one server into
something the offline suite checks forever.
