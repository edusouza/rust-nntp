---
title: "Roadmap: RFC 3977 news reader (v0.1.0 → v0.3.0)"
labels: [epic]
status: open
---

Tracking issue for the work planned in this repository. Each milestone is meant to be a
usable program on its own, not a step towards one big release.

## v0.1.0 — read-only reader

- [ ] M0 Bootstrap workspace, CI, documentation scaffold, ADRs
- [ ] M1 `nntp-proto`: IO-free wire grammar (responses, commands, multi-line blocks,
      `CAPABILITIES`, `LIST`, `GROUP`, `OVER`/`XOVER`, headers, RFC 2047, RFC 5322 dates)
- [ ] M2 `nntp-client`: blocking client generic over `Read + Write`, size limits, timeouts
- [ ] M3 `nntp-testserver`: offline fake server + integration tests
- [ ] M4 TLS (implicit + `STARTTLS`) and `AUTHINFO USER`/`PASS`
- [ ] M5 CLI: `doctor`, `groups`, `article`; TOML configuration; file logging
- [ ] M6 TUI: three-pane reader on ratatui, non-blocking UI
- [ ] M7 Docs, changelog, `v0.1.0` tag

## v0.2.0 — writing and durability

- [ ] Persistent read/unread state
- [ ] `POST` with an external `$EDITOR`
- [ ] MIME multipart, `quoted-printable`/`base64` bodies, non-UTF-8 charsets
- [ ] Threaded article view (`References` / `In-Reply-To`)

## v0.3.0 — scale

- [ ] On-disk cache (see [ADR-0005](../adr/0005-config-and-state-storage.md))
- [ ] `COMPRESS DEFLATE` (RFC 8054)
- [ ] Incremental group-list refresh via `NEWGROUPS`

Coverage against the RFCs is tracked in
[`docs/protocol-coverage.md`](../protocol-coverage.md).
