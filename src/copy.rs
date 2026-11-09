//! The copy procedure: one file to a new name, never over another file.
//!
//! The steps: check the limits of both ends, take the lock of the target, check
//! the stop flag and that the lock is still ours, copy, compare the fingerprint of
//! the new file with the one of the source, remove the copy if they differ, write
//! the log line, release. A copy replaces nothing, so it needs no backup. Its undo
//! is a delete of that exact copy.

use crate::storage::{CopyError, Expect, RemoveError, Storage};
use crate::write::{self, Gate, exit};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Copied {
        print: String,
        size: u64,
    },
    /// A file or folder is at the target already.
    Exists,
    /// There is no source.
    Missing,
    /// The source is a folder, and only a file is copied.
    Folder,
    /// The source is not the version of `--fingerprint`.
    Changed {
        expected: String,
        found: String,
    },
    /// The limits of the door: a closed source, or a closed or read-only target.
    NotAllowed {
        reason: String,
    },
    /// A file that is not text, to a folder outside `ORGTHREAD_BINARY_ALLOW`.
    NotText {
        reason: String,
    },
    Locked {
        holder: String,
        age: Duration,
    },
    /// The copy differed from the source, and it is removed again.
    Removed {
        source: String,
        target: String,
    },
    /// The copy differed from the source, or is no file, and it could not be
    /// removed. `reason` says what is there now.
    NotRemoved {
        source: String,
        target: String,
        reason: String,
    },
    Failed {
        reason: String,
    },
}

impl Outcome {
    pub fn code(&self) -> i32 {
        match self {
            Outcome::Copied { .. } => exit::WRITTEN,
            Outcome::Exists => exit::EXISTS,
            Outcome::Missing => exit::MISSING,
            Outcome::Folder | Outcome::NotAllowed { .. } | Outcome::NotText { .. } => exit::USAGE,
            Outcome::Changed { .. } => exit::CHANGED,
            Outcome::Locked { .. } => exit::LOCKED,
            Outcome::Removed { .. } => exit::RESTORED,
            Outcome::NotRemoved { .. } => exit::NOT_RESTORED,
            Outcome::Failed { .. } => exit::FAILED,
        }
    }

    /// The short word for the log.
    pub fn word(&self) -> &'static str {
        match self {
            Outcome::Copied { .. } => "copied",
            Outcome::Exists => "exists",
            Outcome::Missing => "missing",
            Outcome::Folder => "folder",
            Outcome::Changed { .. } => "changed",
            Outcome::NotAllowed { .. } => "not-allowed",
            Outcome::NotText { .. } => "not-text",
            Outcome::Locked { .. } => "locked",
            // Not restored: a copy brings nothing back, it only goes again.
            Outcome::Removed { .. } => "removed",
            Outcome::NotRemoved { .. } => "not-removed",
            Outcome::Failed { .. } => "failed",
        }
    }
}

#[derive(Debug)]
pub struct Report {
    pub outcome: Outcome,
    pub took_over: Option<String>,
    pub lock_left: Option<String>,
    pub lock_lost: Option<String>,
}

impl Report {
    pub fn plain(outcome: Outcome) -> Report {
        Report {
            outcome,
            took_over: None,
            lock_left: None,
            lock_lost: None,
        }
    }

    /// As for a write: a lock problem or a missing log line shows only on what
    /// would otherwise read as a clean copy.
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

/// One copy. `expect` is the fingerprint the source must have, from
/// `--fingerprint`. `line` is the text of the lock: who and when. The lock is the
/// one of the target, so a copy and a write of the same name never meet.
pub fn copy(
    storage: &dyn Storage,
    from: &str,
    to: &str,
    expect: Option<&str>,
    line: &str,
    stop: &AtomicBool,
) -> Report {
    let run = write::under_lock(storage, to, line, |held| {
        // The last safe point: from here on, the copy runs to its end and its check.
        if stop.load(Ordering::SeqCst) {
            return failed("nothing copied", "a stop was asked for");
        }
        match held.still_ours() {
            Ok(true) => {}
            Ok(false) => {
                return failed("nothing copied", "another writer came in before this copy");
            }
            Err(e) => return failed("could not check that the target is free", e),
        }
        let made = match storage.copy(from, to, expect) {
            Ok(made) => made,
            Err(CopyError::Exists) => return Outcome::Exists,
            Err(CopyError::Missing) => return Outcome::Missing,
            Err(CopyError::Folder) => return Outcome::Folder,
            Err(CopyError::Changed { found }) => {
                return Outcome::Changed {
                    expected: expect.unwrap_or_default().to_string(),
                    found,
                };
            }
            Err(CopyError::Unexpected { reason }) => {
                return Outcome::NotRemoved {
                    source: String::new(),
                    target: String::new(),
                    reason,
                };
            }
            Err(CopyError::Io(e)) => return failed("could not copy the file", e),
        };
        if made.source == made.target {
            return Outcome::Copied {
                print: made.target,
                size: made.size,
            };
        }
        // Only the exact copy that was made goes, by its fingerprint and its rev.
        let expect = Expect::Fingerprint(made.target.clone());
        match storage.remove_if(to, &expect, made.rev.as_deref()) {
            Ok(_) => Outcome::Removed {
                source: made.source,
                target: made.target,
            },
            Err(e) => {
                let why = match e {
                    RemoveError::Changed { found: Some(found) } => {
                        format!("the copy changed again, to fingerprint {found}")
                    }
                    RemoveError::Changed { found: None } => "the copy is gone already".to_string(),
                    RemoveError::Unexpected { reason } => reason,
                    RemoveError::Io(e) => e.to_string(),
                };
                Outcome::NotRemoved {
                    source: made.source,
                    target: made.target,
                    reason: format!("removing the copy failed: {why}"),
                }
            }
        }
    });
    let outcome = match run.result {
        Ok(outcome) => outcome,
        Err(Gate::Busy { holder, age }) => Outcome::Locked { holder, age },
        Err(Gate::Failed(reason)) => failed("could not reserve the target", reason),
    };
    Report {
        outcome,
        took_over: run.took_over,
        lock_left: run.lock_left,
        lock_lost: run.lock_lost,
    }
}

/// The 8th field of a copy line: the source, with a `/` at its start. No other
/// line has a `/` there, so `watch` knows a copy, and a source named `delete`
/// never reads as the mark of a refused delete.
pub fn source_field(from: &str) -> String {
    format!("/{}", from.trim_start_matches('/'))
}

/// The log line: time, word, target, who, fingerprint before, after, size, and
/// the source as the 8th field. A new file has no before. A refusal for a changed
/// source logs what was expected and what was found, as a write does. A removed
/// copy logs the source fingerprint and the one of the copy.
pub fn log_line(now: &str, from: &str, to: &str, who: &str, outcome: &Outcome) -> String {
    let (before, after, size) = match outcome {
        Outcome::Copied { print, size } => (String::new(), print.clone(), size.to_string()),
        Outcome::Changed { expected, found } => (expected.clone(), found.clone(), String::new()),
        Outcome::Removed { source, target } | Outcome::NotRemoved { source, target, .. } => {
            (source.clone(), target.clone(), String::new())
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

/// The answer for the person or the chat, in plain words.
pub fn message(from: &str, to: &str, outcome: &Outcome) -> String {
    match outcome {
        Outcome::Copied { print, size } => {
            format!("copied: {from} -> {to}, {size} bytes, {print}")
        }
        Outcome::Exists => format!(
            "refused, nothing copied. {to} exists, and a copy never replaces a file. Name a new copy."
        ),
        Outcome::Missing => format!("refused, nothing copied. There is no file {from}."),
        Outcome::Folder => {
            format!("refused, nothing copied. {from} is a folder, and only a file is copied.")
        }
        Outcome::Changed { found, .. } => format!(
            "refused, nothing copied. {from} is not the version of --fingerprint: it has {found} now. Look at it again before you copy."
        ),
        Outcome::NotAllowed { reason } => format!("refused, nothing copied. {reason}"),
        Outcome::NotText { reason } => format!("refused, nothing copied. {reason}"),
        Outcome::Locked { holder, age } => format!(
            "refused, nothing copied. {to} is busy since {} s with another writer: {holder}. Try again in a moment.",
            age.as_secs()
        ),
        Outcome::Removed { source, target } => format!(
            "the check after the copy failed: {to} came out with fingerprint {target}, not {source} as {from}. The copy is removed again, and {to} is not there."
        ),
        Outcome::NotRemoved { reason, .. } => format!(
            "DAMAGE POSSIBLE. The check after the copy failed, and {to} is still there: {reason}. Look at {to} now."
        ),
        Outcome::Failed { reason } => format!("failed: {reason}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::fingerprint;
    use crate::storage::{Copied, LocalFolder, Removed};
    use std::cell::RefCell;
    use std::fs;
    use std::io;
    use std::path::PathBuf;

    fn place(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("orgthread-copy-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("root/notes")).unwrap();
        fs::create_dir_all(dir.join("root/.locks")).unwrap();
        dir
    }

    fn go() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn a_local_copy_makes_the_parents_and_leaves_no_temp_file() {
        let dir = place("local");
        let root = dir.join("root");
        fs::write(root.join("notes/a.md"), "Grüße\n").unwrap();
        let storage = LocalFolder::new(&root, ".locks");
        let report = copy(&storage, "notes/a.md", "new/deep/b.md", None, "t", &go());
        assert_eq!(
            report.outcome,
            Outcome::Copied {
                print: fingerprint("Grüße\n".as_bytes()),
                size: 8
            }
        );
        assert_eq!(
            fs::read(root.join("new/deep/b.md")).unwrap(),
            "Grüße\n".as_bytes()
        );
        assert_eq!(
            fs::read(root.join("notes/a.md")).unwrap(),
            "Grüße\n".as_bytes()
        );
        assert_eq!(fs::read_dir(root.join(".locks")).unwrap().count(), 0);
        assert_eq!(report.exit_code(true), 0);
        assert_eq!(report.exit_code(false), exit::NOT_LOGGED);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_local_copy_never_replaces_a_file() {
        let dir = place("exists");
        let root = dir.join("root");
        fs::write(root.join("notes/a.md"), "new\n").unwrap();
        fs::write(root.join("notes/b.md"), "old\n").unwrap();
        let storage = LocalFolder::new(&root, ".locks");
        let report = copy(&storage, "notes/a.md", "notes/b.md", None, "t", &go());
        assert_eq!(report.outcome, Outcome::Exists);
        assert_eq!(report.exit_code(true), exit::EXISTS);
        assert_eq!(fs::read(root.join("notes/b.md")).unwrap(), b"old\n");
        // Also the same name, and a folder in the way.
        let report = copy(&storage, "notes/a.md", "notes/a.md", None, "t", &go());
        assert_eq!(report.outcome, Outcome::Exists);
        let report = copy(&storage, "notes/a.md", "notes", None, "t", &go());
        assert_eq!(report.outcome, Outcome::Exists);
        assert_eq!(fs::read_dir(root.join(".locks")).unwrap().count(), 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_source_a_folder_and_a_changed_source_copy_nothing() {
        let dir = place("refused");
        let root = dir.join("root");
        fs::write(root.join("notes/a.md"), "now\n").unwrap();
        let storage = LocalFolder::new(&root, ".locks");
        let report = copy(&storage, "notes/gone.md", "x/b.md", None, "t", &go());
        assert_eq!(report.outcome, Outcome::Missing);
        let report = copy(&storage, "notes", "x/b.md", None, "t", &go());
        assert_eq!(report.outcome, Outcome::Folder);
        let seen = fingerprint(b"seen\n");
        let report = copy(&storage, "notes/a.md", "x/b.md", Some(&seen), "t", &go());
        assert_eq!(
            report.outcome,
            Outcome::Changed {
                expected: seen,
                found: fingerprint(b"now\n")
            }
        );
        assert_eq!(report.exit_code(true), exit::CHANGED);
        assert!(!root.join("x").exists(), "a refusal makes no folder");
        assert_eq!(fs::read_dir(root.join(".locks")).unwrap().count(), 0);
        let right = fingerprint(b"now\n");
        let report = copy(&storage, "notes/a.md", "x/b.md", Some(&right), "t", &go());
        assert_eq!(report.outcome.code(), 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_stop_or_a_busy_lock_copies_nothing() {
        let dir = place("stop");
        let root = dir.join("root");
        fs::write(root.join("notes/a.md"), "text\n").unwrap();
        let storage = LocalFolder::new(&root, ".locks");
        let stop = AtomicBool::new(true);
        let report = copy(&storage, "notes/a.md", "notes/b.md", None, "t", &stop);
        assert_eq!(report.outcome.code(), exit::FAILED);
        storage
            .lock_create(&write::lock_name("notes/b.md"), "a writer")
            .unwrap();
        let report = copy(&storage, "notes/a.md", "notes/b.md", None, "t", &go());
        assert_eq!(report.outcome.code(), exit::LOCKED);
        assert!(!root.join("notes/b.md").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    /// A storage that makes a copy with other bytes than the source, as a broken
    /// transfer would, and notes each remove.
    struct Bad {
        removed: RefCell<Vec<(String, Expect, Option<String>)>>,
        remove_fails: bool,
    }

    impl Storage for Bad {
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
            Ok(Copied {
                source: "aa".into(),
                target: "bb".into(),
                size: 3,
                rev: Some("r7".into()),
            })
        }
        fn remove_if(
            &self,
            path: &str,
            expect: &Expect,
            rev: Option<&str>,
        ) -> Result<Removed, RemoveError> {
            self.removed
                .borrow_mut()
                .push((path.into(), expect.clone(), rev.map(str::to_string)));
            if self.remove_fails {
                Err(RemoveError::Io(io::Error::other("no network")))
            } else {
                Ok(Removed {
                    size: 3,
                    rev: rev.map(str::to_string),
                })
            }
        }
    }

    #[test]
    fn a_copy_that_differs_is_removed_by_its_own_fingerprint_and_rev() {
        let bad = Bad {
            removed: RefCell::new(Vec::new()),
            remove_fails: false,
        };
        let report = copy(&bad, "a.md", "b.md", None, "t", &go());
        assert_eq!(
            report.outcome,
            Outcome::Removed {
                source: "aa".into(),
                target: "bb".into()
            }
        );
        assert_eq!(report.exit_code(true), exit::RESTORED);
        assert_eq!(
            bad.removed.borrow().as_slice(),
            [(
                "b.md".to_string(),
                Expect::Fingerprint("bb".into()),
                Some("r7".to_string())
            )]
        );
        let failing = Bad {
            removed: RefCell::new(Vec::new()),
            remove_fails: true,
        };
        let report = copy(&failing, "a.md", "b.md", None, "t", &go());
        assert_eq!(report.exit_code(true), exit::NOT_RESTORED);
        let text = message("a.md", "b.md", &report.outcome);
        assert!(text.starts_with("DAMAGE POSSIBLE"), "{text}");
        assert!(text.contains("no network"), "{text}");
    }

    #[test]
    fn the_log_line_has_the_source_with_a_slash_as_its_eighth_field() {
        let copied = Outcome::Copied {
            print: "aa".into(),
            size: 12,
        };
        assert_eq!(
            log_line("T", "org/a.md", "org/b.md", "chat", &copied),
            "T\tcopied\torg/b.md\tchat\t-\taa\t12\t/org/a.md"
        );
        assert_eq!(
            log_line("T", "/org/a.md", "org/b.md", "chat", &Outcome::Exists),
            "T\texists\torg/b.md\tchat\t-\t-\t-\t/org/a.md"
        );
        // A source named like the delete mark keeps its slash.
        assert_eq!(
            log_line("T", "delete", "org/b.md", "chat", &Outcome::Missing),
            "T\tmissing\torg/b.md\tchat\t-\t-\t-\t/delete"
        );
        // What watch makes of it: a copy, never a delete, copied or refused.
        for outcome in [copied.clone(), Outcome::Missing, Outcome::Exists] {
            let line = log_line("2026-11-09 10:00:00Z", "delete", "b", "chat", &outcome);
            let entry = crate::watch::parse_line(&line).unwrap();
            assert!(!entry.delete, "{line}");
            assert_eq!(entry.copy_from.as_deref(), Some("delete"));
        }
        let changed = Outcome::Changed {
            expected: "aa".into(),
            found: "bb".into(),
        };
        assert_eq!(
            log_line("T", "a", "b", "chat", &changed),
            "T\tchanged\tb\tchat\taa\tbb\t-\t/a"
        );
    }

    #[test]
    fn a_removed_copy_logs_removed_never_restored() {
        let removed = Outcome::Removed {
            source: "aa".into(),
            target: "bb".into(),
        };
        let not_removed = Outcome::NotRemoved {
            source: "aa".into(),
            target: "bb".into(),
            reason: "r".into(),
        };
        assert_eq!(removed.word(), "removed");
        assert_eq!(not_removed.word(), "not-removed");
        for outcome in [&removed, &not_removed] {
            let line = log_line("T", "a", "b", "chat", outcome);
            assert!(!line.contains("restored"), "{line}");
            let entry = crate::watch::parse_line(&line).unwrap();
            let why = crate::watch::why_of(&entry);
            assert!(why.contains("removed"), "{why}");
            assert!(!why.contains("not known"), "{why}");
        }
    }

    #[test]
    fn every_refusal_says_nothing_copied() {
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
                holder: "h".into(),
                age: Duration::from_secs(3),
            },
        ];
        for outcome in refusals {
            let text = message("a", "b", &outcome);
            assert!(text.starts_with("refused, nothing copied."), "{text}");
            assert_ne!(outcome.code(), 0);
        }
    }
}
