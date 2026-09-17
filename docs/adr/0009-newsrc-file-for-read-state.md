# ADR-0009: Store read state in a `.newsrc`-format file, one per server

- **Status**: Accepted
- **Date**: 2026-09-17

## Context

[ADR-0005](0005-config-and-state-storage.md) deferred persistent read state out of v0.1 and
recorded an intended shape for it: a SQLite database in the platform data directory, with
"one table for read ranges per group in the compact `low-high,n` form `.newsrc` uses".

Implementing it ([#7]) forced the question of whether that is still the right store, and it
turned out that the reasoning behind it conflated two things that want different answers:

- **Read state** is small, written rarely, read once at startup, and — uniquely among
  everything this program keeps — is a format that *other programs already read*.
- **An overview and body cache** ([#14]) is large, written constantly, needs to be queried
  by key and range without loading it all, and is nobody else's business.

SQLite is a good answer to the second and a poor one to the first.

## Decision

Read state is stored as a `.newsrc`-format text file, one per server, under the platform
data directory:

```text
~/.local/share/nntp-tui/newsrc/news.example.org.newsrc
```

```text
comp.lang.c: 1-4237,4240,4242-4250
misc.test! 1-100
```

Three parts to the decision:

**The format is `.newsrc`, exactly.** `slrn`, `tin` and `nn` all read and write it. A user
migrating to this reader keeps their reading history, and a user leaving it takes theirs
with them. The `:` / `!` subscription flag is parsed, kept and written back even though
this reader has no subscription list yet ([#15]), because silently dropping a field another
reader wrote would make the interoperability claim false.

**One file per server, named after the server.** Article numbers are assigned by the
server, so the same group on two servers has two unrelated numberings. A single shared file
would mark articles read on one server because their numbers happened to be read on
another. The server name is sanitised into the file name — it arrives from a configuration
file or a command line, so it is treated as hostile input rather than as a host name.

**Writes are atomic, reads are forgiving.** Saving writes a temporary file in the same
directory and renames it over the old one, so a crash leaves either the old state or the
new. Loading never fails: a missing file is a first run, and a file that is unreadable, too
large, or partly garbage yields whatever could be recovered plus a list of problems for the
caller to log. Read state is a convenience, not data the user typed — refusing to start
over a malformed line would be the wrong trade every time.

This supersedes the read-state half of ADR-0005's intended shape. The cache half stands:
when an on-disk cache arrives, SQLite is still the answer, and read state stays in its own
file rather than moving into the database.

## Consequences

### Positive

- Interoperability with three decades of newsreaders, for free, in the one place where it
  is worth anything.
- No new dependency. SQLite would have brought a bundled C library into a workspace whose
  argument for safety is that it parses hostile input in Rust with `unsafe_code`
  forbidden — for a file of a few kilobytes read once per run.
- The store is inspectable and hand-editable with `cat` and an editor, which is the right
  property for a tool whose users live in a terminal, and it diffs.
- A corrupt store costs the user their read marks, never their ability to start the
  program.

### Negative / accepted costs

- No transactions and no concurrent access. Two instances of the reader against the same
  server will have the last one to exit win. Acceptable for now, and the file is small
  enough that a lock file is a cheap fix if it ever matters.
- The whole file is rewritten on every save. At a megabyte for a reader following thousands
  of groups, that is nothing; at a hundred megabytes it would be, and that is the point at
  which this ADR should be superseded rather than patched.
- Read state and the future cache live in two different stores, so a future "forget
  everything about this group" has to touch both.
- The size limit (8 MiB) is a policy, not a property of the format. A user with a
  genuinely larger file gets "everything unread" and a log line, which is a worse failure
  than truncation would be for them and a better one than an unbounded read for everyone
  else.

## Related

- [#7] Persist read/unread state between sessions
- [#14] On-disk cache
- [#15] Server-side group filtering and a subscription list

[#7]: https://github.com/edusouza/rust-nntp/issues/7
[#14]: https://github.com/edusouza/rust-nntp/issues/14
[#15]: https://github.com/edusouza/rust-nntp/issues/15
