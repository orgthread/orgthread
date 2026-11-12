//! The delete procedure: one file, only the version the caller saw, with a way back.
//!
//! The steps: check the limits and `ORGTHREAD_DELETE_ALLOW`, find the fingerprint,
//! take the lock, keep the last version where the storage keeps copies, check the
//! stop flag and that the lock is still ours, remove, write the log line, release.
//!
//! The first build is narrow on purpose: files only, one per call, no undo, no
//! batch, and no question to a person. Each of these is an open question.

use crate::fingerprint::fingerprint;
use crate::storage::{Expect, Listing, RemoveError, Storage};
use crate::write::{self, Gate, exit, is_under};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub use crate::settings::DELETE_ALLOW;

/// True when `path` lies in one of the comma-separated folders of `allow`. With
/// the setting empty, the default, nothing may be deleted.
pub fn allowed(path: &str, allow: &str) -> bool {
    allow.split(',').any(|rule| is_under(path, rule.trim()))
}

/// `None` when `allow` lets this path be deleted, else the refusal.
pub fn gate(path: &str, allow: &str) -> Option<Outcome> {
    if allowed(path, allow) {
        return None;
    }
    let empty = allow.split(',').all(|rule| rule.trim().is_empty());
    Some(Outcome::NotAllowed { empty })
}

/// The 8th field of a refused delete, so `watch` tells it from a refused write.
pub const REFUSED_MARK: &str = "delete";

/// Where the fingerprint of a delete comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Known {
    /// What this name read, with the rev noted in the same reply, if any.
    Read { print: String, rev: Option<String> },
    /// Given with `--expect`, for example from `list --fingerprint`.
    Given(String),
    /// Nothing: the delete is refused.
    Nothing,
}

impl Known {
    fn print(&self) -> Option<&str> {
        match self {
            Known::Read { print, .. } | Known::Given(print) => Some(print),
            Known::Nothing => None,
        }
    }

    /// The rev only goes with the fingerprint it was noted with.
    fn rev(&self) -> Option<&str> {
        match self {
            Known::Read { rev, .. } => rev.as_deref(),
            _ => None,
        }
    }
}

/// What this name noted, or `--expect`. When both exist and differ, the given
/// one counts, and no rev goes with it.
pub fn known(noted: Option<(String, Option<String>)>, given: Option<&str>) -> Known {
    match (noted, given) {
        (Some((print, rev)), Some(given)) if print == given => Known::Read { print, rev },
        (_, Some(given)) => Known::Given(given.to_string()),
        (Some((print, rev)), None) => Known::Read { print, rev },
        (None, None) => Known::Nothing,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Deleted {
        print: String,
        size: u64,
        rev: Option<String>,
    },
    /// `--check`: all rules hold, and the file is the version expected.
    Checked {
        print: String,
        size: Option<u64>,
    },
    /// No fingerprint is known for this name, and none was given.
    NotKnown,
    Changed {
        expected: String,
        found: String,
    },
    Missing {
        expected: String,
    },
    /// The path names a folder, which is never deleted.
    Folder,
    /// The path is outside the folders of `ORGTHREAD_DELETE_ALLOW`. `empty`: the
    /// setting names no folder at all.
    NotAllowed {
        empty: bool,
    },
    Locked {
        holder: String,
        age: Duration,
    },
    /// The storage removed the path, but reports other bytes than expected.
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
            Outcome::Deleted { .. } | Outcome::Checked { .. } => exit::WRITTEN,
            Outcome::NotKnown | Outcome::Changed { .. } => exit::CHANGED,
            Outcome::Missing { .. } => exit::MISSING,
            Outcome::Folder | Outcome::NotAllowed { .. } => exit::USAGE,
            Outcome::Locked { .. } => exit::LOCKED,
            Outcome::Unexpected { .. } => exit::NOT_RESTORED,
            Outcome::Failed { .. } => exit::FAILED,
        }
    }

    /// The short word for the log.
    pub fn word(&self) -> &'static str {
        match self {
            Outcome::Deleted { .. } => "deleted",
            Outcome::Checked { .. } => "checked",
            Outcome::NotKnown => "not-read",
            Outcome::Changed { .. } => "changed",
            Outcome::Missing { .. } => "missing",
            Outcome::Folder => "folder",
            Outcome::NotAllowed { .. } => "not-allowed",
            Outcome::Locked { .. } => "locked",
            Outcome::Unexpected { .. } => "not-restored",
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
    fn plain(outcome: Outcome) -> Report {
        Report {
            outcome,
            took_over: None,
            lock_left: None,
            lock_lost: None,
        }
    }

    /// As for a write: a lock problem or a missing log line shows only on what
    /// would otherwise read as a clean delete.
    pub fn exit_code(&self, logged: bool) -> i32 {
        let code = self.outcome.code();
        if code != exit::WRITTEN {
            code
        } else if self.lock_left.is_some() || self.lock_lost.is_some() {
            exit::LOCK_PROBLEM
        } else if !logged && matches!(self.outcome, Outcome::Deleted { .. }) {
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

/// `--check`: the rules and the version, and nothing deleted. With Dropbox this is
/// one call, as a list of one file gives its hash. A local file is read.
pub fn check(storage: &dyn Storage, path: &str, known: &Known) -> Outcome {
    let Some(want) = known.print() else {
        return Outcome::NotKnown;
    };
    let entry = match storage.list(path, 1) {
        Ok(Listing::File(entry)) => entry,
        Ok(Listing::Folder { .. }) => return Outcome::Folder,
        Ok(Listing::Missing) => {
            return Outcome::Missing {
                expected: want.to_string(),
            };
        }
        Err(e) => return failed("could not look at the file", e),
    };
    let found = match entry.hash {
        Some(hash) => hash,
        None => match storage.read(path) {
            Ok(Some(bytes)) => fingerprint(&bytes),
            Ok(None) => {
                return Outcome::Missing {
                    expected: want.to_string(),
                };
            }
            Err(e) => return failed("could not read the file", e),
        },
    };
    if found != want {
        return Outcome::Changed {
            expected: want.to_string(),
            found,
        };
    }
    Outcome::Checked {
        print: found,
        size: entry.size,
    }
}

/// Keeps the last version before it goes, or `None` where the storage keeps no
/// copies on this disk, as on a server: Dropbox keeps the deleted file there.
pub type Keep<'a> = Option<&'a dyn Fn(&[u8]) -> io::Result<()>>;

/// One delete. `line` is the text of the lock: who and when.
pub fn delete(
    storage: &dyn Storage,
    path: &str,
    known: &Known,
    keep: Keep,
    line: &str,
    stop: &AtomicBool,
) -> Report {
    let Some(want) = known.print() else {
        return Report::plain(Outcome::NotKnown);
    };
    let run = write::under_lock(storage, path, line, |held| {
        // A local door reads the file first, to keep it: no copy, no delete.
        if let Some(keep) = keep {
            let bytes = match storage.read(path) {
                Ok(Some(bytes)) => bytes,
                Ok(None) => {
                    return Outcome::Missing {
                        expected: want.to_string(),
                    };
                }
                Err(e) => return failed("could not read the file", e),
            };
            let found = fingerprint(&bytes);
            if found != want {
                return Outcome::Changed {
                    expected: want.to_string(),
                    found,
                };
            }
            if let Err(e) = keep(&bytes) {
                return failed("nothing deleted, as the last version could not be kept", e);
            }
        }
        // The last safe point: from here on, the delete runs to its end.
        if stop.load(Ordering::SeqCst) {
            return failed("nothing deleted", "a stop was asked for");
        }
        match held.still_ours() {
            Ok(true) => {}
            Ok(false) => {
                return failed(
                    "nothing deleted",
                    "another writer came in during this delete",
                );
            }
            Err(e) => return failed("could not check that the file is free", e),
        }
        let expect = Expect::Fingerprint(want.to_string());
        match storage.remove_if(path, &expect, known.rev()) {
            Ok(removed) => Outcome::Deleted {
                print: want.to_string(),
                size: removed.size,
                rev: removed.rev,
            },
            Err(RemoveError::Changed { found: Some(found) }) => Outcome::Changed {
                expected: want.to_string(),
                found,
            },
            Err(RemoveError::Changed { found: None }) => Outcome::Missing {
                expected: want.to_string(),
            },
            Err(RemoveError::Unexpected { reason }) => Outcome::Unexpected { reason },
            Err(RemoveError::Io(e)) if e.kind() == io::ErrorKind::InvalidInput => {
                failed("nothing deleted", e)
            }
            Err(RemoveError::Io(e)) => failed("could not delete the file", e),
        }
    });
    let outcome = match run.result {
        Ok(outcome) => outcome,
        Err(Gate::Busy { holder, age }) => Outcome::Locked { holder, age },
        Err(Gate::Failed(reason)) => failed("could not reserve the file", reason),
    };
    Report {
        outcome,
        took_over: run.took_over,
        lock_left: run.lock_left,
        lock_lost: run.lock_lost,
    }
}

/// The log line: time, word, path, who, fingerprint before, `-` after, size, and
/// an 8th field: on a delete the rev, on a refusal the word `delete`. A refusal
/// for a changed file logs what was expected and what was found, as a write does.
pub fn log_line(now: &str, path: &str, who: &str, outcome: &Outcome) -> String {
    let fields: Vec<String> = match outcome {
        Outcome::Deleted { print, size, rev } => vec![
            print.clone(),
            String::new(),
            size.to_string(),
            rev.clone().unwrap_or_default(),
        ],
        Outcome::Changed { expected, found } => {
            vec![
                expected.clone(),
                found.clone(),
                String::new(),
                REFUSED_MARK.into(),
            ]
        }
        Outcome::Missing { expected } => vec![
            expected.clone(),
            String::new(),
            String::new(),
            REFUSED_MARK.into(),
        ],
        _ => vec![
            String::new(),
            String::new(),
            String::new(),
            REFUSED_MARK.into(),
        ],
    };
    let mut all = vec![now, outcome.word(), path, who];
    all.extend(fields.iter().map(String::as_str));
    crate::log::line(&all)
}

/// The answer for the person or the chat, in plain words.
pub fn message(path: &str, who: &str, outcome: &Outcome) -> String {
    match outcome {
        Outcome::Deleted { size, rev, .. } => match rev {
            Some(rev) => format!(
                "deleted: {path}, {size} bytes. Dropbox keeps it in Deleted files, rev {rev}."
            ),
            None => format!("deleted: {path}, {size} bytes. The last version is kept."),
        },
        Outcome::Checked { size, .. } => match size {
            Some(size) => format!("check passed, nothing deleted: {path}, {size} bytes"),
            None => format!("check passed, nothing deleted: {path}"),
        },
        Outcome::NotKnown => format!(
            "refused, nothing deleted. {who} has not read {path}, and no --expect was given. Read it first with: orgthread read <file> --who <same name>"
        ),
        Outcome::Changed { .. } => format!(
            "refused, nothing deleted. {path} changed since you saw it. Read it again, and look before you delete."
        ),
        Outcome::Missing { .. } => format!("refused, nothing deleted. {path} is gone."),
        Outcome::Folder => {
            format!("refused, nothing deleted. {path} is a folder, and only a file is deleted.")
        }
        Outcome::NotAllowed { empty: true } => format!(
            "refused, nothing deleted. No folder is open for deletes yet: {DELETE_ALLOW} is empty, so every delete is refused. The owner names the folders there, separated by commas. {path} stays."
        ),
        Outcome::NotAllowed { empty: false } => format!(
            "refused, nothing deleted. {path} is outside the folders of {DELETE_ALLOW}. Only the owner can change that setting."
        ),
        Outcome::Locked { holder, age } => format!(
            "refused, nothing deleted. {path} is busy since {} s with another writer: {holder}. Try again in a moment.",
            age.as_secs()
        ),
        Outcome::Unexpected { reason } => format!("DAMAGE POSSIBLE. {reason}"),
        Outcome::Failed { reason } => format!("failed, {path} is unchanged: {reason}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::LocalFolder;
    use crate::versions::Versions;
    use std::fs;
    use std::path::PathBuf;

    fn place(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("orgthread-delete-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("root/notes")).unwrap();
        fs::create_dir_all(dir.join("root/.locks")).unwrap();
        dir
    }

    fn go() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn only_the_folders_of_the_setting_allow_a_delete() {
        assert!(!allowed("org/notes/a.md", ""));
        assert!(!allowed("org/notes/a.md", " , "));
        assert!(allowed("org/notes/a.md", "org/notes"));
        assert!(allowed("org/notes/a.md", "other, /org/notes/"));
        assert!(!allowed("org/notes-old/a.md", "org/notes"));
        assert!(!allowed("org/a.md", "org/notes"));
        assert_eq!(
            gate("org/a.md", " , "),
            Some(Outcome::NotAllowed { empty: true })
        );
        assert_eq!(
            gate("org/a.md", "org/notes"),
            Some(Outcome::NotAllowed { empty: false })
        );
        assert_eq!(gate("org/notes/a.md", "org/notes"), None);
        let refused = log_line("T", "org/a.md", "x", &Outcome::NotAllowed { empty: true });
        assert_eq!(refused, "T\tnot-allowed\torg/a.md\tx\t-\t-\t-\tdelete");
    }

    #[test]
    fn the_given_fingerprint_counts_and_takes_no_rev_of_another() {
        let noted = || Some(("aa".to_string(), Some("r1".to_string())));
        assert_eq!(known(None, None), Known::Nothing);
        assert_eq!(
            known(noted(), None),
            Known::Read {
                print: "aa".into(),
                rev: Some("r1".into())
            }
        );
        assert_eq!(known(noted(), Some("bb")), Known::Given("bb".into()));
        assert_eq!(known(noted(), Some("bb")).rev(), None);
        assert_eq!(known(noted(), Some("aa")).rev(), Some("r1"));
    }

    #[test]
    fn a_local_delete_keeps_the_last_version_and_removes_the_file() {
        let dir = place("local");
        let root = dir.join("root");
        fs::write(root.join("notes/a.md"), "Grüße\n").unwrap();
        let storage = LocalFolder::new(&root, ".locks");
        let versions = Versions::new(dir.join("versions"));
        let keep = |b: &[u8]| versions.save(b);
        let known = Known::Given(fingerprint("Grüße\n".as_bytes()));
        let report = delete(&storage, "notes/a.md", &known, Some(&keep), "test", &go());
        assert_eq!(
            report.outcome,
            Outcome::Deleted {
                print: fingerprint("Grüße\n".as_bytes()),
                size: 8,
                rev: None
            }
        );
        assert!(!root.join("notes/a.md").exists());
        let kept = versions.load(&fingerprint("Grüße\n".as_bytes())).unwrap();
        assert_eq!(kept.unwrap(), "Grüße\n".as_bytes());
        assert_eq!(fs::read_dir(root.join(".locks")).unwrap().count(), 0);
        assert_eq!(report.exit_code(true), 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn no_fingerprint_means_nothing_deleted() {
        let dir = place("unknown");
        let root = dir.join("root");
        fs::write(root.join("notes/a.md"), "text\n").unwrap();
        let storage = LocalFolder::new(&root, ".locks");
        let report = delete(&storage, "notes/a.md", &Known::Nothing, None, "test", &go());
        assert_eq!(report.outcome, Outcome::NotKnown);
        assert_eq!(report.exit_code(true), exit::CHANGED);
        assert!(root.join("notes/a.md").exists());
        assert_eq!(
            check(&storage, "notes/a.md", &Known::Nothing),
            Outcome::NotKnown
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_changed_file_stays_with_or_without_a_copy() {
        let dir = place("changed");
        let root = dir.join("root");
        fs::write(root.join("notes/a.md"), "edited\n").unwrap();
        let storage = LocalFolder::new(&root, ".locks");
        let known = Known::Given(fingerprint(b"seen\n"));
        let keep = |_: &[u8]| Ok(());
        for keep in [None, Some(&keep as &dyn Fn(&[u8]) -> io::Result<()>)] {
            let report = delete(&storage, "notes/a.md", &known, keep, "test", &go());
            assert_eq!(
                report.outcome,
                Outcome::Changed {
                    expected: fingerprint(b"seen\n"),
                    found: fingerprint(b"edited\n"),
                }
            );
            assert_eq!(report.exit_code(true), exit::CHANGED);
            assert_eq!(fs::read(root.join("notes/a.md")).unwrap(), b"edited\n");
            assert_eq!(fs::read_dir(root.join(".locks")).unwrap().count(), 0);
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failed_copy_deletes_nothing() {
        let dir = place("nocopy");
        let root = dir.join("root");
        fs::write(root.join("notes/a.md"), "text\n").unwrap();
        let storage = LocalFolder::new(&root, ".locks");
        let keep = |_: &[u8]| Err(io::Error::other("disk full"));
        let known = Known::Given(fingerprint(b"text\n"));
        let report = delete(&storage, "notes/a.md", &known, Some(&keep), "test", &go());
        assert_eq!(report.outcome.code(), exit::FAILED);
        assert!(root.join("notes/a.md").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_stop_or_a_busy_lock_deletes_nothing() {
        let dir = place("stop");
        let root = dir.join("root");
        fs::write(root.join("notes/a.md"), "text\n").unwrap();
        let storage = LocalFolder::new(&root, ".locks");
        let known = Known::Given(fingerprint(b"text\n"));
        let report = delete(
            &storage,
            "notes/a.md",
            &known,
            None,
            "t",
            &AtomicBool::new(true),
        );
        assert_eq!(report.outcome.code(), exit::FAILED);
        storage
            .lock_create(&write::lock_name("notes/a.md"), "other")
            .unwrap();
        let report = delete(&storage, "notes/a.md", &known, None, "t", &go());
        assert_eq!(report.outcome.code(), exit::LOCKED);
        assert!(root.join("notes/a.md").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_gone_file_is_missing_and_a_folder_is_refused() {
        let dir = place("gone");
        let root = dir.join("root");
        let storage = LocalFolder::new(&root, ".locks");
        let known = Known::Given(fingerprint(b"text\n"));
        let report = delete(&storage, "notes/a.md", &known, None, "t", &go());
        assert_eq!(report.outcome.code(), exit::MISSING);
        let report = delete(&storage, "notes", &known, None, "t", &go());
        assert_eq!(report.outcome.code(), exit::FAILED);
        assert!(root.join("notes").is_dir());
        assert_eq!(check(&storage, "notes", &known), Outcome::Folder);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn check_says_the_version_and_deletes_nothing() {
        let dir = place("check");
        let root = dir.join("root");
        fs::write(root.join("notes/a.md"), "text\n").unwrap();
        let storage = LocalFolder::new(&root, ".locks");
        let right = Known::Given(fingerprint(b"text\n"));
        assert_eq!(
            check(&storage, "notes/a.md", &right),
            Outcome::Checked {
                print: fingerprint(b"text\n"),
                size: Some(5)
            }
        );
        let wrong = Known::Given(fingerprint(b"other\n"));
        assert_eq!(check(&storage, "notes/a.md", &wrong).code(), exit::CHANGED);
        assert_eq!(check(&storage, "notes/b.md", &right).code(), exit::MISSING);
        assert!(root.join("notes/a.md").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_log_line_has_the_rev_or_the_mark_as_its_eighth_field() {
        let deleted = Outcome::Deleted {
            print: "aa".into(),
            size: 12,
            rev: Some("015f".into()),
        };
        assert_eq!(
            log_line("T", "org/a.md", "chat", &deleted),
            "T\tdeleted\torg/a.md\tchat\taa\t-\t12\t015f"
        );
        let local = Outcome::Deleted {
            print: "aa".into(),
            size: 12,
            rev: None,
        };
        assert_eq!(
            log_line("T", "org/a.md", "chat", &local),
            "T\tdeleted\torg/a.md\tchat\taa\t-\t12\t-"
        );
        assert_eq!(
            log_line("T", "org/a.md", "chat", &Outcome::NotKnown),
            "T\tnot-read\torg/a.md\tchat\t-\t-\t-\tdelete"
        );
    }
}
