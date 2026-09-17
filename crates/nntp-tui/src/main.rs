//! Entry point for the `nntp-tui` news reader.
//!
//! Deliberately thin: everything of substance is in the library so that it can be tested
//! without a terminal. This file does argument parsing, logging set-up and the exit code,
//! and nothing else.

use std::process::ExitCode;

use clap::Parser as _;
use nntp_tui::cli::Cli;
use nntp_tui::logging;

fn main() -> ExitCode {
    let cli = Cli::parse();

    // Logging has to be running before anything can be diagnosed, and it must not be
    // fatal: a reader that refuses to start because it cannot open a log file is worse
    // than one that runs without logs.
    //
    // The reader owns the terminal, so its logs must not go to standard error — a single
    // log line would corrupt the display.
    let destination = match (&cli.log_file, cli.opens_the_reader()) {
        (Some(path), _) => logging::Destination::File(path.clone()),
        (None, true) => match logging::default_log_path() {
            Ok(path) => logging::Destination::File(path),
            // No data directory: better to run without logs than to refuse to start.
            Err(_) => logging::Destination::Stderr,
        },
        (None, false) => logging::Destination::Stderr,
    };

    let _guard = match logging::init(&destination, cli.log.as_deref()) {
        Ok(guard) => Some(guard),
        Err(error) => {
            eprintln!("nntp-tui: could not set up logging: {error:#}");
            None
        }
    };

    match nntp_tui::run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // `{:#}` prints the whole context chain, which is where the useful part of a
            // failure usually is: "connecting to x:563: transport error: ...".
            eprintln!("nntp-tui: {error:#}");
            tracing::error!(error = %format!("{error:#}"), "command failed");
            ExitCode::FAILURE
        }
    }
}
