//! The write procedure: take the lock, compare, write, read back, release.

use crate::fingerprint::fingerprint;
use crate::storage::{Expect, Storage, WriteError};
use std::io;
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

#[derive(Debug)]
pub enum Outcome {
    Written {
        old: Option<String>,
        new: String,
        size: usize,
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
struct Held<'a> {
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

/// The steps between taking and releasing the lock.
fn change(storage: &dyn Storage, held: &Held, path: &str, expect: &Expect, new: &[u8]) -> Outcome {
    let old = match storage.read(path) {
        Ok(old) => old,
        Err(e) => return failed("could not read the file", e),
    };
    let old_print = old.as_deref().map(fingerprint);
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

    // A slow read, of a file not yet on this disk, can outlast the 15 minutes.
    match held.is_ours() {
        Ok(true) => {}
        Ok(false) => return failed("nothing written", "this write lost its lock while reading"),
        Err(e) => return failed("could not check the lock", e),
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
    let name = lock_name(path);
    let refused = |outcome| Report {
        outcome,
        took_over: None,
        lock_left: None,
        lock_lost: None,
    };
    let took_over = match acquire(storage, &name, line) {
        Ok(Acquired::Yes { took_over }) => took_over,
        Ok(Acquired::No(outcome)) => return refused(outcome),
        Err(e) => return refused(failed("could not take the lock", e)),
    };
    let mut held = Held {
        storage,
        name,
        line: line.to_string(),
        done: false,
    };
    let outcome = change(storage, &held, path, expect, new);
    let (lock_left, lock_lost) = match held.release() {
        Ok(Release::Done) => (None, None),
        Ok(Release::NotOurs(other)) => (
            None,
            Some(other.unwrap_or_else(|| "the lock is gone".to_string())),
        ),
        Err(e) => (Some(e.to_string()), None),
    };
    Report {
        outcome,
        took_over,
        lock_left,
        lock_lost,
    }
}
