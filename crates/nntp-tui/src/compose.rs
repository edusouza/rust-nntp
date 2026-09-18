//! Writing an article in the editor the user already knows.
//!
//! A terminal newsreader has no business growing a text editor, and nobody wants to learn
//! a second one to answer a post. So composing suspends the interface, hands the file to
//! `$VISUAL` or `$EDITOR`, and picks the article back up when it exits — the arrangement
//! `mutt`, `tin` and `git commit` all use, for the same reason.
//!
//! # Where the draft lives
//!
//! Not in a temporary file. The draft is written straight into the reader's own data
//! directory and **deleted only once the server has accepted the article**. Everything
//! else — a rejection, a dropped connection, a crash, a power cut mid-edit — leaves the
//! file exactly where the reader can name it. "Never lose a draft" is not a feature that
//! can be bolted on after a failure; it has to be where the file is written in the first
//! place.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result};

/// The directory drafts are written to, created if it does not exist.
///
/// # Errors
///
/// Returns an error if the platform has no data directory or the directory cannot be
/// created.
pub fn drafts_dir() -> Result<PathBuf> {
    let directories = directories::ProjectDirs::from("", "", crate::config::APP_NAME)
        .context("this platform has no data directory")?;
    let dir = directories.data_local_dir().join("drafts");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating the drafts directory {}", dir.display()))?;
    Ok(dir)
}

/// A path for a new draft, named so that two of them cannot collide.
fn draft_path() -> Result<PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    // The process id as well as the clock: a second is long enough to write two articles
    // in two readers, and losing one to the other's file name would be exactly the failure
    // this module exists to prevent.
    let name = format!("draft-{stamp}-{}.article", std::process::id());
    Ok(drafts_dir()?.join(name))
}

/// The editor to run: `$VISUAL`, then `$EDITOR`, then the platform's usual.
pub fn editor_command() -> (OsString, Vec<OsString>) {
    choose_editor(std::env::var_os("VISUAL"), std::env::var_os("EDITOR"))
}

/// The choice itself, separated from where the values come from.
///
/// Not for tidiness: `std::env::set_var` is `unsafe` in edition 2024, and this workspace
/// forbids `unsafe` everywhere including tests. A pure function is the only way this
/// decision gets tested at all — which is worth more than the convenience of reading the
/// environment inline.
///
/// The value may carry arguments — `code --wait`, `emacsclient -c` — so it is split on
/// whitespace, as every other program that reads `$EDITOR` does. A path containing a space
/// therefore has to be given some other way; the alternative, handing the value to a
/// shell, would turn an environment variable into a way to run arbitrary commands.
fn choose_editor(visual: Option<OsString>, editor: Option<OsString>) -> (OsString, Vec<OsString>) {
    let configured = visual
        .filter(|value| !value.is_empty())
        .or_else(|| editor.filter(|value| !value.is_empty()));

    let Some(configured) = configured else {
        // `notepad` exists on every Windows install; `vi` is required by POSIX.
        let fallback = if cfg!(windows) { "notepad" } else { "vi" };
        return (OsString::from(fallback), Vec::new());
    };

    let text = configured.to_string_lossy().into_owned();
    let mut parts = text.split_whitespace().map(OsString::from);
    let program = parts.next().unwrap_or_else(|| OsString::from("vi"));
    (program, parts.collect())
}

/// What came back from the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Composed {
    /// The text as the editor left it, and the file it is still in.
    Edited {
        /// The article text.
        text: String,
        /// Where it is on disk, in case it cannot be sent.
        path: PathBuf,
    },
    /// The user left the template untouched, which is how you abandon a post.
    Abandoned,
}

/// Writes the template, runs the editor on it, and reads back what was written.
///
/// The interface must already have released the terminal: the editor needs it, and two
/// programs drawing on one terminal produce a mess neither can clean up.
///
/// # Errors
///
/// Returns an error if the draft cannot be written or read back, or if the editor cannot
/// be started or exits with a failure status. The draft file survives all of those.
pub fn compose(template: &str) -> Result<Composed> {
    let path = draft_path()?;
    std::fs::write(&path, template)
        .with_context(|| format!("writing the draft to {}", path.display()))?;

    let (program, args) = editor_command();
    let status = Command::new(&program)
        .args(&args)
        .arg(&path)
        .status()
        .with_context(|| {
            format!(
                "running {} — set $EDITOR to something that exists",
                program.to_string_lossy()
            )
        })?;

    if !status.success() {
        anyhow::bail!(
            "{} exited with {status}; the draft is at {}",
            program.to_string_lossy(),
            path.display()
        );
    }

    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading the draft back from {}", path.display()))?;

    if text.trim() == template.trim() {
        // Quitting without saving is the conventional way to say "never mind", and it must
        // not post the template. The file goes too: an abandoned post is not a draft.
        let _ = std::fs::remove_file(&path);
        return Ok(Composed::Abandoned);
    }

    Ok(Composed::Edited { text, path })
}

/// Deletes a draft that is no longer needed.
///
/// Only ever called once the server has accepted the article. A failure is not worth
/// reporting to the user — the article is posted, which is what they asked for — but it is
/// worth a log line, since a drafts directory that fills up is a puzzle later.
pub fn discard(path: &Path) {
    if let Err(error) = std::fs::remove_file(path) {
        tracing::warn!(path = %path.display(), %error, "could not remove the posted draft");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(text: &str) -> Option<OsString> {
        Some(OsString::from(text))
    }

    #[test]
    fn falls_back_to_the_platform_editor_when_nothing_is_set() {
        let (program, args) = choose_editor(None, None);
        assert!(program == "vi" || program == "notepad", "{program:?}");
        assert!(args.is_empty());
    }

    #[test]
    fn visual_wins_over_editor() {
        let (program, _) = choose_editor(value("preferred"), value("other"));
        assert_eq!(program, "preferred");
    }

    #[test]
    fn arguments_in_the_variable_are_passed_through() {
        // `code --wait` and `emacsclient -c` are how people actually set this.
        let (program, args) = choose_editor(None, value("code --wait --new-window"));
        assert_eq!(program, "code");
        assert_eq!(
            args,
            vec![OsString::from("--wait"), OsString::from("--new-window")]
        );
    }

    #[test]
    fn an_empty_variable_is_not_a_choice_of_editor() {
        // `EDITOR=` in a shell profile is a common way to *unset* it, and treating it as
        // the name of an editor produces "could not run ''".
        let (program, _) = choose_editor(value(""), value("my-editor"));
        assert_eq!(program, "my-editor");

        let (program, _) = choose_editor(value(""), value(""));
        assert!(program == "vi" || program == "notepad", "{program:?}");
    }
}
