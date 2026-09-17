//! Entry point for the `nntp-tui` news reader.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

fn main() -> anyhow::Result<()> {
    println!(
        "nntp-tui {} (not implemented yet)",
        env!("CARGO_PKG_VERSION")
    );
    Ok(())
}
