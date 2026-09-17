# Contributing

Thanks for considering a contribution.

## Local checks

Everything CI runs can be run locally:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

There is no network in CI and no real news server is used by the test suite: integration
tests spin up [`nntp-testserver`](crates/nntp-testserver) on `127.0.0.1:0`. Any test that
needs a real server must be `#[ignore]`d and documented.

## Lint policy

The workspace denies `unsafe_code` and warns on `clippy::unwrap_used`,
`clippy::expect_used`, `clippy::panic` and `clippy::indexing_slicing`. CI turns warnings
into errors. This is deliberate: every byte we parse comes from a remote peer, so a
parser that can panic is a denial-of-service bug.

In test code these lints are relaxed. Library and binary code must not opt out without a
comment explaining why.

## Commits

[Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/): `feat:`, `fix:`,
`docs:`, `refactor:`, `test:`, `chore:`, `ci:`, optionally scoped by crate, e.g.
`feat(proto): parse OVER responses`.

## Architecture decisions

Anything that constrains future work gets an ADR in [`docs/adr/`](docs/adr/) using the
existing numbering and template. Record the decision *and* the options that were rejected.

## The README screenshot

The screenshot in the README is generated, not pasted. If you change the reader's layout,
`cargo test -p nntp-tui --test screenshot` fails and tells you to refresh it:

```sh
UPDATE_SCREENSHOT=1 cargo test -p nntp-tui --test screenshot
```

A screenshot nobody regenerates ends up describing a program that no longer exists, which
is worse than having none.

## Cutting a release

1. Move everything under `## [Unreleased]` in `CHANGELOG.md` into a new version section,
   with the date and a short paragraph saying what the release *is* — and an honest one
   saying what it still is not.
2. Bump `workspace.package.version` in the root `Cargo.toml`, and run `cargo check` so the
   lock file follows.
3. Check that `docs/protocol-coverage.md` matches reality. It is part of the definition of
   done for a command, and it is the first thing a reader of this project will believe.
4. Run everything CI runs, on both feature configurations:
   `cargo test --workspace --all-features` and
   `cargo test --workspace --no-default-features`.
5. Merge to `main`, then tag *that* commit: `git tag -a v0.1.0 -m 'v0.1.0'`. Tag the
   default branch, not a feature branch — a tag on an unmerged commit points at history
   that a squash merge will orphan.

## Issues

Found a protocol bug, a server quirk, or a gap in RFC coverage? Open an issue rather than
fixing it silently in an unrelated pull request — the coverage matrix in
[`docs/protocol-coverage.md`](docs/protocol-coverage.md) should stay honest about what is
and is not implemented.
