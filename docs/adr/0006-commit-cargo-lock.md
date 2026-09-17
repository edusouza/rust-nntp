# ADR-0006: Commit `Cargo.lock`

- **Status**: Accepted
- **Date**: 2026-09-17

## Context

Cargo's guidance is to commit `Cargo.lock` for binaries and omit it for libraries. This
workspace is both: three libraries and one binary.

## Decision

`Cargo.lock` is committed. The published libraries are unaffected by it — consumers resolve
their own versions — while CI and anybody building `nntp-tui` get a reproducible build.

To make sure the libraries still build against current dependency versions, the dependency
bot opens weekly update pull requests, which re-resolve the lock file under CI.

## Consequences

### Positive

- Reproducible builds and bisectable CI: a failure is a code change, not a silent
  dependency upgrade.
- A supply-chain audit has an exact input to work from.

### Negative / accepted costs

- Lock file churn in the history and occasional merge conflicts (resolved by regenerating,
  never by hand-editing).
- CI does not, by itself, prove the crates still compile against the newest semver-compatible
  dependencies; the weekly bot run is what covers that.

## Alternatives considered

- **Omit the lock file.** Treats the workspace as library-first; rejected because the
  primary deliverable is an executable.
- **Commit it and also run a "latest dependencies" CI job** (`cargo update` before build).
  Worth adding once the dependency set stabilises; skipped for now to keep CI fast and
  deterministic.
