# ADR-0007: Use rustls with webpki-roots for TLS

- **Status**: Accepted
- **Date**: 2026-09-17

## Context

News servers are commonly reached over implicit TLS on port 563 (RFC 4642 also defines
`STARTTLS` on 119). A client that sends `AUTHINFO PASS` in the clear leaks a password to
the network, so TLS is not optional for any server that requires authentication.

The two realistic choices in Rust are `native-tls`/`openssl` and `rustls`.

## Decision

`rustls` 0.23 with the `ring` crypto provider and `webpki-roots` as the trust anchor set,
behind a default-on `tls` feature in `nntp-client`.

Certificate verification is on and cannot be disabled through the configuration file.
A `--danger-accept-invalid-certs` style escape hatch is deliberately *not* provided; users
with a private CA point the config at a PEM bundle (`extra_ca_file`) instead.

## Consequences

### Positive

- No C toolchain and no system OpenSSL, so `cargo build` works identically on the three CI
  platforms and cross-compiles cleanly.
- Trust anchors are the same everywhere, which makes "it works on my machine" reports
  easier to interpret.
- The `tls` feature can be switched off for an audit build with a minimal dependency tree.

### Negative / accepted costs

- `webpki-roots` ships Mozilla's root store inside the binary, so it ignores the operating
  system's trust store: certificates trusted only by a corporate OS-level CA will be
  rejected until the user supplies `extra_ca_file`. Switching to
  `rustls-platform-verifier` later is an option and is tracked as an issue.
- Root updates arrive via a dependency bump rather than an OS update.
- `ring` rather than `aws-lc-rs`: fewer build prerequisites (no CMake/NASM), at the cost of
  a slower cipher implementation on some platforms.

## Alternatives considered

- **`native-tls`.** Uses the OS trust store, which is what a corporate user expects, but
  drags in OpenSSL on Linux and made the Windows CI job the most fragile part of the build.
- **`rustls-platform-verifier`.** Best of both, and the likely eventual answer; deferred
  because it adds platform-specific code paths that cannot be tested against a real server
  in this environment yet.
- **No TLS in v0.1.** Rejected outright: it would make the first release unusable with any
  authenticated server and would teach users to send passwords in the clear.
