# ADR-0005: TOML configuration and in-memory cache for v0.1

- **Status**: Accepted
- **Date**: 2026-09-17

## Context

A news reader accumulates state: server definitions, credentials, which articles have been
read, cached overview records and bodies. Traditional readers keep read state in `.newsrc`;
modern ones use SQLite. Deciding this up front matters because an on-disk schema is a
compatibility commitment.

## Decision

For v0.1:

- **Configuration**: a single TOML file at the platform config directory
  (`~/.config/nntp-tui/config.toml` on Linux, via the `directories` crate), deserialised
  with `serde`. Servers, connection options and UI preferences live there.
- **Cache**: in memory, per session. Group lists, overview records and fetched articles are
  kept in the running process and discarded on exit.
- **Read state**: not persisted in v0.1. The UI tracks what was opened during the session.

On-disk caching and read state are deferred, with the intended shape recorded here:
a SQLite database in the platform data directory, one table for groups, one for overview
records keyed by `(group, number)`, one for read ranges per group in the compact
`low-high,n` form `.newsrc` uses.

> **The read-state half of that plan is superseded by
> [ADR-0009](0009-newsrc-file-for-read-state.md)**, which stores read state as a
> `.newsrc`-format file per server instead of a table. The reasoning: read state is the
> one thing this program keeps that other programs already read, and it is small and
> written rarely, so the properties that make SQLite right for a cache do not apply to it.
> The cache half of this ADR stands.

## Consequences

### Positive

- v0.1 ships without a schema to migrate, and without a storage bug being able to corrupt
  anything a user cares about.
- Configuration is hand-editable and diffable, which matters for a tool whose users live in
  a terminal.

### Negative / accepted costs

- Every start re-fetches the group list, which is slow against a full-feed server
  (`LIST` can be several megabytes). The UI must therefore make that fetch interruptible
  and visibly progressive from day one.
- Read/unread state is lost between sessions, which is a real functional gap for a news
  reader. It is the single most important item in the v0.2 scope.

## Alternatives considered

- **SQLite from v0.1** (`rusqlite`). The right end state, but it front-loads schema design,
  migrations and a C dependency before a single article has been rendered, against the
  goal of shipping a working reader early.
- **Classic `.newsrc`.** Attractive for interoperability with `slrn`/`tin`, and the range
  format is worth adopting for read state. Rejected as the *primary* store because it has
  no place for overview caches and no atomic update story.
- **Credentials in the config file** — deliberately allowed but not required: the config
  accepts a `password_command` that is executed to obtain the password, so users can defer
  to a password manager instead of leaving a secret in a plaintext file.
