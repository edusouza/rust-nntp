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

## Issues

Found a protocol bug, a server quirk, or a gap in RFC coverage? Open an issue rather than
fixing it silently in an unrelated pull request — the coverage matrix in
[`docs/protocol-coverage.md`](docs/protocol-coverage.md) should stay honest about what is
and is not implemented.
