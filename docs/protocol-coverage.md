# NNTP coverage matrix

What `rust-nntp` implements, per RFC. This file is part of the definition of done for any
change to command coverage: if you add a command, add its row.

Legend: ✅ implemented end to end · 🟡 grammar implemented in `nntp-proto`, not yet driven by
the client or surfaced in the UI · ⬜ not implemented · 🚫 out of scope for a reader client

## RFC 3977 — Network News Transfer Protocol

### Session administration (§5)

| Command | Status | Notes |
| --- | --- | --- |
| `CAPABILITIES` | 🟡 | Grammar and query API in `nntp-proto`; not yet issued by a client. |
| `MODE READER` | 🟡 | Encoded; sent when capabilities advertise `MODE-READER` without `READER`. |
| `QUIT` | 🟡 | Encoded. |

### Article posting and retrieval (§6)

| Command | Status | Notes |
| --- | --- | --- |
| `GROUP` | 🟡 | Encoded; `211` response parsed, including the `low > high` spelling of an empty group. |
| `LISTGROUP` | 🟡 | Encoded, including the `group range` form. |
| `LAST` / `NEXT` | 🟡 | Encoded. |
| `ARTICLE` | 🟡 | Encoded; response split into headers and body. |
| `HEAD` | 🟡 | Encoded; response parsed as a headers-only article. |
| `BODY` | 🟡 | Encoded. |
| `STAT` | 🟡 | Encoded. |
| `POST` | 🚫 v0.1 | Planned for v0.2. |
| `IHAVE` | 🚫 | Transit command, not used by readers. |

### Information (§7)

| Command | Status | Notes |
| --- | --- | --- |
| `DATE` | 🟡 | Encoded; `111 yyyymmddhhmmss` parsed. |
| `HELP` | 🟡 | Encoded. |
| `NEWGROUPS` | 🟡 | Encoded with a four-digit year in GMT. |
| `NEWNEWS` | ⬜ | Optional and frequently disabled by servers. |
| `LIST ACTIVE` | 🟡 | Encoded; lines parsed (note: `high` precedes `low`, unlike `GROUP`). |
| `LIST ACTIVE.TIMES` | 🟡 | Encoded; creation time and creator parsed. |
| `LIST NEWSGROUPS` | 🟡 | Encoded; tab- and space-separated descriptions both accepted. |
| `LIST OVERVIEW.FMT` | 🟡 | Encoded and parsed, including `:full` fields and a leading `:number`. |
| `LIST HEADERS` | 🟡 | Encoded. |
| `LIST DISTRIB.PATS` | 🚫 | Posting-only. |
| `OVER` | 🟡 | Encoded (range, message-id and current forms); records parsed. |
| `HDR` | 🟡 | Encoded. |

## RFC 2980 — Common NNTP extensions (pre-RFC-3977)

| Command | Status | Notes |
| --- | --- | --- |
| `XOVER` | 🟡 | Encoded; same record parser as `OVER`. Fallback when `OVER` is not advertised. |
| `XHDR` | 🟡 | Encoded. |
| `XPAT` | ⬜ | Server-side search; useful but rarely enabled. |
| `AUTHINFO SIMPLE` | 🚫 | Obsolete and insecure. |
| `XGTITLE`, `XINDEX`, `XTHREAD` | 🚫 | Server-specific. |

## RFC 4642 — TLS

| Feature | Status | Notes |
| --- | --- | --- |
| Implicit TLS (port 563) | ⬜ | |
| `STARTTLS` on port 119 | 🟡 | Command encoded; the TLS upgrade itself is milestone M4. |

## RFC 4643 — Authentication

| Command | Status | Notes |
| --- | --- | --- |
| `AUTHINFO USER` / `PASS` | 🟡 | Encoded, with the password redacted from logs. Refusal on a plaintext link is milestone M4. |
| `AUTHINFO SASL` | ⬜ | Needed by a minority of commercial providers. |

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
| MIME multipart bodies | ⬜ | v0.2. |
| `quoted-printable` / `base64` body decoding | ✅ | Brought forward from v0.2: unreadable bodies were too common without it. |
| Non-UTF-8 body charsets | ✅ | Declared charsets via `encoding_rs`; unlabelled 8-bit falls back to Windows-1252. |
| yEnc / uuencode attachments | 🚫 v0.1 | Binary groups are out of scope for the first release. |
