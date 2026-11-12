//! The write procedure: take the lock, compare, write, read back, release.

use crate::fingerprint::fingerprint;
use crate::storage::{Expect, Storage, WriteError};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// A lock older than this is stale.
pub const STALE_AFTER: Duration = Duration::from_secs(15 * 60);

/// Exit codes of the command. Each outcome keeps its own number.
pub mod exit {
    pub const WRITTEN: i32 = 0;
    pub const FAILED: i32 = 1;
    pub const USAGE: i32 = 2;
    pub const LOCKED: i32 = 3;
    pub const CHANGED: i32 = 4;
    pub const EXISTS: i32 = 5;
    pub const MISSING: i32 = 6;
    pub const RESTORED: i32 = 7;
    pub const NOT_LOGGED: i32 = 8;
    pub const NOT_RESTORED: i32 = 9;
    pub const LOCK_PROBLEM: i32 = 10;
    pub const MISFIT: i32 = 11;
}

/// The lock name of a file: its path from the root with each `/` written as `--`.
pub fn lock_name(path: &str) -> String {
    format!("{}.lock", path.replace('/', "--"))
}

/// Plain text only: valid UTF-8 with no NUL sign, and not empty, so that a broken
/// pipe can never wipe a file. Every byte of it is kept as it is.
pub fn check_text(bytes: &[u8]) -> Result<(), String> {
    if bytes.is_empty() {
        return Err("the new text is empty, and an empty write is refused".into());
    }
    if std::str::from_utf8(bytes).is_err() {
        return Err("the new text is not valid UTF-8, and only plain text is written".into());
    }
    if bytes.contains(&0) {
        return Err("the new text holds a NUL sign, and only plain text is written".into());
    }
    Ok(())
}

/// True when `path` is the folder or file `rule`, or lies inside it. Capitals do
/// not count, as Dropbox does not count them either.
pub fn is_under(path: &str, rule: &str) -> bool {
    let path = path.trim_start_matches('/').to_lowercase();
    let rule = rule.trim_matches('/').to_lowercase();
    !rule.is_empty() && (path == rule || path.starts_with(&format!("{rule}/")))
}

/// The limits of a door, from comma-separated lists. An empty `allow` allows all.
/// A refusal says why. These guard against a confused chat, not against a thief.
pub fn check_limits(
    path: &str,
    writing: bool,
    allow: &str,
    deny: &str,
    read_only: &str,
) -> Result<(), String> {
    let list = |text: &str| -> Vec<String> {
        text.split(',')
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty())
            .collect()
    };
    let allowed = list(allow);
    if !allowed.is_empty() && !allowed.iter().any(|rule| is_under(path, rule)) {
        return Err(format!("{path} is outside the folders this door may touch"));
    }
    if list(deny).iter().any(|rule| is_under(path, rule)) {
        return Err(format!("{path} is closed to this door"));
    }
    if writing && list(read_only).iter().any(|rule| is_under(path, rule)) {
        return Err(format!("{path} is read only for this door"));
    }
    Ok(())
}

/// The setting that keeps the first level fixed.
pub use crate::settings::FIXED_FOLDERS;

/// The guard of `ORGTHREAD_FIXED_FOLDERS`, a comma-separated list: no new folder
/// may appear directly inside a folder of the list. Deeper new folders are fine.
/// `new` holds the new folders from the root, as `new_folders` gives them. `/`
/// in the list stands for the root itself. An empty list guards nothing.
pub fn check_fixed(new: &[String], fixed: &str) -> Result<(), String> {
    let clean = |path: &str| path.trim().trim_matches('/').to_lowercase();
    let list: Vec<String> = fixed
        .split(',')
        .filter(|rule| !rule.trim().is_empty())
        .map(clean)
        .collect();
    for folder in new {
        let parent = match folder.trim_matches('/').rsplit_once('/') {
            Some((parent, _)) => clean(parent),
            None => String::new(),
        };
        if let Some(rule) = list.iter().find(|rule| **rule == parent) {
            let place = if rule.is_empty() { "/" } else { rule.as_str() };
            return Err(format!(
                "{folder} would be a new folder directly in {place}, a folder of {FIXED_FOLDERS}. Only the owner makes a new folder there, by hand. Use a folder that exists."
            ));
        }
    }
    Ok(())
}

#[derive(Debug)]
pub enum Outcome {
    Written {
        old: Option<String>,
        new: String,
        size: usize,
        /// The old text, so that the caller can keep a copy.
        old_text: Option<Vec<u8>>,
        /// The new text, as written.
        new_text: Vec<u8>,
        /// Notes of a patch for the answer.
        notes: Vec<String>,
        /// The folders made for the file, from the root, the outer one first.
        folders: Vec<String>,
    },
    /// An edit of a patch does not fit the file. Nothing was written.
    Misfit {
        reason: String,
    },
    /// The text a patch would make is not plain text. Nothing was written.
    NotText {
        reason: String,
    },
    Locked {
        holder: String,
        age: Duration,
    },
    Changed {
        expected: String,
        found: String,
    },
    Exists {
        found: String,
    },
    Missing {
        expected: String,
    },
    /// The check after the move failed, and the old state is back. What the file
    /// held at that moment is kept at `rescued`, as it may be a person's edit.
    Restored {
        reason: String,
        rescued: Option<String>,
    },
    /// The check failed and the old state is not back.
    NotRestored {
        reason: String,
        rescued: Option<String>,
    },
    Failed {
        reason: String,
    },
}

impl Outcome {
    pub fn code(&self) -> i32 {
        match self {
            Outcome::Written { .. } => exit::WRITTEN,
            Outcome::Locked { .. } => exit::LOCKED,
            Outcome::Changed { .. } => exit::CHANGED,
            Outcome::Exists { .. } => exit::EXISTS,
            Outcome::Missing { .. } => exit::MISSING,
            Outcome::Restored { .. } => exit::RESTORED,
            Outcome::NotRestored { .. } => exit::NOT_RESTORED,
            Outcome::Failed { .. } => exit::FAILED,
            Outcome::Misfit { .. } => exit::MISFIT,
            Outcome::NotText { .. } => exit::USAGE,
        }
    }

    /// The short word for the log.
    pub fn word(&self) -> &'static str {
        match self {
            Outcome::Written { .. } => "written",
            Outcome::Locked { .. } => "locked",
            Outcome::Changed { .. } => "changed",
            Outcome::Exists { .. } => "exists",
            Outcome::Missing { .. } => "missing",
            Outcome::Restored { .. } => "restored",
            Outcome::NotRestored { .. } => "not-restored",
            Outcome::Failed { .. } => "failed",
            Outcome::Misfit { .. } => "misfit",
            Outcome::NotText { .. } => "not-text",
        }
    }
}

#[derive(Debug)]
pub struct Report {
    pub outcome: Outcome,
    /// The line of a stale lock this write took over.
    pub took_over: Option<String>,
    /// Set when the lock could not be released, with the reason.
    pub lock_left: Option<String>,
    /// Set when the lock was no longer this write's own at the release.
    pub lock_lost: Option<String>,
}

impl Report {
    /// The exit code of the whole write. A lock problem shows only on what would
    /// otherwise read as a clean write: every other code says more.
    pub fn exit_code(&self, logged: bool) -> i32 {
        let code = self.outcome.code();
        if code != exit::WRITTEN {
            return code;
        }
        if self.lock_left.is_some() || self.lock_lost.is_some() {
            exit::LOCK_PROBLEM
        } else if !logged {
            exit::NOT_LOGGED
        } else {
            exit::WRITTEN
        }
    }
}

/// Releases the lock also when the steps panic. It removes the lock only while
/// the lock still holds this write's own line.
pub(crate) struct Held<'a> {
    storage: &'a dyn Storage,
    name: String,
    line: String,
    done: bool,
}

enum Release {
    Done,
    /// Another writer's line, or nothing when the lock is gone.
    NotOurs(Option<String>),
}

impl Held<'_> {
    fn is_ours(&self) -> io::Result<bool> {
        Ok(self.storage.lock_read(&self.name)?.as_deref() == Some(self.line.as_str()))
    }

    fn release(&mut self) -> io::Result<Release> {
        self.done = true;
        match self.storage.lock_read(&self.name)? {
            Some(line) if line == self.line => {
                self.storage.lock_remove(&self.name)?;
                Ok(Release::Done)
            }
            other => Ok(Release::NotOurs(other)),
        }
    }
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        if !self.done {
            let _ = self.release();
        }
    }
}

enum Acquired {
    Yes { took_over: Option<String> },
    No(Outcome),
}

fn acquire(storage: &dyn Storage, name: &str, line: &str) -> io::Result<Acquired> {
    let mut took_over = None;
    // Three tries: a lock can go away, or turn out stale, between two looks.
    for _ in 0..3 {
        if storage.lock_create(name, line)? {
            return Ok(Acquired::Yes { took_over });
        }
        match storage.lock_age(name)? {
            None => continue,
            Some(age) if age > STALE_AFTER => {
                if let Some(old) = storage.lock_take_stale(name, STALE_AFTER)? {
                    took_over = Some(old);
                }
            }
            Some(age) => {
                let holder = storage.lock_read(name)?.unwrap_or_default();
                return Ok(Acquired::No(Outcome::Locked { holder, age }));
            }
        }
    }
    let holder = storage.lock_read(name)?.unwrap_or_default();
    let age = storage.lock_age(name)?.unwrap_or_default();
    Ok(Acquired::No(Outcome::Locked { holder, age }))
}

fn failed(step: &str, e: impl std::fmt::Display) -> Outcome {
    Outcome::Failed {
        reason: format!("{step}: {e}"),
    }
}

/// Why a patch could not make its new text.
#[derive(Debug)]
pub enum Refusal {
    Misfit(String),
    NotText(String),
}

/// What a patch makes of the old bytes: the new text and notes for the answer.
pub type Build<'a> = &'a dyn Fn(&[u8]) -> Result<(Vec<u8>, Vec<String>), Refusal>;

/// What the write puts in the file.
enum Plan<'a> {
    /// A whole new text, when the file is as the caller expects. `folders` are
    /// the folders the write makes for a new file.
    Text {
        expect: &'a Expect,
        text: &'a [u8],
        folders: &'a [String],
    },
    /// A text made from the file as it is under the lock. The file must exist.
    Patch(Build<'a>),
}

/// The steps between taking and releasing the lock.
fn change(
    storage: &dyn Storage,
    held: &Held<'_>,
    path: &str,
    plan: &Plan,
    stop: &AtomicBool,
) -> Outcome {
    let old = match storage.read(path) {
        Ok(old) => old,
        Err(e) => return failed("could not read the file", e),
    };
    let old_print = old.as_deref().map(fingerprint);
    let (expect, new, notes, folders) = match plan {
        Plan::Text {
            expect,
            text,
            folders,
        } => {
            match (expect, &old_print) {
                (Expect::Absent, Some(found)) => {
                    return Outcome::Exists {
                        found: found.clone(),
                    };
                }
                (Expect::Fingerprint(want), None) => {
                    return Outcome::Missing {
                        expected: want.clone(),
                    };
                }
                (Expect::Fingerprint(want), Some(found)) if want != found => {
                    return Outcome::Changed {
                        expected: want.clone(),
                        found: found.clone(),
                    };
                }
                _ => {}
            }
            (
                (*expect).clone(),
                text.to_vec(),
                Vec::new(),
                folders.to_vec(),
            )
        }
        Plan::Patch(build) => {
            let (Some(old), Some(print)) = (&old, &old_print) else {
                return Outcome::Missing {
                    expected: String::new(),
                };
            };
            match build(old) {
                // The file must still be the one the patch was applied to.
                Ok((new, notes)) => (Expect::Fingerprint(print.clone()), new, notes, Vec::new()),
                Err(Refusal::Misfit(reason)) => return Outcome::Misfit { reason },
                Err(Refusal::NotText(reason)) => return Outcome::NotText { reason },
            }
        }
    };
    let (expect, new) = (&expect, new.as_slice());

    // The last safe point: from here on, the write runs to its end and its check.
    if stop.load(Ordering::SeqCst) {
        return failed("nothing written", "a stop was asked for");
    }

    // A slow read, of a file not yet on this disk, can outlast the 15 minutes.
    match held.is_ours() {
        Ok(true) => {}
        Ok(false) => {
            return failed(
                "nothing written",
                "another writer came in during this write",
            );
        }
        Err(e) => return failed("could not check that the file is free", e),
    }

    match storage.write_if_unchanged(path, expect, new) {
        Ok(()) => {}
        Err(WriteError::Changed { found }) => {
            return match (expect, found) {
                (Expect::Fingerprint(want), Some(found)) => Outcome::Changed {
                    expected: want.clone(),
                    found,
                },
                (Expect::Fingerprint(want), None) => Outcome::Missing {
                    expected: want.clone(),
                },
                (Expect::Absent, found) => Outcome::Exists {
                    found: found.unwrap_or_default(),
                },
            };
        }
        Err(WriteError::Io(e)) => return failed("could not write the file", e),
    }

    let back = storage.read(path);
    let reason = match &back {
        Ok(Some(back)) if back == new => {
            return Outcome::Written {
                old: old_print,
                new: fingerprint(new),
                size: new.len(),
                old_text: old,
                new_text: new.to_vec(),
                notes,
                folders,
            };
        }
        Ok(Some(back)) => format!(
            "the file read back with fingerprint {}, not {}",
            fingerprint(back),
            fingerprint(new)
        ),
        Ok(None) => "the file was gone when read back".to_string(),
        Err(e) => format!("the file could not be read back: {e}"),
    };

    // Different bytes may be a person's edit that landed right after the move.
    // Keep them first. If they cannot be kept, leave the file alone.
    let mut rescued = None;
    if let Ok(Some(found)) = &back {
        match storage.rescue(path, found) {
            Ok(place) => rescued = Some(place),
            Err(e) => {
                return Outcome::NotRestored {
                    reason: format!(
                        "{reason}; keeping those bytes failed, so the file stays as it is: {e}"
                    ),
                    rescued: None,
                };
            }
        }
    }
    if let Err(e) = storage.restore(path, old.as_deref()) {
        return Outcome::NotRestored {
            reason: format!("{reason}; putting it back failed: {e}"),
            rescued,
        };
    }
    match storage.read(path) {
        Ok(now) if now == old => Outcome::Restored { reason, rescued },
        Ok(_) => Outcome::NotRestored {
            reason: format!("{reason}; the file differs from the old state after putting it back"),
            rescued,
        },
        Err(e) => Outcome::NotRestored {
            reason: format!("{reason}; the file could not be read after putting it back: {e}"),
            rescued,
        },
    }
}

/// One write. `line` is the text of the lock: who and when.
pub fn write(storage: &dyn Storage, path: &str, expect: &Expect, new: &[u8], line: &str) -> Report {
    write_unless_stopped(storage, path, expect, new, line, &AtomicBool::new(false))
}

/// The same, and a stop asked for before the last safe point writes nothing.
pub fn write_unless_stopped(
    storage: &dyn Storage,
    path: &str,
    expect: &Expect,
    new: &[u8],
    line: &str,
    stop: &AtomicBool,
) -> Report {
    write_into(storage, path, expect, new, &[], line, stop)
}

/// The same, for a file that needs new folders: `folders` are the ones missing
/// right before this call, from `new_folders`. The answer names them.
pub fn write_into(
    storage: &dyn Storage,
    path: &str,
    expect: &Expect,
    new: &[u8],
    folders: &[String],
    line: &str,
    stop: &AtomicBool,
) -> Report {
    let plan = Plan::Text {
        expect,
        text: new,
        folders,
    };
    run(storage, path, &plan, line, stop)
}

/// A patch: `build` makes the new text from the file as it is under the lock,
/// and the write goes on as any write, with the same lock, check and read-back.
pub fn patch_unless_stopped(
    storage: &dyn Storage,
    path: &str,
    build: Build,
    line: &str,
    stop: &AtomicBool,
) -> Report {
    run(storage, path, &Plan::Patch(build), line, stop)
}

fn run(storage: &dyn Storage, path: &str, plan: &Plan, line: &str, stop: &AtomicBool) -> Report {
    let run = under_lock(storage, path, line, |held| {
        change(storage, held, path, plan, stop)
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

/// Why the steps under a lock did not run.
pub(crate) enum Gate {
    Busy { holder: String, age: Duration },
    Failed(String),
}

/// What happened around the steps under a lock.
pub(crate) struct Locked<T> {
    pub result: Result<T, Gate>,
    pub took_over: Option<String>,
    pub lock_left: Option<String>,
    pub lock_lost: Option<String>,
}

impl Held<'_> {
    /// True while the lock still holds this caller's own line.
    pub(crate) fn still_ours(&self) -> io::Result<bool> {
        self.is_ours()
    }
}

/// Takes the lock of `path`, runs `steps`, and releases the lock, also on a panic.
/// The write and the delete share it, so both lock in the same way.
pub(crate) fn under_lock<T>(
    storage: &dyn Storage,
    path: &str,
    line: &str,
    steps: impl FnOnce(&Held<'_>) -> T,
) -> Locked<T> {
    let name = lock_name(path);
    let refused = |gate| Locked {
        result: Err(gate),
        took_over: None,
        lock_left: None,
        lock_lost: None,
    };
    let took_over = match acquire(storage, &name, line) {
        Ok(Acquired::Yes { took_over }) => took_over,
        Ok(Acquired::No(Outcome::Locked { holder, age })) => {
            return refused(Gate::Busy { holder, age });
        }
        Ok(Acquired::No(other)) => return refused(Gate::Failed(format!("{other:?}"))),
        Err(e) => return refused(Gate::Failed(e.to_string())),
    };
    let mut held = Held {
        storage,
        name,
        line: line.to_string(),
        done: false,
    };
    let result = steps(&held);
    let (lock_left, lock_lost) = match held.release() {
        Ok(Release::Done) => (None, None),
        Ok(Release::NotOurs(other)) => (
            None,
            Some(other.unwrap_or_else(|| "none left a name".to_string())),
        ),
        Err(e) => (Some(e.to_string()), None),
    };
    Locked {
        result: Ok(result),
        took_over,
        lock_left,
        lock_lost,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new(list: &[&str]) -> Vec<String> {
        list.iter().map(|f| f.to_string()).collect()
    }

    #[test]
    fn the_fixed_folders_refuse_only_a_new_folder_directly_inside() {
        let fixed = " /org , /Archive/";
        assert!(check_fixed(&new(&["org/garden"]), fixed).is_err());
        assert!(check_fixed(&new(&["org/helth", "org/helth/diet"]), fixed).is_err());
        assert!(
            check_fixed(&new(&["Org/Garden"]), fixed).is_err(),
            "capitals do not count"
        );
        assert!(check_fixed(&new(&["archive/2026"]), fixed).is_err());
        assert!(check_fixed(&new(&["org/proposals/items/2026"]), fixed).is_ok());
        assert!(
            check_fixed(&new(&["new"]), fixed).is_ok(),
            "the root is not in the list"
        );
        assert!(check_fixed(&[], fixed).is_ok());
        assert!(
            check_fixed(&new(&["org/garden"]), "").is_ok(),
            "no setting, no guard"
        );
        assert!(check_fixed(&new(&["new"]), "/").is_err(), "/ is the root");
    }
}
