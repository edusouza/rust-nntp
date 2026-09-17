# NNTP coverage matrix

What `rust-nntp` implements, per RFC. This file is part of the definition of done for any
change to command coverage: if you add a command, add its row.

Legend: ✅ implemented end to end · 🟡 grammar implemented in `nntp-proto`, not yet driven by
the client or surfaced in the UI · ⬜ not implemented · 🚫 out of scope for a reader client

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
| `GROUP` | ✅ | Including the `low > high` spelling of an empty group, and filling in the group name when a `411` omits it. |
| `LISTGROUP` | 🟡 | Encoded and served by the test server; the client has no method for it yet. |
| `LAST` / `NEXT` | 🟡 | Encoded and served by the test server; the client has no method for it yet. |
| `ARTICLE` | ✅ | |
| `HEAD` | ✅ | |
| `BODY` | ✅ | |
| `STAT` | ✅ | Article number `0` is reported as "not applicable" rather than as article zero. |
| `POST` | 🚫 v0.1 | Planned for v0.2. |
| `IHAVE` | 🚫 | Transit command, not used by readers. |

### Information (§7)

| Command | Status | Notes |
| --- | --- | --- |
| `DATE` | ✅ | |
| `HELP` | ✅ | |
| `NEWGROUPS` | 🟡 | Encoded with a four-digit year in GMT. |
| `NEWNEWS` | ⬜ | Optional and frequently disabled by servers. |
| `LIST ACTIVE` | ✅ | Streaming variant available. Note: `high` precedes `low`, unlike `GROUP`. |
| `LIST ACTIVE.TIMES` | 🟡 | Encoded, parsed and served; the client has no method for it yet. |
| `LIST NEWSGROUPS` | ✅ | Tab- and space-separated descriptions both accepted. |
| `LIST OVERVIEW.FMT` | ✅ | Fetched once per session and cached; a refusal falls back to the standard layout. |
| `LIST HEADERS` | 🟡 | Encoded. |
| `LIST DISTRIB.PATS` | 🚫 | Posting-only. |
| `OVER` | ✅ | Streaming variant available; falls back to `XOVER` on refusal. |
| `HDR` | 🟡 | Encoded. |

## RFC 2980 — Common NNTP extensions (pre-RFC-3977)

| Command | Status | Notes |
| --- | --- | --- |
| `XOVER` | ✅ | Chosen directly when capabilities omit `OVER`, or after `OVER` is refused; the choice is remembered. |
| `XHDR` | 🟡 | Encoded. |
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
| MIME multipart bodies | ⬜ | v0.2. |
| `quoted-printable` / `base64` body decoding | ✅ | Brought forward from v0.2: unreadable bodies were too common without it. |
| Non-UTF-8 body charsets | ✅ | Declared charsets via `encoding_rs`; unlabelled 8-bit falls back to Windows-1252. |
| yEnc / uuencode attachments | 🚫 v0.1 | Binary groups are out of scope for the first release. |
