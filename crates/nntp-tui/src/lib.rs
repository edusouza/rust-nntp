//! A terminal news reader for Usenet, and the small command-line tool that grew with it.
//!
//! Split into a library and a thin binary so that the reader can be tested without a
//! terminal: the integration tests drive [`tui::app::App`] and [`tui::worker`] against a
//! fake news server, which is the only way to cover the interface's behaviour rather than
//! just its pixels.
//!
//! # Layout
//!
//! - [`config`] — the TOML configuration file.
//! - [`cli`] — the command-line interface.
//! - [`session`] — merging configuration and flags into a connected client.
//! - [`commands`] — the command-line subcommands, including `doctor`.
//! - [`tui`] — the terminal interface: state machine, drawing and network worker.
//! - [`logging`] — where log records go, which differs between the two modes.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod cli;
pub mod commands;
pub mod config;
pub mod logging;
pub mod session;
pub mod tui;

use crate::cli::{Cli, Command};
use crate::config::Config;

/// Runs whatever the command line asked for.
///
/// # Errors
///
/// Returns an error if the configuration cannot be read, if a server cannot be
/// identified, or if the chosen command fails. Failures carry an `anyhow` context chain,
/// so printing with `{:#}` gives the whole story rather than just the last link.
pub fn run(cli: &Cli) -> anyhow::Result<()> {
    // `config` is the one subcommand that must work without a valid configuration file:
    // it is how a broken one gets diagnosed.
    if let Some(Command::Config { action }) = &cli.command {
        return commands::config(action, cli.config.as_deref());
    }

    let config = Config::load(cli.config.as_deref())?;

    match &cli.command {
        // No subcommand: open the reader against the configured server.
        None => open_reader(&config, &cli::ServerArgs::default()),
        Some(Command::Tui { server }) => open_reader(&config, server),

        Some(Command::Doctor { server, group }) => {
            commands::doctor(&config, server, group.as_deref())
        }
        Some(Command::Groups {
            server,
            pattern,
            descriptions,
            limit,
        }) => commands::groups(&config, server, pattern.as_deref(), *descriptions, *limit),
        Some(Command::Overview {
            server,
            group,
            count,
        }) => commands::overview(&config, server, group, *count),
        Some(Command::Article {
            server,
            article,
            group,
            part,
            raw,
        }) => commands::article(&config, server, article, group.as_deref(), *part, *raw),
        // Handled above, before the configuration is loaded.
        Some(Command::Config { .. }) => {
            unreachable!("handled before the configuration is loaded")
        }
    }
}

fn open_reader(config: &Config, server: &cli::ServerArgs) -> anyhow::Result<()> {
    let target = session::resolve(config, server)?;
    tracing::info!(server = %target.authority(), "opening the reader");
    tui::run(config, target)
}
