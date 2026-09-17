# Validating against a real news server

Every automated test in this workspace runs against
[`nntp-testserver`](../crates/nntp-testserver), which implements *our reading* of the RFCs.
A misunderstanding shared by the client and the fake server is invisible to all of them.

Closing that gap needs a real server, real credentials and a network — none of which the
project's CI has. So it is a deliberate, opt-in, run-it-yourself step, and this is the
runbook. It was the outstanding item in
[issue #4](https://github.com/edusouza/rust-nntp/issues/4), closed by the run recorded in
[the coverage matrix](protocol-coverage.md#verified-against-a-real-server) — re-run it
whenever the protocol layer changes, and add your results there.

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
  --group misc.test
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
  --group misc.test
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

Re-run it after any change to the protocol layer, and after a dependency bump that touches
decoding: `base64` and `encoding_rs` both sit in the path that turns a real RFC 2047 subject
into text, and this server's `LIST NEWSGROUPS` alone carries around 1 900 non-ASCII
descriptions. That is a better test of a decoder than any fixture in this repository.

### Windows (PowerShell)

```powershell
$env:NNTP_TEST_HOST     = 'news.eternal-september.org'
$env:NNTP_TEST_SECURITY = 'tls'            # or 'starttls'
$env:NNTP_TEST_USER     = 'YOUR_USERNAME'
$env:NNTP_TEST_GROUP    = 'misc.test'
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
export NNTP_TEST_GROUP=misc.test
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
9. **real MIME articles yield something to read** — how many of a real group's articles
   are multipart, how many are `format=flowed`, and whether every multipart one produces
   either text or a named part. A multipart article that produces neither would be a blank
   pane in the reader, which is the failure MIME support exists to prevent.

Test 9 is the one worth pointing somewhere other than `misc.test`. A text-only group has
no multipart traffic to find, so the test says so instead of passing quietly:

```powershell
$env:NNTP_TEST_GROUP = 'linux.debian.user'   # or any group fed from a mailing list
cargo test -p nntp-client --test real_server -- --ignored --nocapture --test-threads=1 `
  real_mime_articles_yield_something_to_read
```

Groups fed from mailing lists carry `multipart/alternative` from people writing in mail
clients, and `format=flowed` from the same. A `comp.*` or `misc.*` group carries neither.

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

### Seeing MIME without a real server

The bundled fake server can serve the MIME traffic too, which is the fastest way to see
what the feature does — no account, no network:

```powershell
cargo run -p nntp-testserver -- --port 1119 --mime     # terminal 1
cargo run -p nntp-tui -- --host 127.0.0.1 --port 1119 --no-tls   # terminal 2
```

`news.software.readers` then holds three articles worth opening: a mail-to-news gateway
`multipart/mixed` wrapping a `multipart/alternative` plus a patch, an article in
`format=flowed` with a quoted paragraph and a signature separator, and an article that is
nothing but an attachment. What to look for:

- the gateway article shows the plain text, **not** the boundary lines, the part headers or
  the HTML copy, with the other two parts named above the body;
- the flowed article shows one paragraph rather than three short lines, the quoted
  paragraph stays separate from the reply, and `-- ` stays on its own line;
- the attachment-only article says `(no text in this article)` rather than showing nothing.

Add `--raw` to `nntp-tui article` to see what the same article looked like before any of
this — the boundaries, the part headers and the base64 are all still there, which is the
point of `--raw`.

## Step 4 — read state

The automated suite covers the protocol layer, which does not know read state exists. The
store has unit tests and an end-to-end test against the fake server, so what a real server
adds is scale — a group with six-digit article numbers and real gaps — and the one thing no
test can check: whether the marks a user sees match what they actually read.

Fifteen minutes, by hand. `nntp-tui config path` prints where the file will be:

```text
read state:    C:\Users\you\AppData\Local\nntp-tui\data\newsrc\news.eternal-september.org.newsrc
```

Then:

1. **Open a group and read three or four articles**, moving with `n`. The count beside the
   group in the left pane should drop by one each time, and the bullet should disappear
   from each subject as you leave it.
2. **Press `u`.** The articles you just read vanish from the list, and the pane subtitle
   shows both counts. Press `u` again: they come back, with the cursor still on the same
   article.
3. **Press `M`** on a read article. It comes back as unread, with its mark. Press it again
   to put it back.
4. **Quit with `q` and look at the file.** One line per group you opened, with the numbers
   you actually read — real article numbers, so six digits on Eternal September:

   ```powershell
   Get-Content "$env:LOCALAPPDATA\nntp-tui\data\newsrc\news.eternal-september.org.newsrc"
   ```

   ```sh
   cat ~/.local/share/nntp-tui/newsrc/news.eternal-september.org.newsrc
   ```

   Expect something like `misc.test: 970370-970373`. Consecutive articles must appear as
   **one range**, not four numbers. That is the whole reason for the representation, and it
   is visible right here.

5. **Start the reader again.** What you read is still read, the count is still lower, and
   `u` still hides it. That is the acceptance criterion of
   [#7](https://github.com/edusouza/rust-nntp/issues/7), with a real server behind it.
6. **Press `c` in a group you do not mind losing**, quit, and look again: the range should
   span the group's whole watermark range, not just the articles that were listed.
   Catch-up that covered only the visible list would not be catching up.
7. **Break the file on purpose.** Add a junk line and a bad range:

   ```text
   misc.test: 969063-970373
   this line is not a newsrc line
   comp.lang.c: 1-10,oops,20
   ```

   The reader must open, `misc.test` must still be caught up, `comp.lang.c` must keep
   `1-10,20`, and `m` must show one message per fault. Anything else — refusing to start,
   or starting with everything silently unread — is a bug worth an issue.
8. **Check that the numbers are per server.** With a second account anywhere, open the same
   group on both: two files, two unrelated sets of numbers. The same group on two servers
   has two unrelated numberings, and one shared file would mark articles read that were
   never opened.

What to report if something is wrong: the file's contents, the group, and what you expected
to be unread. The file holds no credentials — but it does name the groups you have been
reading, which may be more than you want in a public issue.

**Pick a group the server actually carries.** `misc.test` exists nearly everywhere and is
the default. `comp.lang.rust` does *not* exist — Rust discussion never moved to Usenet — so
using it makes every article test fail with `no such newsgroup`. If you get that, the
failure message lists groups the server does carry, and `nntp-tui groups` lists them all.

## Step 5 — what to do with a failure

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
