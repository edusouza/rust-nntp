//! Where read state lives between runs: one `.newsrc`-format file per server.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::Context as _;

use super::ReadSet;
use crate::config::APP_NAME;

/// Largest store file that will be read, in octets.
///
/// A file holding every group a full-feed server carries, each with a handful of ranges,
/// is on the order of a megabyte; 8 MiB leaves room for a reader who has followed
/// thousands of groups for years. The limit exists because this file is parsed at startup
/// and can have been edited by hand or copied from anywhere, and a parser that will read
/// an arbitrarily large file into memory is a denial of service waiting for an accident.
pub const MAX_STORE_BYTES: u64 = 8 * 1024 * 1024;

/// Read state for every group on one server, and the file it is stored in.
///
/// # The format
///
/// The `.newsrc` format, unchanged since the 1980s and understood by `slrn`, `tin` and
/// `nn`:
///
/// ```text
/// comp.lang.c: 1-4237,4240,4242-4250
/// misc.test! 1-100
/// ```
///
/// `:` marks a subscribed group and `!` an unsubscribed one. This reader does not yet have
/// a subscription list ([#15]), but the flag is read, kept and written back, because
/// destroying information that another reader put there would make "you can copy this file
/// between readers" a lie.
///
/// # One file per server
///
/// Article numbers are assigned by the server, so the same group on two servers has two
/// unrelated numberings. A single shared file would silently mark articles read on one
/// server because they happened to be read on another — so the file is named after the
/// server it describes.
///
/// [#15]: https://github.com/edusouza/rust-nntp/issues/15
#[derive(Debug, Clone)]
pub struct ReadStore {
    path: PathBuf,
    groups: BTreeMap<String, Entry>,
    dirty: bool,
}

/// One group's line in the store.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Entry {
    read: ReadSet,
    /// `true` for `:`, `false` for `!`. Defaults to subscribed, which is what a group the
    /// user has opened effectively is.
    subscribed: bool,
}

/// Something wrong with a store file that was recovered from rather than fatal.
///
/// Read state is a convenience, not data the user typed. Refusing to start because a line
/// is malformed would be the wrong trade every time, so problems are collected and handed
/// to the caller to log, and the reader opens with whatever survived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// The file could not be read at all. Everything is unread this session.
    Unreadable {
        /// Why, as the operating system put it.
        reason: String,
    },
    /// The file is larger than [`MAX_STORE_BYTES`] and was not read.
    TooLarge {
        /// The file's size in octets.
        size: u64,
    },
    /// A line was not `group: ranges` or `group! ranges`.
    MalformedLine {
        /// 1-based line number, so it can be found in an editor.
        line: usize,
    },
    /// A group's line parsed, but part of its range list did not.
    MalformedRanges {
        /// The group whose line it was.
        group: String,
        /// The pieces that did not parse.
        pieces: Vec<String>,
    },
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable { reason } => {
                write!(
                    f,
                    "read state could not be read ({reason}); treating everything as unread"
                )
            }
            Self::TooLarge { size } => write!(
                f,
                "read state is {size} octets, over the {MAX_STORE_BYTES} octet limit; treating everything as unread"
            ),
            Self::MalformedLine { line } => {
                write!(f, "read state line {line} is not `group: ranges`; skipped")
            }
            Self::MalformedRanges { group, pieces } => write!(
                f,
                "read state for {group} has {} unreadable range(s) ({}); the rest was kept",
                pieces.len(),
                pieces.join(", ")
            ),
        }
    }
}

impl ReadStore {
    /// An empty store that will be written to `path`.
    pub fn empty(path: PathBuf) -> Self {
        Self {
            path,
            groups: BTreeMap::new(),
            dirty: false,
        }
    }

    /// The default store path for `server`, under the platform data directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the platform has no data directory.
    pub fn default_path(server: &str) -> anyhow::Result<PathBuf> {
        let directories = directories::ProjectDirs::from("", "", APP_NAME)
            .context("this platform has no data directory")?;
        Ok(directories
            .data_local_dir()
            .join("newsrc")
            .join(file_name_for(server)))
    }

    /// Loads the store at `path`, recovering from anything wrong with it.
    ///
    /// Never fails. A missing file is an empty store — the first run of a new reader is
    /// not an error — and a file that cannot be read, is too large, or is partly garbage
    /// yields whatever could be recovered plus a [`Problem`] for each thing that was
    /// wrong. The acceptance criterion from [#7] is exactly this: a corrupt store degrades
    /// to "everything unread" rather than refusing to start.
    ///
    /// [#7]: https://github.com/edusouza/rust-nntp/issues/7
    pub fn load(path: PathBuf) -> (Self, Vec<Problem>) {
        let mut problems = Vec::new();

        match fs::metadata(&path) {
            Ok(metadata) if metadata.len() > MAX_STORE_BYTES => {
                problems.push(Problem::TooLarge {
                    size: metadata.len(),
                });
                return (Self::empty(path), problems);
            }
            // A missing file is the normal first run, and is not worth a problem.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return (Self::empty(path), problems);
            }
            Err(error) => {
                problems.push(Problem::Unreadable {
                    reason: error.to_string(),
                });
                return (Self::empty(path), problems);
            }
            Ok(_) => {}
        }

        let text = match fs::read(&path) {
            Ok(bytes) => {
                // Group names are ASCII (RFC 3977 §4.1) and so are the ranges, but the
                // file may have been touched by an editor that left something else in it.
                // Lossy conversion keeps the readable lines; a malformed group name will
                // simply not match any group the server offers.
                String::from_utf8_lossy(&bytes).into_owned()
            }
            Err(error) => {
                problems.push(Problem::Unreadable {
                    reason: error.to_string(),
                });
                return (Self::empty(path), problems);
            }
        };

        let mut store = Self::empty(path);
        for (index, line) in text.lines().enumerate() {
            let line = line.trim();
            // Blank lines, and comments, which `.newsrc` does not define but hand-editors
            // write anyway.
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let Some((group, subscribed, ranges)) = split_line(line) else {
                problems.push(Problem::MalformedLine { line: index + 1 });
                continue;
            };

            let (read, malformed) = ReadSet::parse(ranges);
            if !malformed.is_empty() {
                problems.push(Problem::MalformedRanges {
                    group: group.to_owned(),
                    pieces: malformed,
                });
            }

            // A group repeated in the file is merged rather than letting the last line
            // win: both lines were written by something that believed them.
            let entry = store.groups.entry(group.to_owned()).or_default();
            for &(low, high) in read.ranges() {
                entry.read.insert_range(low, high);
            }
            entry.subscribed = entry.subscribed || subscribed;
        }

        (store, problems)
    }

    /// The path this store reads from and writes to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How many groups have state recorded.
    pub fn group_count(&self) -> usize {
        self.groups.len()
    }

    /// Whether anything has changed since the last [`Self::save`].
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// The read set for `group`, or an empty one if the group is unknown.
    ///
    /// Returning an empty set rather than `None` is deliberate: "no record" and "nothing
    /// read" are the same thing to every caller, and making them distinguish the two would
    /// only invite one of them to get it wrong.
    pub fn read_set(&self, group: &str) -> &ReadSet {
        static EMPTY: ReadSet = ReadSet::new();
        self.groups.get(group).map_or(&EMPTY, |entry| &entry.read)
    }

    /// Marks one article read.
    pub fn mark_read(&mut self, group: &str, number: u64) {
        self.entry(group).read.insert(number);
        self.dirty = true;
    }

    /// Marks a range of article numbers read, as "catch up" does.
    pub fn mark_range_read(&mut self, group: &str, low: u64, high: u64) {
        self.entry(group).read.insert_range(low, high);
        self.dirty = true;
    }

    /// Marks one article unread.
    pub fn mark_unread(&mut self, group: &str, number: u64) {
        self.entry(group).read.remove(number);
        self.dirty = true;
    }

    /// Replaces a group's read set wholesale.
    pub fn set_read(&mut self, group: &str, read: ReadSet) {
        self.entry(group).read = read;
        self.dirty = true;
    }

    /// Drops article numbers below `low`, which the server no longer carries.
    ///
    /// Expiry means the numbers below a group's low watermark can never come back, so
    /// keeping them makes the file grow forever with state about articles that no longer
    /// exist. Called when a group is selected and its watermarks are known.
    pub fn forget_expired(&mut self, group: &str, low: u64) {
        if low <= 1 {
            return;
        }
        let Some(entry) = self.groups.get_mut(group) else {
            return;
        };

        let before = entry.read.range_count();
        let read_before = entry.read.count();
        entry.read.remove_range(1, low - 1);
        if entry.read.range_count() != before || entry.read.count() != read_before {
            self.dirty = true;
        }
    }

    fn entry(&mut self, group: &str) -> &mut Entry {
        self.groups
            .entry(group.to_owned())
            .or_insert_with(|| Entry {
                read: ReadSet::new(),
                subscribed: true,
            })
    }

    /// Renders the store in `.newsrc` syntax.
    pub fn to_newsrc(&self) -> String {
        let mut out = String::new();
        for (group, entry) in &self.groups {
            let separator = if entry.subscribed { ':' } else { '!' };
            // Groups with nothing read are still written when they carry a subscription
            // flag another reader may care about, but a subscribed group with an empty
            // read set says nothing, so it is left out rather than growing the file.
            if entry.read.is_empty() && entry.subscribed {
                continue;
            }
            // `writeln!` to a String cannot fail; the result is discarded rather than
            // unwrapped because `unwrap` is denied in this workspace.
            let _ = writeln!(out, "{group}{separator} {}", entry.read);
        }
        out
    }

    /// Writes the store, atomically, creating the parent directory if needed.
    ///
    /// The file is written to a temporary name in the same directory and renamed over the
    /// old one, so a crash or a full disk leaves either the old state or the new, never a
    /// half-written file. `fs::rename` replaces the destination on both POSIX and Windows
    /// — the Windows implementation passes `MOVEFILE_REPLACE_EXISTING` — and the temporary
    /// file is in the same directory to keep the rename on one filesystem.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created, or the file cannot be written
    /// or renamed.
    pub fn save(&mut self) -> anyhow::Result<()> {
        let parent = self
            .path
            .parent()
            .context("read state path has no parent directory")?;
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;

        let temporary = self.path.with_extension("newsrc.tmp");
        fs::write(&temporary, self.to_newsrc())
            .with_context(|| format!("writing {}", temporary.display()))?;
        fs::rename(&temporary, &self.path).with_context(|| {
            format!(
                "replacing {} with {}",
                self.path.display(),
                temporary.display()
            )
        })?;

        self.dirty = false;
        Ok(())
    }

    /// Writes the store only if something has changed.
    ///
    /// # Errors
    ///
    /// Returns an error if the store is dirty and cannot be written.
    pub fn save_if_dirty(&mut self) -> anyhow::Result<()> {
        if self.dirty { self.save() } else { Ok(()) }
    }
}

/// Splits a `.newsrc` line into group, subscription flag and range list.
fn split_line(line: &str) -> Option<(&str, bool, &str)> {
    // The separator is the first `:` or `!`. A group name contains neither (RFC 3977
    // §4.1), so the first occurrence is unambiguous.
    let position = line.find([':', '!'])?;
    let (group, rest) = line.split_at(position);
    let group = group.trim();
    if group.is_empty() {
        return None;
    }

    let mut characters = rest.chars();
    let subscribed = match characters.next() {
        Some(':') => true,
        Some('!') => false,
        _ => return None,
    };

    Some((group, subscribed, characters.as_str()))
}

/// A file name that stands for `server`, safe on every platform this runs on.
///
/// Host names are already restricted, but the value here comes from a configuration file
/// or a command line and may be anything at all — including `..`, a path separator, or a
/// Windows reserved device name. Everything outside a conservative set is replaced with
/// `_`, which can collide in principle; the collision costs two servers a shared read
/// state, which is why the full host name is kept rather than hashed into something
/// unreadable.
fn file_name_for(server: &str) -> String {
    let mut name: String = server
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|character| match character {
            'a'..='z' | '0'..='9' | '.' | '-' | '_' => character,
            _ => '_',
        })
        .collect();

    // `.`, `..` and the empty string are not names; a leading dot would also hide the
    // file for no reason the user asked for.
    while name.starts_with('.') {
        name.remove(0);
    }
    if name.is_empty() {
        name.push_str("unknown-server");
    }

    name.push_str(".newsrc");
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_directory(label: &str) -> PathBuf {
        // A unique directory per test, without a dependency: the process id plus the test
        // name is enough, and the tests clean up after themselves.
        let directory = std::env::temp_dir().join(format!(
            "nntp-tui-readstate-{}-{}",
            std::process::id(),
            label
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("create temporary directory");
        directory
    }

    fn write(path: &Path, contents: &str) {
        fs::write(path, contents).expect("write fixture");
    }

    #[test]
    fn a_missing_file_is_an_empty_store_and_not_a_problem() {
        let directory = temporary_directory("missing");
        let (store, problems) = ReadStore::load(directory.join("nowhere.newsrc"));

        assert_eq!(store.group_count(), 0);
        assert!(problems.is_empty(), "{problems:?}");
        assert!(!store.is_dirty());
        assert!(store.read_set("misc.test").is_empty());

        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn reads_the_newsrc_format_including_the_subscription_flag() {
        let directory = temporary_directory("read");
        let path = directory.join("server.newsrc");
        write(
            &path,
            "comp.lang.c: 1-4237,4240,4242-4250\nmisc.test! 1-100\n",
        );

        let (store, problems) = ReadStore::load(path);

        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(store.group_count(), 2);
        assert!(store.read_set("comp.lang.c").contains(4_240));
        assert!(!store.read_set("comp.lang.c").contains(4_241));
        assert_eq!(store.read_set("misc.test").count(), 100);

        // The unsubscribed flag survives a round trip, even though this reader has no
        // subscription list yet.
        let rendered = store.to_newsrc();
        assert!(rendered.contains("misc.test! 1-100"), "{rendered}");

        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_corrupt_file_degrades_to_what_survived() {
        let directory = temporary_directory("corrupt");
        let path = directory.join("server.newsrc");
        write(
            &path,
            "# hand-written comment\n\
             comp.lang.c: 1-10,oops,20\n\
             this line has no separator at all\n\
             misc.test: 1-5\n",
        );

        let (store, problems) = ReadStore::load(path);

        // The point of the test: the readable groups are still there.
        assert_eq!(store.read_set("comp.lang.c").to_string(), "1-10,20");
        assert_eq!(store.read_set("misc.test").to_string(), "1-5");

        // And the two problems are reported rather than swallowed, with the line number
        // for the one that can be found in an editor.
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems.iter().any(|problem| matches!(
            problem,
            Problem::MalformedRanges { group, pieces }
                if group == "comp.lang.c" && pieces == &["oops".to_owned()]
        )));
        assert!(
            problems
                .iter()
                .any(|problem| matches!(problem, Problem::MalformedLine { line: 3 }))
        );

        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_file_over_the_size_limit_is_not_read() {
        let directory = temporary_directory("huge");
        let path = directory.join("server.newsrc");
        let mut contents = String::from("misc.test: 1-100\n");
        while contents.len() as u64 <= MAX_STORE_BYTES {
            contents.push_str("filler.group: 1-1000000\n");
        }
        write(&path, &contents);

        let (store, problems) = ReadStore::load(path);

        assert_eq!(store.group_count(), 0);
        assert!(
            problems
                .iter()
                .any(|problem| matches!(problem, Problem::TooLarge { .. })),
            "{problems:?}"
        );

        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_group_repeated_in_the_file_is_merged_rather_than_overwritten() {
        let directory = temporary_directory("repeated");
        let path = directory.join("server.newsrc");
        write(&path, "misc.test: 1-10\nmisc.test: 20-30\n");

        let (store, _) = ReadStore::load(path);

        assert_eq!(store.read_set("misc.test").to_string(), "1-10,20-30");

        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn state_survives_a_save_and_a_reload() {
        let directory = temporary_directory("roundtrip");
        let path = directory.join("server.newsrc");

        let mut store = ReadStore::empty(path.clone());
        store.mark_range_read("misc.test", 1, 100);
        store.mark_read("misc.test", 105);
        store.mark_unread("misc.test", 50);
        assert!(store.is_dirty());

        store.save().expect("save");
        assert!(!store.is_dirty(), "saving clears the dirty flag");

        let (reloaded, problems) = ReadStore::load(path);

        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(
            reloaded.read_set("misc.test").to_string(),
            "1-49,51-100,105"
        );

        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn saving_twice_replaces_the_file_rather_than_failing() {
        // `fs::rename` over an existing file behaves differently across platforms unless
        // the implementation asks for replacement; on Windows it needs
        // MOVEFILE_REPLACE_EXISTING. Rust's std does ask, and this test is what proves it
        // on the CI matrix rather than on the word of the documentation.
        let directory = temporary_directory("replace");
        let path = directory.join("server.newsrc");

        let mut store = ReadStore::empty(path.clone());
        store.mark_range_read("misc.test", 1, 10);
        store.save().expect("first save");

        store.mark_range_read("misc.test", 11, 20);
        store.save().expect("second save over an existing file");

        let (reloaded, _) = ReadStore::load(path.clone());
        assert_eq!(reloaded.read_set("misc.test").to_string(), "1-20");

        // The temporary file is not left behind.
        let leftovers: Vec<_> = fs::read_dir(&directory)
            .expect("list directory")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "left behind {leftovers:?}");

        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn saving_creates_the_directory_it_needs() {
        let directory = temporary_directory("mkdir");
        let path = directory
            .join("nested")
            .join("deeper")
            .join("server.newsrc");

        let mut store = ReadStore::empty(path.clone());
        store.mark_read("misc.test", 1);
        store
            .save()
            .expect("save into a directory that does not exist yet");

        assert!(path.exists());

        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn nothing_is_written_when_nothing_changed() {
        let directory = temporary_directory("clean");
        let path = directory.join("server.newsrc");

        let mut store = ReadStore::empty(path.clone());
        store.save_if_dirty().expect("no-op save");

        assert!(!path.exists(), "a clean store should not create a file");

        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_group_with_nothing_read_is_not_written() {
        let mut store = ReadStore::empty(PathBuf::from("unused"));
        store.mark_read("misc.test", 1);
        store.mark_unread("misc.test", 1);

        assert_eq!(store.to_newsrc(), "");
    }

    #[test]
    fn expired_articles_are_forgotten_when_the_watermarks_say_so() {
        let mut store = ReadStore::empty(PathBuf::from("unused"));
        store.mark_range_read("misc.test", 1, 1_000);

        // The server now starts at 900: everything below it is gone for good, and keeping
        // read state about it makes the file grow forever.
        store.forget_expired("misc.test", 900);

        assert_eq!(store.read_set("misc.test").to_string(), "900-1000");
    }

    #[test]
    fn forgetting_expired_articles_is_a_no_op_for_a_group_that_starts_at_one() {
        let mut store = ReadStore::empty(PathBuf::from("unused"));
        store.mark_range_read("misc.test", 1, 10);
        store.save_if_dirty().ok();

        let before = store.to_newsrc();
        store.forget_expired("misc.test", 1);
        store.forget_expired("unknown.group", 500);

        assert_eq!(store.to_newsrc(), before);
    }

    #[test]
    fn a_server_name_becomes_a_safe_file_name() {
        assert_eq!(
            file_name_for("news.eternal-september.org"),
            "news.eternal-september.org.newsrc"
        );
        assert_eq!(file_name_for("NEWS.EXAMPLE.ORG"), "news.example.org.newsrc");

        // The value comes from a configuration file, so it has to survive hostile input
        // without escaping the directory it belongs in. The results are ugly, which is
        // the right trade: a readable name for a sane host, and something inert for input
        // that was trying to be a path.
        assert_eq!(file_name_for("../../etc/passwd"), "_.._etc_passwd.newsrc");
        assert_eq!(file_name_for("a/b\\c"), "a_b_c.newsrc");
        assert_eq!(file_name_for(".."), "unknown-server.newsrc");
        assert_eq!(file_name_for(""), "unknown-server.newsrc");
        assert_eq!(file_name_for("[::1]"), "___1_.newsrc");

        for name in [
            "../../etc/passwd",
            "a/b\\c",
            "..",
            "",
            "c:\\windows\\system32",
        ] {
            let produced = file_name_for(name);
            assert!(
                !produced.contains(['/', '\\']) && !produced.starts_with('.'),
                "{name:?} produced {produced:?}"
            );
        }
    }

    #[test]
    fn the_problem_messages_say_what_happened_and_what_it_cost() {
        let unreadable = Problem::Unreadable {
            reason: "permission denied".to_owned(),
        }
        .to_string();
        assert!(unreadable.contains("permission denied"), "{unreadable}");
        assert!(unreadable.contains("unread"), "{unreadable}");

        let malformed = Problem::MalformedRanges {
            group: "misc.test".to_owned(),
            pieces: vec!["oops".to_owned()],
        }
        .to_string();
        assert!(malformed.contains("misc.test"), "{malformed}");
        assert!(malformed.contains("oops"), "{malformed}");
        assert!(malformed.contains("kept"), "{malformed}");
    }
}
