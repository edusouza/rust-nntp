# NNTP coverage matrix

What `rust-nntp` implements, per RFC. This file is part of the definition of done for any
change to command coverage: if you add a command, add its row.

Legend: ✅ implemented · 🟡 parsed but not surfaced in the UI · ⬜ not implemented ·
🚫 out of scope for a reader client

## RFC 3977 — Network News Transfer Protocol

### Session administration (§5)

| Command | Status | Notes |
| --- | --- | --- |
| `CAPABILITIES` | ⬜ | |
| `MODE READER` | ⬜ | Sent when the greeting or capabilities indicate a transit server. |
| `QUIT` | ⬜ | |

### Article posting and retrieval (§6)

| Command | Status | Notes |
| --- | --- | --- |
| `GROUP` | ⬜ | |
| `LISTGROUP` | ⬜ | |
| `LAST` / `NEXT` | ⬜ | |
| `ARTICLE` | ⬜ | |
| `HEAD` | ⬜ | |
| `BODY` | ⬜ | |
| `STAT` | ⬜ | |
| `POST` | 🚫 v0.1 | Planned for v0.2. |
| `IHAVE` | 🚫 | Transit command, not used by readers. |

### Information (§7)

| Command | Status | Notes |
| --- | --- | --- |
| `DATE` | ⬜ | |
| `HELP` | ⬜ | |
| `NEWGROUPS` | ⬜ | |
| `NEWNEWS` | ⬜ | Optional and frequently disabled by servers. |
| `LIST ACTIVE` | ⬜ | |
| `LIST ACTIVE.TIMES` | ⬜ | |
| `LIST NEWSGROUPS` | ⬜ | Group descriptions. |
| `LIST OVERVIEW.FMT` | ⬜ | Required to interpret `OVER` fields beyond the first seven. |
| `LIST HEADERS` | ⬜ | |
| `LIST DISTRIB.PATS` | 🚫 | Posting-only. |
| `OVER` | ⬜ | |
| `HDR` | ⬜ | |

## RFC 2980 — Common NNTP extensions (pre-RFC-3977)

| Command | Status | Notes |
| --- | --- | --- |
| `XOVER` | ⬜ | Fallback when `OVER` is not advertised. Still the only option on some servers. |
| `XHDR` | ⬜ | Fallback for `HDR`. |
| `XPAT` | ⬜ | Server-side search; useful but rarely enabled. |
| `AUTHINFO SIMPLE` | 🚫 | Obsolete and insecure. |
| `XGTITLE`, `XINDEX`, `XTHREAD` | 🚫 | Server-specific. |

## RFC 4642 — TLS

| Feature | Status | Notes |
| --- | --- | --- |
| Implicit TLS (port 563) | ⬜ | |
| `STARTTLS` on port 119 | ⬜ | Refused after authentication, per §2.2. |

## RFC 4643 — Authentication

| Command | Status | Notes |
| --- | --- | --- |
| `AUTHINFO USER` / `PASS` | ⬜ | Refused on a plaintext link unless explicitly allowed. |
| `AUTHINFO SASL` | ⬜ | Needed by a minority of commercial providers. |

## RFC 8054 — Compression

| Command | Status | Notes |
| --- | --- | --- |
| `COMPRESS DEFLATE` | ⬜ | Large win on `LIST`/`OVER`; not needed for correctness. |

## Message format

| Feature | Status | Notes |
| --- | --- | --- |
| RFC 5322 header folding/unfolding | ⬜ | |
| RFC 5322 `Date` parsing | ⬜ | Plus the malformed forms seen in practice. |
| RFC 2047 encoded words in headers | ⬜ | `=?UTF-8?Q?...?=`; required for readable subjects. |
| MIME multipart bodies | ⬜ | v0.2. |
| `quoted-printable` / `base64` body decoding | ⬜ | v0.2. |
| Non-UTF-8 body charsets | ⬜ | Via `encoding_rs`. |
| yEnc / uuencode attachments | 🚫 v0.1 | Binary groups are out of scope for the first release. |
