# Security policy

## Threat model

An NNTP client parses data supplied by a remote server and by anyone who can post an
article to a newsgroup that server carries. `rust-nntp` therefore treats all wire data as
hostile:

- Parsers are written to return errors, never to panic. `unsafe_code` is forbidden and
  `clippy::indexing_slicing` / `unwrap_used` / `expect_used` are denied workspace-wide.
- Response lines and multi-line blocks have configurable size limits so a malicious or
  broken server cannot exhaust memory.
- Commands are validated before being written to the socket; arguments containing CR or LF
  are rejected so that untrusted input cannot inject additional NNTP commands.
- Credentials are never sent over a plaintext connection unless the user explicitly opts
  in (`allow_plaintext_auth = true`).
- Article bodies are never executed, rendered as HTML, or written outside a path the user
  chose.

## Reporting a vulnerability

Please open a [private security advisory](https://github.com/edusouza/rust-nntp/security/advisories/new)
rather than a public issue.
