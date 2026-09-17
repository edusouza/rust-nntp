# Architecture Decision Records

Short documents recording decisions that constrain future work, in
[MADR](https://adr.github.io/madr/) style. They are append-only: a decision that turns out
to be wrong gets a new ADR that supersedes the old one, and the old one is marked
`Superseded by ADR-NNNN` rather than deleted.

| ADR | Title | Status |
| --- | --- | --- |
| [0001](0001-record-architecture-decisions.md) | Record architecture decisions | Accepted |
| [0002](0002-layered-workspace.md) | Split the client into a layered workspace | Accepted |
| [0003](0003-blocking-io-on-a-worker-thread.md) | Blocking IO on a worker thread instead of an async runtime | Accepted |
| [0004](0004-fake-server-for-tests.md) | Test against an in-repo fake NNTP server | Accepted |
| [0005](0005-config-and-state-storage.md) | TOML configuration and in-memory cache for v0.1 | Accepted |
| [0006](0006-commit-cargo-lock.md) | Commit `Cargo.lock` | Accepted |
| [0007](0007-rustls-for-tls.md) | Use rustls with webpki-roots for TLS | Accepted |
| [0008](0008-ui-state-machine-separate-from-rendering.md) | Keep the interface state machine separate from rendering | Accepted |

## Template

```markdown
# ADR-NNNN: Title

- **Status**: Proposed | Accepted | Superseded by ADR-NNNN
- **Date**: YYYY-MM-DD

## Context

## Decision

## Consequences

### Positive

### Negative / accepted costs

## Alternatives considered
```
