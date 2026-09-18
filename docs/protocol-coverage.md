# NNTP coverage matrix

What `rust-nntp` implements, per RFC. This file is part of the definition of done for any
change to command coverage: if you add a command, add its row.

Legend: ✅ implemented end to end · 🟡 grammar implemented in `nntp-proto`, not yet driven by
the client or surfaced in the UI · ⬜ not implemented · 🚫 out of scope for a reader client

A ✅ in the tables below means the offline suite covers it. What a real server confirms is
recorded separately, under [Verified against a real server](#verified-against-a-real-server)
at the end of this file: the whole suite passed against INN 2.8.0 on 2026-09-17, and the one
divergence it found is noted under `GROUP`.

## RFC 3977 — Network News Transfer Protocol

### Session administration (§5)

| Command | Status | Notes |
| --- | --- | --- |
| `CAPABILITIES` | ✅ | Issued during the handshake; a `500` is handled as "assume RFC 2980". |
| `MODE READER` | ✅ | Sent when capabilities advertise `MODE-READER` without `READER`, or when there are no capabilities at all. |
| `QUIT` | ✅ | A server that drops the socket instead of answering is not treated as an error. |

### Article posting and retrieval (§6)

| Command | Status | Notes |
| --- | --- | --- |
| `GROUP` | ✅ | Including the `low > high` spelling of an empty group. A `411` never names the group (INN answers `411 No such newsgroup`), so the requested name is always substituted rather than read from the response; the server's own text is kept, since a `411` sometimes means "access denied". |
| `LISTGROUP` | ✅ | `article_numbers` — which numbers a group *holds*, rather than the range they lie in, which is the only way to see the gaps expiry leaves. Selects the group, as the command does. |
| `LAST` / `NEXT` | ✅ | `previous_article` / `next_article` — how a group is walked when the server offers no overview at all. |
| `ARTICLE` | ✅ | |
| `HEAD` | ✅ | |
| `BODY` | ✅ | |
| `STAT` | ✅ | Article number `0` is reported as "not applicable" rather than as article zero. |
| `POST` | ✅ | The two-step exchange, dot-stuffed. The draft is validated before `POST` is sent, so a server that counts refused offers is not given one for a missing `Subject`. `440`/`441` are reported with the server's own text and leave the connection usable. |
| `IHAVE` | 🚫 | Transit command, not used by readers. |

### Information (§7)

| Command | Status | Notes |
| --- | --- | --- |
| `DATE` | ✅ | |
| `HELP` | ✅ | |
| `NEWGROUPS` | ✅ | `new_groups` — the cheap half of keeping a group list fresh. Compared against the *server's* clock, which is why `doctor` reports the skew. Does not report groups that have been removed, so a full refresh is still needed occasionally. |
| `NEWNEWS` | ⬜ | Optional and frequently disabled by servers. |
| `LIST ACTIVE` | ✅ | Streaming variant available. Note: `high` precedes `low`, unlike `GROUP`. |
| `LIST ACTIVE.TIMES` | ✅ | `group_creation_times`. Optional; a server that does not keep it answers `503`, reported as `CommandNotSupported`. |
| `LIST NEWSGROUPS` | ✅ | Tab- and space-separated descriptions both accepted. |
| `LIST OVERVIEW.FMT` | ✅ | Fetched once per session and cached; a refusal falls back to the standard layout. |
| `LIST HEADERS` | ✅ | `available_header_fields`. A server may answer `:` alone, meaning any field in the article; that is returned as it arrived rather than expanded into a list nobody can enumerate. |
| `LIST DISTRIB.PATS` | 🚫 | Posting-only. |
| `OVER` | ✅ | Streaming variant available; falls back to `XOVER` on refusal. |
| `HDR` | ✅ | `header_field` and a streaming variant. One field across a range, at a fraction of `OVER`'s bytes — what threading a whole group needs. Falls back to `XHDR`, remembering the answer. Accepts `225` or `221`: RFC 3977 §8.5.2 gives `HDR` its own code, RFC 2980 had `XHDR` share `HEAD`'s, and servers mix them. |

## RFC 2980 — Common NNTP extensions (pre-RFC-3977)

| Command | Status | Notes |
| --- | --- | --- |
| `XOVER` | ✅ | Chosen directly when capabilities omit `OVER`, or after `OVER` is refused; the choice is remembered. |
| `XHDR` | ✅ | Chosen when capabilities omit `HDR`, or after `HDR` is refused; the choice is remembered, exactly as for `XOVER`. |
| `XPAT` | ⬜ | Server-side search; useful but rarely enabled. |
| `AUTHINFO SIMPLE` | 🚫 | Obsolete and insecure. |
| `XGTITLE`, `XINDEX`, `XTHREAD` | 🚫 | Server-specific. |

## RFC 4642 — TLS

| Feature | Status | Notes |
| --- | --- | --- |
| Implicit TLS (port 563) | ✅ | rustls with the Mozilla root set; verification cannot be disabled from the configuration. |
| `STARTTLS` on port 119 | ✅ | Refused after authentication and on an already-encrypted link; the capability list read in the clear is discarded afterwards, per §2.2. Data buffered after the `382` aborts the upgrade. |

## RFC 4643 — Authentication

| Command | Status | Notes |
| --- | --- | --- |
| `AUTHINFO USER` / `PASS` | ✅ | Refused on a plaintext link unless explicitly allowed; password redacted from logs; capabilities re-read afterwards per RFC 4643 §2.1. |
| `AUTHINFO SASL` | ⬜ | Needed by a minority of commercial providers. Tracked as an issue. |

## RFC 8054 — Compression

| Command | Status | Notes |
| --- | --- | --- |
| `COMPRESS DEFLATE` | ⬜ | Large win on `LIST`/`OVER`; not needed for correctness. |

## Message format

| Feature | Status | Notes |
| --- | --- | --- |
| RFC 5322 header folding/unfolding | ✅ | Unfolded per §2.2.3; unparseable lines collected rather than dropped. |
| RFC 5322 `Date` parsing | ✅ | Plus the obsolete forms of §4.3 and the malformed ones seen in practice. |
| RFC 2047 encoded words in headers | ✅ | `B` and `Q`, adjacent-word whitespace elision, split across folds. |
| MIME multipart bodies | ✅ | Split per RFC 2046 §5.1.1. `multipart/alternative` shows the plain-text part, other types the first text part; everything else is named rather than dumped. Depth and part-count limits bound remote input. |
| `format=flowed` (RFC 3676) | ✅ | Soft breaks joined, quote depth respected, `delsp=yes` honoured, `-- ` still a hard break. |
| Signed articles | ✅ display | `multipart/signed` (RFC 3156) shows the signed part and reports the signature as a fact rather than an attachment; inline clearsign armour (RFC 4880 §7) is stripped, dash-escaping undone. **Nothing is verified** — no cryptography here, and a reader implying otherwise would be worse than one that says nothing. |
| `quoted-printable` / `base64` body decoding | ✅ | Brought forward from v0.2: unreadable bodies were too common without it. |
| Non-UTF-8 body charsets | ✅ | Declared charsets via `encoding_rs`; unlabelled 8-bit falls back to Windows-1252. |
| yEnc / uuencode attachments | 🚫 v0.1 | Binary groups are out of scope for the first release. |

## Verified against a real server

The eight `#[ignore]`d tests in
[`crates/nntp-client/tests/real_server.rs`](../crates/nntp-client/tests/real_server.rs) are
the only thing here that is not self-confirming: everything else is the client agreeing with
a fake server this project also wrote, which [ADR-0004](adr/0004-fake-server-for-tests.md)
identifies as the weak point of that design. They must be run by hand, from a machine with
outbound TCP and an account on a news server — the runbook is
[`validating-against-a-real-server.md`](validating-against-a-real-server.md).

Last run: **2026-09-17**, INN 2.8.0 (20260619 snapshot) at `news.eternal-september.org:563`,
implicit TLS, `AUTHINFO USER`/`PASS`, `NNTP_TEST_GROUP=misc.test`. **8 passed, 0 failed**,
on Windows (`x86_64-pc-windows-msvc`). Run twice that day: once before and once after the
`base64` 0.23 and `toml` 1.1 bumps, with identical counts. The second run is the one that
matters for the bump — `base64` sits in the path that decodes RFC 2047, and 1 907 of the
descriptions below are non-ASCII, so "0 undecoded subjects" on real traffic is a better
answer than any fixture could give.

| What was checked | Result |
| --- | --- |
| Greeting, `CAPABILITIES`, `MODE READER`, authentication | `VERSION IMPLEMENTATION AUTHINFO COMPRESS HDR LIST OVER POST READER XPAT`; implementation read as `INN 2.8.0 (20260619 snapshot)` |
| `LIST NEWSGROUPS` | 45 102 descriptions, **0 unparseable lines**; 1 907 of them contain non-ASCII text |
| `LIST ACTIVE` | 26 188 groups in 1.81 s, **0 unparseable lines** |
| `GROUP` | `misc.test`: ~1 242 articles, 969 063..970 373 |
| `ARTICLE <message-id>` with no group selected | fetched `<w8TqS.267351$nBp.244460@usenetxs.com>` on a fresh connection |
| `OVER` on 50 real articles | 44 records, **0 unparseable lines**; 12 replies, **0 unparseable dates, 0 missing message-ids, 0 undecoded subjects** |
| `ARTICLE` / `HEAD` on 10 real articles | headers agree between the two, **0 unparseable header lines** |
| `LIST OVERVIEW.FMT` | `Subject From Date Message-ID References bytes lines Xref:full` — the RFC 3977 §8.3 prefix exactly |
| `DATE` | `2026-09-17T16:49:27+00:00`, **0 s skew** |
| `XOVER` vs `OVER` | 19 records each, identical field by field, 0 unparseable |

### Read state, checked by hand

Read state is not in the automated suite: the protocol layer does not know it exists, and
what a real server adds is scale and the one question no test can answer — whether the
marks a user sees match what they actually read. Step 4 of the runbook is that pass, and it
was walked through against the same server on 2026-09-17:

| What was checked | Result |
| --- | --- |
| Reading articles moves the counts and the marks | unread count drops per article, the bullet clears |
| `u` hides read articles, and brings them back | works, and the cursor keeps its place |
| `M` marks a read article unread | works |
| Marks survive quitting and reopening the reader | works |
| A deliberately corrupted store | **the reader opened, the fault was reported in the message pane, and the readable groups survived** |

That last row is the acceptance criterion from [#7] that only a person can check: not
refusing to start, and not silently forgetting everything.

[#7]: https://github.com/edusouza/rust-nntp/issues/7

### Notes on the numbers

Two things are worth naming about that table. The `OVER` range asked for 50 articles and got
44: the watermarks from `GROUP` and `LIST ACTIVE` are an estimate that counts cancelled and
expired articles, which is why the group listing shows `≤n` rather than `n`. And the date,
subject and `References` columns are the ones that matter most — they are the fields where
real Usenet traffic is least like a fixture, and they came back clean across 44 records from
30 years of accumulated posting conventions.

The first run, an hour earlier, failed four of the eight. One failure was a fixture mistake
on my part (`comp.lang.rust`, which no server carries — `select_group` now lists groups the
server does carry when `GROUP` fails) and the other was real: the `411` divergence under
`GROUP` above.
