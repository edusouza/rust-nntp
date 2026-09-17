# ADR-0001: Record architecture decisions

- **Status**: Accepted
- **Date**: 2026-09-17

## Context

This project implements a protocol specified across several RFCs (3977, 4642, 4643, 2980,
5322, 2047) whose real-world implementations disagree with the specification in ways that
only become visible after weeks of use against live servers. Decisions such as "we parse
`XOVER` as well as `OVER` because pre-RFC-3977 servers only have the former" are cheap to
make and expensive to rediscover.

The project is also built in small increments with long gaps between them. Without written
decisions, each increment re-litigates the previous one.

## Decision

Every decision that constrains future work is recorded as a numbered ADR in `docs/adr/`,
in MADR style, including the alternatives that were rejected and why. ADRs are append-only:
they are superseded, never rewritten.

Decisions that do *not* need an ADR: anything reversible in a single pull request without
touching a public API or the wire format.

## Consequences

### Positive

- The rationale survives when the code changes.
- Code review can point at an ADR instead of repeating an argument.

### Negative / accepted costs

- Every non-trivial pull request has a documentation cost.

## Alternatives considered

- **Comments in the code.** Good for local invariants, bad for cross-crate decisions and
  for recording rejected options; also lost when the code is deleted.
- **Wiki / external doc.** Drifts from the tree it describes and is not reviewed with the
  change that invalidates it.
