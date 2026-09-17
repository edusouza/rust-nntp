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

```sh
cargo build --release -p nntp-tui

# Keep the password out of your shell history and out of the argument list.
read -rs -p 'password: ' ES_PASS; export ES_PASS; echo

./target/release/nntp-tui doctor \
  --host news.eternal-september.org --tls \
  --username YOUR_USERNAME \
  --password-command 'printf %s "$ES_PASS"' \
  --group comp.lang.rust
```

On Windows PowerShell:

```powershell
$env:ES_PASS = Read-Host -AsSecureString | ConvertFrom-SecureString -AsPlainText
.\target\release\nntp-tui.exe doctor `
  --host news.eternal-september.org --tls `
  --username YOUR_USERNAME `
  --password-command 'echo %ES_PASS%' `
  --group comp.lang.rust
```

Read the output against these expectations:

| Line | What it should say | If it does not |
| --- | --- | --- |
| `encrypted` | `yes` | The `--tls` flag did not take effect. Stop and investigate before sending a password. |
| `reader mode` | `yes` | The server wants `MODE READER`; the handshake should have sent it. |
| `overview` | `OVER, including by message-id` or `OVER, by range only` | `not advertised` means the reader will fall back to `XOVER` — fine, but note it. |
| `AUTHINFO` | `USER/PASS` | `SASL only` is not supported yet; that is a known gap. |
| `auth` | `accepted` | The credentials or the `password_command` are wrong. |
| `server date` | a skew of a few seconds | A large skew breaks `NEWGROUPS`, which the disk cache will rely on. |
| `overview fmt` | begins `Subject, From, Date, Message-ID, References, bytes, lines` | A different order is handled by name-based mapping, but it is worth a fixture. |
| the group probe | records listed, an article fetched | Any `failed` line here is the interesting part. |

## Step 2 — the automated suite

This is the part worth doing, because it turns "it looked fine" into pass or fail:

```sh
export NNTP_TEST_HOST=news.eternal-september.org
export NNTP_TEST_SECURITY=tls          # or starttls
export NNTP_TEST_USER=YOUR_USERNAME
read -rs -p 'password: ' NNTP_TEST_PASS; export NNTP_TEST_PASS; echo
export NNTP_TEST_GROUP=comp.lang.rust
export NNTP_TEST_SAMPLE=50

cargo test -p nntp-client --test real_server -- \
  --ignored --nocapture --test-threads=1
```

`--nocapture` matters: the tests print the counts, and the counts are the evidence.
`--test-threads=1` matters too — a public server will refuse a handful of simultaneous
connections from one address.

Eight tests, each asserting something the offline suite cannot:

1. **the server clock parses** and is close to ours.
2. **every line of `LIST ACTIVE` parses** — over a hundred thousand lines written by
   decades of different software. This is the headline test.
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

```sh
./target/release/nntp-tui config init      # then edit it with your server
./target/release/nntp-tui                  # opens the reader
```

Worth trying deliberately: a group with a hundred thousand articles (`comp.lang.c`), an
article with an attachment, a thread with a missing parent, a non-Latin hierarchy
(`fido7.*`, `japan.*`) to exercise charset handling.

## Step 4 — what to do with a failure

**A failure here is a finding, not a broken test.** For each one:

1. Capture the wire conversation:

   ```sh
   RUST_LOG=nntp_client=trace ./target/release/nntp-tui doctor \
     --host news.eternal-september.org --tls --username YOUR_USERNAME \
     --password-command 'printf %s "$ES_PASS"' 2> trace.log
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
