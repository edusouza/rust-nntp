# User guide

> This guide is written as the features land. Until v0.1.0 is tagged, treat anything here
> as subject to change and check `nntp-tui --help` for the authoritative CLI.

## Installing

```sh
cargo install --git https://github.com/edusouza/rust-nntp nntp-tui
```

Or from a clone:

```sh
cargo build --release -p nntp-tui
./target/release/nntp-tui --help
```

## Trying it without a Usenet account

The workspace ships a fake server, so you can drive the reader offline:

```sh
cargo run -p nntp-testserver -- --port 1119      # terminal 1
cargo run -p nntp-tui -- --server 127.0.0.1:1119 --no-tls   # terminal 2
```

## Configuration

*(Written in milestone M5.)*

## Key bindings

*(Written in milestone M6.)*
