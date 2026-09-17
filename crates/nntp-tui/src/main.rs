//! Entry point for the `nntp-tui` news reader.
//!
//! The binary is both a terminal reader and a small command-line tool. The command-line
//! side exists for three reasons: it is useful on its own, it gives the terminal UI
//! something to be built on top of, and `doctor` is the only practical way to find out
//! what a real news server does — the test suite runs entirely against a fake one
//! (see ADR-0004).
// This is a binary crate: `pub` here means "visible to the rest of this binary", and
// there is no external API for anything to be unreachable from. Writing `pub(crate)`
// everywhere would say the same thing more noisily.
#![allow(unreachable_pub)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

mod cli;
mod commands;
mod config;
mod logging;
mod session;

use std::process::ExitCode;

use clap::Parser as _;

use crate::cli::{Cli, Command};
use crate::config::Config;

fn main() -> ExitCode {
    let cli = Cli::parse();

    // Logging has to be running before anything can be diagnosed, and it must not be
    // fatal: a reader that refuses to start because it cannot open a log file is worse
    // than one that runs without logs.
    let destination = match &cli.log_file {
        Some(path) => logging::Destination::File(path.clone()),
        None => logging::Destination::Stderr,
    };
    let _guard = match logging::init(&destination, cli.log.as_deref()) {
        Ok(guard) => Some(guard),
        Err(error) => {
            eprintln!("nntp-tui: could not set up logging: {error:#}");
            None
        }
    };

    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // `{:#}` prints the whole anyhow context chain, which is where the useful
            // part of a failure usually is: "connecting to x:563: transport error: ...".
            eprintln!("nntp-tui: {error:#}");
            tracing::error!(error = %format!("{error:#}"), "command failed");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> anyhow::Result<()> {
    // `config` is the one subcommand that must work without a valid configuration file:
    // it is how a broken one gets diagnosed.
    if let Command::Config { action } = &cli.command {
        return commands::config(action, cli.config.as_deref());
    }

    let config = Config::load(cli.config.as_deref())?;

    match &cli.command {
        Command::Doctor { server, group } => commands::doctor(&config, server, group.as_deref()),
        Command::Groups {
            server,
            pattern,
            descriptions,
            limit,
        } => commands::groups(&config, server, pattern.as_deref(), *descriptions, *limit),
        Command::Overview {
            server,
            group,
            count,
        } => commands::overview(&config, server, group, *count),
        Command::Article {
            server,
            article,
            group,
            part,
            raw,
        } => commands::article(&config, server, article, group.as_deref(), *part, *raw),
        // Handled above, before the configuration is loaded.
        Command::Config { .. } => unreachable!("handled before the configuration is loaded"),
    }
}
