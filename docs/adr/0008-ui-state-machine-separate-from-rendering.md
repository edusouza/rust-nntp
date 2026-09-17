# ADR-0008: Keep the interface state machine separate from rendering

- **Status**: Accepted
- **Date**: 2026-09-17

## Context

Terminal user interfaces have a reputation for being untested, and the reason is
structural: when the code that decides *what* to show is interleaved with the code that
draws it, the only way to exercise a decision is to draw it, and the only way to check
what was drawn is for a human to look. So the decisions go unverified, and the bugs that
result — a cursor that runs off the end of a filtered list, a stale reply displayed under
the wrong heading, a spinner that turns forever after a failure — are exactly the ones a
user notices.

## Decision

`nntp-tui` splits the interface into three parts with one-way dependencies:

- **`tui::app`** is a state machine. It takes key events and worker events in, and
  produces state changes and `Request` values out. It has no terminal, no socket, no
  clock and no `async`. Every behavioural rule of the reader lives here.
- **`tui::ui`** draws whatever `app` holds and decides nothing. It reads state; it never
  writes any except the pane heights, which it measures.
- **`tui::worker`** owns the client and performs every blocking operation on its own
  thread, exchanging `Request` and `Event` values over channels (ADR-0003).

The crate is also built as a library with a thin binary, so integration tests can drive
`app` and `worker` directly.

This gives three layers of test, each catching what the others cannot:

| Test | What it covers | What it cannot see |
| --- | --- | --- |
| unit tests in `tui::app` | every key binding, cursor clamp, filter rule, event reaction | whether any of it is drawn |
| `TestBackend` tests in `tui::ui` | the drawn output, including tiny terminals and overlays | whether the state was right |
| `tests/reader.rs` | `app` + `worker` + a real socket + a fake server | the poll loop |

What remains untested is the twenty-line poll loop in `tui::run`, which is deliberately
too thin to hold a bug worth catching. It was also driven by hand through a pseudo-terminal
during development.

## Consequences

### Positive

- The reader's behaviour is tested as plain functions: 106 unit tests plus 13 that go over
  a real socket, all offline and all fast.
- Bugs found this way were found before a human ever saw them. Two examples from the
  initial implementation: a late overview reply for a group the user had navigated away
  from would have been displayed under the current group's heading, and a dropped
  connection during the optional group-description fetch was being swallowed, leaving the
  interface believing it was connected while every subsequent command failed for no
  visible reason.
- Rendering can be rewritten — a different layout, a different widget library — without
  touching a single behavioural rule.

### Negative / accepted costs

- More indirection than drawing straight from the data: a new feature usually touches
  `protocol`, `app` and `ui` rather than one function.
- The state machine holds presentation-shaped data (`ArticleView`, `GroupRow`) that
  duplicates some of what `nntp-proto` already models. That duplication is the price of
  `app` not depending on rendering, and of the interface being able to show a partially
  parsed article.
- `app` cannot ask a question and wait for an answer; everything is a request and a later
  event. That is more code for a simple operation, and it is also what keeps the UI
  responsive.

## Alternatives considered

- **Draw directly from the client.** Simplest, and it makes the whole interface untestable
  and the UI blocking. Rejected on both counts.
- **State machine, but tested through the terminal** (a pseudo-terminal harness asserting
  on escape sequences). Useful as a smoke test and used as one during development, but far
  too slow and brittle to carry the behavioural coverage.
- **Retained-mode widget tree with callbacks.** Familiar from GUI toolkits, and it puts
  state inside widgets, which is the entanglement this ADR exists to avoid.
