//! Logging setup.
//!
//! The terminal UI owns the terminal, so anything written to stdout or stderr while it is
//! running corrupts the display. Logs therefore go to a file by default when the UI runs,
//! and to stderr for the command-line subcommands, where a human is reading the output
//! directly.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use tracing_subscriber::EnvFilter;

use crate::config::APP_NAME;

/// Where log records should go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    /// To standard error, for command-line use.
    Stderr,
    /// To a file, for the terminal UI.
    File(PathBuf),
}

/// Keeps the background writer alive.
///
/// `tracing-appender` writes from a worker thread; dropping the guard flushes it. Losing
/// the last few log lines is exactly what one does not want when diagnosing a crash, so
/// the guard is returned to the caller rather than dropped on the spot.
pub struct LogGuard(
    // Held only for its `Drop`, which flushes the writer thread. Never read.
    #[allow(dead_code)] Option<tracing_appender::non_blocking::WorkerGuard>,
);

impl std::fmt::Debug for LogGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogGuard").finish_non_exhaustive()
    }
}

/// The default log file path.
///
/// # Errors
///
/// Returns an error if the platform has no state or data directory.
pub fn default_log_path() -> anyhow::Result<PathBuf> {
    let directories = directories::ProjectDirs::from("", "", APP_NAME)
        .context("this platform has no data directory")?;
    // state_dir is Linux-only; data_local_dir exists everywhere.
    let directory = directories
        .state_dir()
        .unwrap_or_else(|| directories.data_local_dir());
    Ok(directory.join("nntp-tui.log"))
}

/// Initialises logging.
///
/// `directive` is an `EnvFilter` directive such as `info` or `nntp_client=trace`; if it is
/// `None`, `RUST_LOG` is used, falling back to `warn`. `warn` rather than `info` because
/// the default for a terminal program should be silence unless something is wrong.
///
/// # Errors
///
/// Returns an error if the log file's directory cannot be created or the file cannot be
/// opened.
pub fn init(destination: &Destination, directive: Option<&str>) -> anyhow::Result<LogGuard> {
    let filter = match directive {
        Some(directive) => EnvFilter::new(directive),
        None => EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
    };

    match destination {
        Destination::Stderr => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(std::io::stderr)
                .with_target(true)
                .without_time()
                .init();
            Ok(LogGuard(None))
        }
        Destination::File(path) => {
            let file = open_log_file(path)?;
            let (writer, guard) = tracing_appender::non_blocking(file);
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(writer)
                .with_target(true)
                .with_ansi(false)
                .init();
            tracing::info!(version = env!("CARGO_PKG_VERSION"), "nntp-tui starting");
            Ok(LogGuard(Some(guard)))
        }
    }
}

fn open_log_file(path: &Path) -> anyhow::Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_path_is_available_on_this_platform() {
        let path = default_log_path().expect("log path");
        assert!(path.ends_with("nntp-tui.log"), "{}", path.display());
    }

    #[test]
    fn creates_the_log_directory_if_it_is_missing() {
        let directory = std::env::temp_dir().join("nntp-tui-log-test-dir");
        let _ = std::fs::remove_dir_all(&directory);

        let path = directory.join("nested").join("nntp-tui.log");
        let file = open_log_file(&path).expect("open");
        drop(file);

        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn appends_rather_than_truncating() {
        // A log that loses the previous run is a log that cannot explain a crash.
        let path = std::env::temp_dir().join("nntp-tui-append-test.log");
        let _ = std::fs::remove_file(&path);

        for _ in 0..2 {
            use std::io::Write as _;
            let mut file = open_log_file(&path).expect("open");
            writeln!(file, "line").expect("write");
        }

        let contents = std::fs::read_to_string(&path).expect("read");
        assert_eq!(contents.lines().count(), 2);
        let _ = std::fs::remove_file(&path);
    }
}
