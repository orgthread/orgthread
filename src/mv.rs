//! The move procedure: one file to a new name, never over another file. A
//! rename is a move in the same folder.
//!
//! The steps: check the limits and `ORGTHREAD_MOVE_ALLOW` for both names, take
//! the locks of both names in a fixed order, check the stop flag and that both
//! locks are still ours, move, compare the fingerprint at the new name with the
//! one of `--fingerprint`, move it back if they differ, write the log line,
//! release. The way back is a second move, from the new name to the old one.

use crate::copy::source_field;
use crate::storage::{CopyError, Moved, Storage, still_there};
use crate::write::{self, Gate, exit, is_under};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub const MOVE_ALLOW: &str = "ORGTHREAD_MOVE_ALLOW";

/// The folders of `allow`, comma-separated. `None` when the setting names no
/// folder: then a move has no limit beyond those of the door.
fn folders(allow: &str) -> Option<Vec<&str>> {
    let list: Vec<&str> = allow
        .split(',')
        .map(str::trim)
        .filter(|rule| !rule.is_empty())
        .collect();
    (!list.is_empty()).then_some(list)
}

/// `None` when `ORGTHREAD_MOVE_ALLOW` lets both names be moved, else the refusal.
/// Each name must lie in one of its folders, not both in the same one.
pub fn gate(from: &str, to: &str, allow: &str) -> Option<Outcome> {
    let list = folders(allow)?;
    let outside: Vec<&str> = [from, to]
        .into_iter()
        .filter(|path| !list.iter().any(|rule| is_under(path, rule)))
        .collect();
    if outside.is_empty() {
        return None;
    }
    Some(Outcome::NotAllowed {
        reason: format!(
            "{} outside the folders of {MOVE_ALLOW}. Only the owner can change that setting.",
            match outside.as_slice() {
                [one] => format!("{one} is"),
                _ => format!("{from} and {to} are"),
            }
        ),
    })
}

/// True when the file name changes and the old one is a Markdown file, so
/// wikilinks to the old name, which name the file without its folder, break.
pub fn breaks_links(from: &str, to: &str) -> bool {
    let name = |path: &str| path.rsplit('/').next().unwrap_or(path).to_string();
    from.to_lowercase().ends_with(".md") && name(from) != name(to)
}

/// True when both paths name the same file. Dropbox does not count capitals,
/// and neither does the disk of a Mac by default, so neither does this.
fn same_file(from: &str, to: &str) -> bool {
    let clean = |path: &str| path.trim_start_matches('/').to_lowercase();
    clean(from) == clean(to)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Moved {
        print: String,
        size: u64,
        /// The folders made for the new name, the outer one first.
        folders: Vec<String>,
    },
    /// A file or folder is at the new name already.
    Exists,
    /// There is no file at the old name.
    Missing,
    /// The old name is a folder, and only a file is moved.
    Folder,
    /// The file is not the version of `--fingerprint`.
    Changed {
        expected: String,
        found: String,
    },
    /// The limits of the door, or `ORGTHREAD_MOVE_ALLOW`.
    NotAllowed {
        reason: String,
    },
    /// A file that is not text, to a folder outside `ORGTHREAD_BINARY_ALLOW`.
    NotText {
        reason: String,
    },
    Locked {
        name: String,
        holder: String,
        age: Duration,
    },
    /// The file changed during the move, so it is not the version of
    /// `--fingerprint`, and it is moved back to its old name.
    MovedBack {
        expected: String,
        found: String,
    },
    /// The move ended in a state that needs a person: `reason` says where the
    /// file may be now.
    Unexpected {
        reason: String,
    },
    Failed {
        reason: String,
    },
}

impl Outcome {
    pub fn code(&self) -> i32 {
        match self {
            Outcome::Moved { .. } => exit::WRITTEN,
            Outcome::Exists => exit::EXISTS,
            Outcome::Missing => exit::MISSING,
            Outcome::Folder | Outcome::NotAllowed { .. } | Outcome::NotText { .. } => exit::USAGE,
            Outcome::Changed { .. } => exit::CHANGED,
            Outcome::Locked { .. } => exit::LOCKED,
            Outcome::MovedBack { .. } => exit::RESTORED,
            Outcome::Unexpected { .. } => exit::NOT_RESTORED,
            Outcome::Failed { .. } => exit::FAILED,
        }
    }

    /// The short word for the log. Each word of a move starts with `move`, so
    /// `watch` tells a move from a copy, which has the same 8th field.
    pub fn word(&self) -> &'static str {
        match self {
            Outcome::Moved { .. } => "moved",
            Outcome::Exists => "move-exists",
            Outcome::Missing => "move-missing",
            Outcome::Folder => "move-folder",
            Outcome::Changed { .. } => "move-changed",
            Outcome::NotAllowed { .. } => "move-not-allowed",
            Outcome::NotText { .. } => "move-not-text",
            Outcome::Locked { .. } => "move-locked",
            Outcome::MovedBack { .. } => "moved-back",
            Outcome::Unexpected { .. } => "move-unexpected",
            Outcome::Failed { .. } => "move-failed",
        }
    }
}

/// True for a log word of a move.
pub fn is_move_word(word: &str) -> bool {
    word.starts_with("move")
}

#[derive(Debug)]
pub struct Report {
    pub outcome: Outcome,
    /// The folders the move made, the outer one first, also when the file went
    /// back after a failed check: the folders stay, and the log names them.
    pub folders: Vec<String>,
    pub took_over: Option<String>,
    pub lock_left: Option<String>,
    pub lock_lost: Option<String>,
}

impl Report {
    pub fn plain(outcome: Outcome) -> Report {
        Report {
            outcome,
            folders: Vec::new(),
            took_over: None,
            lock_left: None,
            lock_lost: None,
        }
    }

    /// As for a write: a lock problem or a missing log line shows only on what
    /// would otherwise read as a clean move.
    pub fn exit_code(&self, logged: bool) -> i32 {
        let code = self.outcome.code();
        if code != exit::WRITTEN {
            code
        } else if self.lock_left.is_some() || self.lock_lost.is_some() {
            exit::LOCK_PROBLEM
        } else if !logged {
            exit::NOT_LOGGED
        } else {
            exit::WRITTEN
        }
    }
}

fn failed(step: &str, e: impl std::fmt::Display) -> Outcome {
    Outcome::Failed {
        reason: format!("{step}: {e}"),
    }
}

/// Two notes of the two locks in one, as the answer shows one.
fn both(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(a), Some(b)) => Some(format!("{a}; {b}")),
        (a, b) => a.or(b),
    }
}

/// One move. `expect` is the fingerprint the file must have, from
/// `--fingerprint`. `line` is the text of the locks: who and when. The locks are
/// the ones of both names, taken in the order of their lock names, so a write of
/// either name waits, and two moves never wait on each other in a circle.
pub fn move_file(
    storage: &dyn Storage,
    from: &str,
    to: &str,
    expect: Option<&str>,
    line: &str,
    stop: &AtomicBool,
) -> Report {
    if same_file(from, to) {
        return Report::plain(Outcome::Exists);
    }
    let (first, second) = if write::lock_name(from) <= write::lock_name(to) {
        (from, to)
    } else {
        (to, from)
    };
    let outer = write::under_lock(storage, first, line, |held_first| {
        write::under_lock(storage, second, line, |held_second| {
            // The last safe point: from here on, the move runs to its end and its check.
            if stop.load(Ordering::SeqCst) {
                return (failed("nothing moved", "a stop was asked for"), Vec::new());
            }
            for held in [held_first, held_second] {
                let refused = match held.still_ours() {
                    Ok(true) => continue,
                    Ok(false) => failed("nothing moved", "another writer came in before this move"),
                    Err(e) => failed("could not check that both names are free", e),
                };
                return (refused, Vec::new());
            }
            steps(storage, from, to, expect)
        })
    });
    let locked = |name: &str, gate: Gate| match gate {
        Gate::Busy { holder, age } => Outcome::Locked {
            name: name.to_string(),
            holder,
            age,
        },
        Gate::Failed(reason) => failed(&format!("could not reserve {name}"), reason),
    };
    match outer.result {
        Err(gate) => Report::plain(locked(first, gate)),
        Ok(inner) => {
            let (outcome, folders) = match inner.result {
                Ok(done) => done,
                Err(gate) => (locked(second, gate), Vec::new()),
            };
            Report {
                outcome,
                folders,
                took_over: both(outer.took_over, inner.took_over),
                lock_left: both(outer.lock_left, inner.lock_left),
                lock_lost: both(outer.lock_lost, inner.lock_lost),
            }
        }
    }
}

/// The outcome of a move that the storage refused or that failed.
fn refusal(e: CopyError, expect: Option<&str>) -> Outcome {
    match e {
        CopyError::Exists => Outcome::Exists,
        CopyError::Missing => Outcome::Missing,
        CopyError::Folder => Outcome::Folder,
        CopyError::Changed { found } => Outcome::Changed {
            expected: expect.unwrap_or_default().to_string(),
            found,
        },
        CopyError::Unexpected { reason } => Outcome::Unexpected { reason },
        CopyError::Io(e) => failed("could not move the file", e),
        CopyError::After { error, .. } => refusal(*error, expect),
    }
}

/// Why a move back to the old name failed, in plain words.
fn why_back(e: CopyError, from: &str, to: &str) -> String {
    match e {
        CopyError::Exists => format!("a file is at {from} again"),
        CopyError::Missing => format!("{to} is gone already"),
        CopyError::Changed { found } => format!("it changed again, to fingerprint {found}"),
        CopyError::Folder => format!("{to} is a folder now"),
        CopyError::Unexpected { reason } => reason,
        CopyError::Io(e) => e.to_string(),
        CopyError::After { error, .. } => why_back(*error, from, to),
    }
}

/// The steps under both locks: the move, and the check of `--fingerprint` at
/// the new name.
fn steps(
    storage: &dyn Storage,
    from: &str,
    to: &str,
    expect: Option<&str>,
) -> (Outcome, Vec<String>) {
    let moved = match storage.move_file(from, to, expect) {
        Ok(moved) => moved,
        // New folders made before the error stay, and the log names them.
        Err(e) => {
            let folders = still_there(storage, e.folders());
            return (refusal(e, expect), folders);
        }
    };
    let Moved {
        target,
        size,
        folders,
        ..
    } = moved;
    // The file was checked before the move. If it changed in between, it is not
    // the version the caller named, so it goes back where it was.
    if let Some(want) = expect
        && want != target
    {
        let outcome = match storage.move_file(to, from, Some(&target)) {
            Ok(_) => Outcome::MovedBack {
                expected: want.to_string(),
                found: target,
            },
            Err(e) => {
                let why = why_back(e, from, to);
                Outcome::Unexpected {
                    reason: format!(
                        "{from} changed during the move, to fingerprint {target}, not {want}, and moving it back failed: {why}. It is at {to} now"
                    ),
                }
            }
        };
        return (outcome, folders);
    }
    let made = folders.clone();
    let outcome = Outcome::Moved {
        print: target,
        size,
        folders,
    };
    (outcome, made)
}

/// The log line: time, word, new name, who, fingerprint before, after, size, and
/// the old name as the 8th field, with a `/` at its start, as a copy has it. A
/// moved file has no before at its new name. A refusal for a changed file logs
/// what was expected and what was found.
pub fn log_line(now: &str, from: &str, to: &str, who: &str, outcome: &Outcome) -> String {
    let (before, after, size) = match outcome {
        Outcome::Moved { print, size, .. } => (String::new(), print.clone(), size.to_string()),
        Outcome::Changed { expected, found } | Outcome::MovedBack { expected, found } => {
            (expected.clone(), found.clone(), String::new())
        }
        _ => (String::new(), String::new(), String::new()),
    };
    let source = source_field(from);
    crate::log::line(&[
        now,
        outcome.word(),
        to,
        who,
        &before,
        &after,
        &size,
        &source,
    ])
}

/// The answer for the person or the chat, in plain words. A move that is done
/// is one line, and one more when wikilinks to the old name break.
pub fn message(from: &str, to: &str, outcome: &Outcome) -> String {
    match outcome {
        Outcome::Moved {
            print,
            size,
            folders,
        } => {
            let mut text = format!("moved: {from} -> {to}, {size} bytes, {print}");
            if !folders.is_empty() {
                text.push_str(&format!(", new folders: {}", folders.join(", ")));
            }
            if breaks_links(from, to) {
                let old = from.rsplit('/').next().unwrap_or(from);
                let old = &old[..old.len() - 3];
                text.push_str(&format!(
                    "\nnote: the file name changed, so wikilinks to [[{old}]] break. Fix them to the new name."
                ));
            }
            text
        }
        Outcome::Exists => format!(
            "refused, nothing moved. {to} exists, and a move never replaces a file. Choose another new name."
        ),
        Outcome::Missing => format!("refused, nothing moved. There is no file {from}."),
        Outcome::Folder => {
            format!("refused, nothing moved. {from} is a folder, and only a file is moved.")
        }
        Outcome::Changed { found, .. } => format!(
            "refused, nothing moved. {from} is not the version of --fingerprint: it has {found} now. Look at it again before you move it."
        ),
        Outcome::NotAllowed { reason } => format!("refused, nothing moved. {reason}"),
        Outcome::NotText { reason } => format!("refused, nothing moved. {reason}"),
        Outcome::Locked { name, holder, age } => format!(
            "refused, nothing moved. {name} is busy since {} s with another writer: {holder}. Try again in a moment.",
            age.as_secs()
        ),
        Outcome::MovedBack { expected, found } => format!(
            "the check after the move failed: {from} changed during the move, to fingerprint {found}, not {expected}. It is moved back to {from}, and {to} is not there."
        ),
        Outcome::Unexpected { reason } => {
            format!(
                "DAMAGE POSSIBLE. The move ended in an unexpected state: {reason}. Look at {from} and {to} now."
            )
        }
        Outcome::Failed { reason } => format!("failed: {reason}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::fingerprint;
    use crate::storage::{Copied, Expect, LocalFolder};
    use std::cell::RefCell;
    use std::fs;
    use std::io;
    use std::path::PathBuf;

    fn place(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("orgthread-mv-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("root/notes")).unwrap();
        fs::create_dir_all(dir.join("root/.locks")).unwrap();
        dir
    }

    fn go() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn a_local_move_makes_and_names_the_folders_and_leaves_no_lock() {
        let dir = place("local");
        let root = dir.join("root");
        fs::write(root.join("notes/a.md"), "Grüße\n").unwrap();
        let storage = LocalFolder::new(&root, ".locks");
        let report = move_file(&storage, "notes/a.md", "new/deep/b.md", None, "t", &go());
        assert_eq!(
            report.outcome,
            Outcome::Moved {
                print: fingerprint("Grüße\n".as_bytes()),
                size: 8,
                folders: vec!["new".into(), "new/deep".into()],
            }
        );
        assert_eq!(
            fs::read(root.join("new/deep/b.md")).unwrap(),
            "Grüße\n".as_bytes()
        );
        assert!(!root.join("notes/a.md").exists(), "the old name is gone");
        assert_eq!(fs::read_dir(root.join(".locks")).unwrap().count(), 0);
        assert_eq!(report.exit_code(true), 0);
        assert_eq!(report.exit_code(false), exit::NOT_LOGGED);
        // A move into a folder that exists names no folder.
        let report = move_file(&storage, "new/deep/b.md", "notes/c.md", None, "t", &go());
        assert!(
            matches!(&report.outcome, Outcome::Moved { folders, .. } if folders.is_empty()),
            "{report:?}"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_local_move_never_replaces_a_file_or_a_folder() {
        let dir = place("exists");
        let root = dir.join("root");
        fs::write(root.join("notes/a.md"), "new\n").unwrap();
        fs::write(root.join("notes/b.md"), "old\n").unwrap();
        fs::create_dir_all(root.join("notes/sub")).unwrap();
        let storage = LocalFolder::new(&root, ".locks");
        for to in ["notes/b.md", "notes/sub", "notes/a.md", "Notes/A.md"] {
            let report = move_file(&storage, "notes/a.md", to, None, "t", &go());
            assert_eq!(report.outcome, Outcome::Exists, "{to}");
            assert_eq!(report.exit_code(true), exit::EXISTS);
        }
        assert_eq!(fs::read(root.join("notes/b.md")).unwrap(), b"old\n");
        assert_eq!(fs::read(root.join("notes/a.md")).unwrap(), b"new\n");
        assert_eq!(fs::read_dir(root.join(".locks")).unwrap().count(), 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_file_a_folder_and_another_version_move_nothing() {
        let dir = place("refused");
        let root = dir.join("root");
        fs::write(root.join("notes/a.md"), "now\n").unwrap();
        let storage = LocalFolder::new(&root, ".locks");
        let report = move_file(&storage, "notes/gone.md", "x/b.md", None, "t", &go());
        assert_eq!(report.outcome, Outcome::Missing);
        let report = move_file(&storage, "notes", "x/b.md", None, "t", &go());
        assert_eq!(report.outcome, Outcome::Folder);
        assert_eq!(report.exit_code(true), exit::USAGE);
        let seen = fingerprint(b"seen\n");
        let report = move_file(&storage, "notes/a.md", "x/b.md", Some(&seen), "t", &go());
        assert_eq!(
            report.outcome,
            Outcome::Changed {
                expected: seen,
                found: fingerprint(b"now\n")
            }
        );
        assert_eq!(report.exit_code(true), exit::CHANGED);
        assert!(!root.join("x").exists(), "a refusal makes no folder");
        assert!(root.join("notes/a.md").exists());
        let right = fingerprint(b"now\n");
        let report = move_file(&storage, "notes/a.md", "x/b.md", Some(&right), "t", &go());
        assert_eq!(report.outcome.code(), 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_stop_or_a_busy_lock_of_either_name_moves_nothing() {
        let dir = place("stop");
        let root = dir.join("root");
        fs::write(root.join("notes/a.md"), "text\n").unwrap();
        let storage = LocalFolder::new(&root, ".locks");
        let report = move_file(
            &storage,
            "notes/a.md",
            "notes/b.md",
            None,
            "t",
            &AtomicBool::new(true),
        );
        assert_eq!(report.outcome.code(), exit::FAILED);
        for name in ["notes/a.md", "notes/b.md"] {
            storage
                .lock_create(&write::lock_name(name), "a writer")
                .unwrap();
            let report = move_file(&storage, "notes/a.md", "notes/b.md", None, "t", &go());
            assert!(
                matches!(&report.outcome, Outcome::Locked { name: busy, .. } if busy == name),
                "{report:?}"
            );
            assert_eq!(report.exit_code(true), exit::LOCKED);
            storage.lock_remove(&write::lock_name(name)).unwrap();
        }
        assert!(root.join("notes/a.md").exists());
        assert!(!root.join("notes/b.md").exists());
        assert_eq!(
            fs::read_dir(root.join(".locks")).unwrap().count(),
            0,
            "the lock taken first is released when the second is busy"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    /// A storage whose first move finds the file changed in between, as a write
    /// that lands during the move would, and that notes each move.
    struct Racing {
        moves: RefCell<Vec<(String, String, Option<String>)>>,
        back_fails: bool,
    }

    impl Storage for Racing {
        fn read(&self, _: &str) -> io::Result<Option<Vec<u8>>> {
            Ok(None)
        }
        fn write_if_unchanged(
            &self,
            _: &str,
            _: &Expect,
            _: &[u8],
        ) -> Result<(), crate::storage::WriteError> {
            unreachable!()
        }
        fn restore(&self, _: &str, _: Option<&[u8]>) -> io::Result<()> {
            unreachable!()
        }
        fn rescue(&self, _: &str, _: &[u8]) -> io::Result<String> {
            unreachable!()
        }
        fn lock_create(&self, _: &str, _: &str) -> io::Result<bool> {
            Ok(true)
        }
        fn lock_read(&self, _: &str) -> io::Result<Option<String>> {
            Ok(Some("t".into()))
        }
        fn lock_age(&self, _: &str) -> io::Result<Option<Duration>> {
            Ok(None)
        }
        fn lock_take_stale(&self, _: &str, _: Duration) -> io::Result<Option<String>> {
            Ok(None)
        }
        fn lock_remove(&self, _: &str) -> io::Result<()> {
            Ok(())
        }
        fn copy(&self, _: &str, _: &str, _: Option<&str>) -> Result<Copied, CopyError> {
            unreachable!()
        }
        fn move_file(
            &self,
            from: &str,
            to: &str,
            expect: Option<&str>,
        ) -> Result<Moved, CopyError> {
            let first = self.moves.borrow().is_empty();
            self.moves
                .borrow_mut()
                .push((from.into(), to.into(), expect.map(str::to_string)));
            if !first && self.back_fails {
                return Err(CopyError::Io(io::Error::other("no network")));
            }
            Ok(Moved {
                source: "aa".into(),
                target: "bb".into(),
                size: 3,
                folders: Vec::new(),
            })
        }
    }

    #[test]
    fn a_file_that_changed_during_the_move_goes_back() {
        let racing = Racing {
            moves: RefCell::new(Vec::new()),
            back_fails: false,
        };
        let report = move_file(&racing, "a.md", "b.md", Some("aa"), "t", &go());
        assert_eq!(
            report.outcome,
            Outcome::MovedBack {
                expected: "aa".into(),
                found: "bb".into()
            }
        );
        assert_eq!(report.exit_code(true), exit::RESTORED);
        assert_eq!(
            racing.moves.borrow().as_slice(),
            [
                ("a.md".into(), "b.md".into(), Some("aa".into())),
                ("b.md".into(), "a.md".into(), Some("bb".into())),
            ]
        );
        // Without --fingerprint, the file moves as it is.
        let racing = Racing {
            moves: RefCell::new(Vec::new()),
            back_fails: false,
        };
        let report = move_file(&racing, "a.md", "b.md", None, "t", &go());
        assert_eq!(report.outcome.code(), 0);
        assert_eq!(racing.moves.borrow().len(), 1);
        // A move back that fails is damage, and says where the file is.
        let racing = Racing {
            moves: RefCell::new(Vec::new()),
            back_fails: true,
        };
        let report = move_file(&racing, "a.md", "b.md", Some("aa"), "t", &go());
        assert_eq!(report.exit_code(true), exit::NOT_RESTORED);
        let text = message("a.md", "b.md", &report.outcome);
        assert!(text.starts_with("DAMAGE POSSIBLE"), "{text}");
        assert!(
            text.contains("no network") && text.contains("It is at b.md now"),
            "{text}"
        );
    }

    #[test]
    fn the_setting_needs_both_names_inside_its_folders() {
        assert_eq!(gate("org/a/x.md", "org/elsewhere/x.md", ""), None);
        assert_eq!(gate("org/a/x.md", "org/elsewhere/x.md", " , "), None);
        let allow = "/org/a, /org/b";
        assert_eq!(gate("org/a/x.md", "org/b/2026/x.md", allow), None);
        assert_eq!(gate("/org/a/x.md", "/org/a/y.md", allow), None);
        for (from, to, named) in [
            ("org/a/x.md", "org/c/x.md", "org/c/x.md is outside"),
            ("org/c/x.md", "org/a/x.md", "org/c/x.md is outside"),
            (
                "org/c/x.md",
                "org/d/x.md",
                "org/c/x.md and org/d/x.md are outside",
            ),
            ("org/ab/x.md", "org/a/x.md", "org/ab/x.md is outside"),
        ] {
            let refused = gate(from, to, allow).unwrap();
            assert_eq!(refused.code(), exit::USAGE);
            let text = message(from, to, &refused);
            assert!(text.contains(named) && text.contains(MOVE_ALLOW), "{text}");
        }
    }

    #[test]
    fn a_renamed_markdown_file_says_that_wikilinks_break() {
        let moved = Outcome::Moved {
            print: "aa".into(),
            size: 3,
            folders: Vec::new(),
        };
        let text = message("org/notes/plan.md", "org/notes/goal.md", &moved);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(lines[1].contains("[[plan]]"), "{text}");
        // The same name in another folder, and a file that is not Markdown, keep their links.
        for (from, to) in [
            ("org/notes/plan.md", "org/2026/11/plan.md"),
            ("org/notes/a.pdf", "org/notes/b.pdf"),
        ] {
            assert_eq!(message(from, to, &moved).lines().count(), 1, "{from}");
        }
        assert!(breaks_links("a/Plan.MD", "a/plan.txt"));
    }

    #[test]
    fn the_log_line_has_the_old_name_as_its_eighth_field_and_reads_as_a_move() {
        let moved = Outcome::Moved {
            print: "aa".into(),
            size: 12,
            folders: vec!["org/new".into()],
        };
        assert_eq!(
            log_line("T", "org/a.md", "org/new/b.md", "chat", &moved),
            "T\tmoved\torg/new/b.md\tchat\t-\taa\t12\t/org/a.md"
        );
        assert_eq!(
            log_line("T", "/org/a.md", "org/b.md", "chat", &Outcome::Exists),
            "T\tmove-exists\torg/b.md\tchat\t-\t-\t-\t/org/a.md"
        );
        // What watch makes of it: a move, never a copy or a delete, done or refused.
        let refusals = [
            Outcome::Exists,
            Outcome::Missing,
            Outcome::Folder,
            Outcome::Changed {
                expected: "a".into(),
                found: "b".into(),
            },
            Outcome::NotAllowed { reason: "r".into() },
            Outcome::NotText { reason: "r".into() },
            Outcome::Locked {
                name: "n".into(),
                holder: "h".into(),
                age: Duration::from_secs(3),
            },
            Outcome::MovedBack {
                expected: "a".into(),
                found: "b".into(),
            },
            Outcome::Unexpected { reason: "r".into() },
            Outcome::Failed { reason: "r".into() },
        ];
        for outcome in refusals.iter().chain([&moved]) {
            let line = log_line("2026-11-09 10:00:00Z", "delete", "b", "chat", outcome);
            let entry = crate::watch::parse_line(&line).unwrap();
            assert!(!entry.delete, "{line}");
            assert_eq!(entry.copy_from, None, "{line}");
            assert_eq!(entry.move_from.as_deref(), Some("delete"), "{line}");
            let why = crate::watch::why_of(&entry);
            assert!(
                !why.contains("not known") && !why.contains("cop"),
                "{line}: {why}"
            );
        }
        for outcome in &refusals[..7] {
            assert!(
                message("a", "b", outcome).starts_with("refused, nothing moved."),
                "{outcome:?}"
            );
            assert_ne!(outcome.code(), 0);
        }
    }
}
