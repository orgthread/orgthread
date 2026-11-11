//! The revert of `orgthread watch`: `r`, then `y`, puts a file back to its text
//! before one change.
//!
//! Only the owner's key press reverts: there is no command and no MCP tool. The
//! revert is an ordinary safe write under the lock, with the "after" fingerprint
//! of the change as what must hold. So it changes nothing when the file changed
//! since, by hand or on the other door. It logs a `reverted` line, which is a
//! change with two fingerprints too, so `r` on it brings the change back.

use crate::fingerprint::fingerprint;
use crate::log;
use crate::storage::{Expect, Storage};
use crate::versions::Versions;
use crate::watch::{self, Entry};
use crate::write::{self, Outcome};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, mpsc};
use std::time::Duration;

/// The setting that names the owner, as `user.name` does for git. A revert is
/// logged as `<owner> (watch)`.
pub const OWNER: &str = "ORGTHREAD_OWNER";
/// The owner when the setting is not there.
pub const DEFAULT_OWNER: &str = "owner";

/// The "who" of a revert in the log: the owner and `(watch)`. A name on more
/// than one line, or with a tab, would break the log, so it is refused.
pub fn who_of(owner: Option<&str>) -> Result<String, String> {
    let owner = owner.map(str::trim).filter(|o| !o.is_empty());
    let owner = owner.unwrap_or(DEFAULT_OWNER);
    if owner.chars().any(char::is_control) {
        return Err(format!("{OWNER} must be a name on one line"));
    }
    Ok(format!("{owner} (watch)"))
}
/// The log word of a revert. `restored` stays the rollback inside a write.
pub const WORD: &str = "reverted";

pub const NO_COPY: &str = "No copy of the old version is kept. Nothing changed.";
pub const NEWER: &str = "A newer change exists. Press l to go to it.";
pub const CHANGED: &str = "The file changed since this change. Nothing changed.";
pub const CANCELLED: &str = "Nothing changed.";
pub const NO_OLDER: &str = "There is no older change of this file in the log.";
pub const NO_NEWER: &str = "There is no newer change of this file in the log.";
pub const BUSY: &str = "A revert runs already. Wait for its answer.";
/// How long the screen waits for a revert on Dropbox: a write there is a lock,
/// a read, an upload, a read back and the release, each a call of its own.
pub const WAIT: Duration = Duration::from_secs(60);
pub const TIMEOUT: &str = "Dropbox did not answer in 60 seconds. The revert can still finish: the log shows it when it does.";

/// True for a line that `r` can undo: a write, a patch or a revert of a file
/// that existed before, so a line with both fingerprints. A new file has no
/// "before", and a delete has no "after".
pub fn can_revert(entry: &Entry) -> bool {
    matches!(entry.outcome.as_str(), "written" | "patched" | WORD)
        && !entry.before.is_empty()
        && !entry.after.is_empty()
        && !entry.delete
        && entry.copy_from.is_none()
        && entry.move_from.is_none()
}

/// True for a line that changed its file: what `h` and `l` walk, and what makes
/// a line older than the newest. A refusal changed nothing.
pub fn is_change(entry: &Entry) -> bool {
    watch::is_written(&entry.outcome)
        || matches!(entry.outcome.as_str(), "deleted" | "copied" | "moved")
}

/// Dropbox does not count capitals in a path, and the Mac does not either.
fn same_path(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// The next older change of the same path than the line at `index`, in a list
/// with the oldest first. A move starts the history of its new name, so a walk
/// never leaves the name.
pub fn older(entries: &[Entry], index: usize) -> Option<usize> {
    let path = &entries.get(index)?.path;
    (0..index)
        .rev()
        .find(|&i| is_change(&entries[i]) && same_path(&entries[i].path, path))
}

/// The next newer change of the same path than the line at `index`.
pub fn newer(entries: &[Entry], index: usize) -> Option<usize> {
    let path = &entries.get(index)?.path;
    (index + 1..entries.len())
        .find(|&i| is_change(&entries[i]) && same_path(&entries[i].path, path))
}

/// The lines a revert takes away and puts back: the diff from the text after
/// the change to the text before it.
pub fn counts(before: &str, after: &str) -> (usize, usize) {
    let rows = watch::diff_rows(after, before);
    let removed = rows
        .iter()
        .filter(|r| matches!(r, watch::Row::Removed { .. }))
        .count();
    let added = rows
        .iter()
        .filter(|r| matches!(r, watch::Row::Added { .. }))
        .count();
    (removed, added)
}

/// The time of a log line in the local zone, to the minute.
pub fn local_time(time: &str, zone: &jiff::tz::TimeZone) -> String {
    watch::seconds_of(time)
        .and_then(|s| jiff::Timestamp::from_second(i64::try_from(s).ok()?).ok())
        .map(|t| {
            t.to_zoned(zone.clone())
                .strftime("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| time.to_string())
}

/// What the revert question shows before anything changes, as a popup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    pub path: String,
    /// The word of the change: written, patched or reverted.
    pub outcome: String,
    /// When the change was made, in the local zone.
    pub time: String,
    /// Who made the change: the "who" of its log line.
    pub who: String,
    /// The lines the revert takes away and puts back, when both texts are known.
    pub counts: Option<(usize, usize)>,
}

impl Question {
    /// The size of the revert in plain words.
    pub fn size(&self) -> String {
        match self.counts {
            Some((removed, added)) => format!("-{removed} +{added} lines"),
            None => "not known: the copy of the newer text is gone".to_string(),
        }
    }
}

/// The question for a line.
pub fn question(
    entry: &Entry,
    counts: Option<(usize, usize)>,
    zone: &jiff::tz::TimeZone,
) -> Question {
    Question {
        path: entry.path.clone(),
        outcome: entry.outcome.clone(),
        time: local_time(&entry.time, zone),
        who: entry.who.clone(),
        counts,
    }
}

/// What one `y` writes: the text before the change, where the text after it
/// must still be.
#[derive(Clone, Debug)]
pub struct Plan {
    pub path: String,
    /// The fingerprint before the change: what the file gets again.
    pub before: String,
    /// The fingerprint after the change: what the file must hold now.
    pub after: String,
    /// The bytes with the fingerprint `before`.
    pub old: Vec<u8>,
}

/// The revert itself: the safe write, then the log line, then on the Mac the
/// two copies for the diff screen. The answer is the text for the screen.
pub fn revert(
    storage: &dyn Storage,
    plan: &Plan,
    who: &str,
    log_file: &Path,
    versions: Option<&Versions>,
) -> Result<String, String> {
    // A copy is always checked against its fingerprint before it is written.
    if fingerprint(&plan.old) != plan.before {
        return Err(NO_COPY.to_string());
    }
    let now = log::utc_now();
    let lock_line = format!("orgthread {who} {now} pid {}", std::process::id());
    let report = write::write_into(
        storage,
        &plan.path,
        &Expect::Fingerprint(plan.after.clone()),
        &plan.old,
        &[],
        &lock_line,
        &AtomicBool::new(false),
    );
    let (word, before, after, size) = match &report.outcome {
        Outcome::Written { old, new, size, .. } => (
            WORD,
            old.clone().unwrap_or_default(),
            new.clone(),
            size.to_string(),
        ),
        Outcome::Changed { expected, found } => {
            ("changed", expected.clone(), found.clone(), String::new())
        }
        other => (other.word(), String::new(), String::new(), String::new()),
    };
    let line = log::line(&[&now, word, &plan.path, who, &before, &after, &size]);
    // First the log, as for every write.
    let logged = log::append(log_file, &line);
    let path = &plan.path;
    let mut answer = match &report.outcome {
        Outcome::Written {
            old_text, new_text, ..
        } => {
            let mut text = format!(
                "Reverted {path}. Its new line is the newest change: r on it brings the change back."
            );
            if let Some(versions) = versions
                && let Err(e) = versions.keep(old_text.as_deref(), new_text)
            {
                text.push_str(&format!(" No copy was kept for the diff screen: {e}."));
            }
            Ok(text)
        }
        Outcome::Changed { .. } | Outcome::Missing { .. } | Outcome::Exists { .. } => {
            Err(CHANGED.to_string())
        }
        Outcome::Locked { holder, .. } => Err(format!(
            "Another writer holds {path} now: {holder}. Nothing changed. Try again in a moment."
        )),
        Outcome::Restored { reason, .. } => Err(format!(
            "The check after the revert failed: {reason}. The old state is back, so nothing changed."
        )),
        Outcome::NotRestored { reason, .. } => Err(format!(
            "DAMAGE POSSIBLE. The check after the revert failed: {reason}. Look at {path} now."
        )),
        Outcome::Failed { reason } => Err(format!("The revert failed: {reason}. Nothing changed.")),
        Outcome::Misfit { reason } | Outcome::NotText { reason } => {
            Err(format!("{reason}. Nothing changed."))
        }
    };
    let mut extra = String::new();
    if let Err(e) = logged {
        extra.push_str(&format!(
            " The log line failed, {}: {e}. The line: {line}",
            log_file.display()
        ));
    }
    if let Some(other) = &report.lock_lost {
        extra.push_str(&format!(
            " Another writer may have been at work during the revert. Check {path}. That writer: {other}"
        ));
    }
    if let Some(reason) = &report.lock_left {
        extra.push_str(&format!(
            " The clean-up of the lock did not finish: {reason}. {path} is free again within 15 minutes."
        ));
    }
    if !extra.is_empty() {
        match &mut answer {
            Ok(text) | Err(text) => text.push_str(&extra),
        }
    }
    answer
}

/// Opens the storage for one revert: a fresh link each time, so that a token
/// never goes stale while the screen stays open for hours.
pub type OpenStorage = Arc<dyn Fn() -> Result<Box<dyn Storage>, String> + Send + Sync>;

/// What the screen calls on `y`.
pub struct Reverter {
    open: OpenStorage,
    /// The "who" of the log lines, from `who_of`.
    who: String,
    log: PathBuf,
    /// The folder of the copies on the Mac. A server keeps none.
    versions: Option<PathBuf>,
    /// On a server the revert runs in a thread of its own, so that the screen
    /// never freezes. On the Mac it is one fast local write.
    background: bool,
}

impl Reverter {
    pub fn new(
        open: OpenStorage,
        who: String,
        log: PathBuf,
        versions: Option<PathBuf>,
        background: bool,
    ) -> Reverter {
        Reverter {
            open,
            who,
            log,
            versions,
            background,
        }
    }

    /// Starts one revert. The answer comes through the channel.
    pub fn start(&self, plan: Plan) -> mpsc::Receiver<Result<String, String>> {
        let (send, receive) = mpsc::channel();
        let open = Arc::clone(&self.open);
        let log = self.log.clone();
        let who = self.who.clone();
        let versions = self.versions.clone();
        let work = move || {
            let result = match open() {
                Ok(storage) => revert(
                    storage.as_ref(),
                    &plan,
                    &who,
                    &log,
                    versions.map(Versions::new).as_ref(),
                ),
                Err(e) => Err(format!(
                    "Could not reach the storage: {}. Nothing changed.",
                    crate::history::clean(&e)
                )),
            };
            let _ = send.send(result);
        };
        if self.background {
            std::thread::spawn(work);
        } else {
            work();
        }
        receive
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::LocalFolder;
    use crate::watch::parse_line;
    use std::fs;

    pub fn line(time: &str, outcome: &str, path: &str, before: &str, after: &str) -> Entry {
        parse_line(&format!(
            "{time}\t{outcome}\t{path}\tinvented chat\t{before}\t{after}\t3"
        ))
        .unwrap()
    }

    #[test]
    fn only_a_change_with_both_fingerprints_can_be_reverted() {
        let t = "2026-11-10 09:14:00Z";
        assert!(can_revert(&line(t, "written", "org/a.md", "aa", "bb")));
        assert!(can_revert(&line(t, "patched", "org/a.md", "aa", "bb")));
        assert!(can_revert(&line(t, "reverted", "org/a.md", "aa", "bb")));
        assert!(
            !can_revert(&line(t, "written", "org/a.md", "-", "bb")),
            "a new file"
        );
        assert!(
            !can_revert(&line(t, "deleted", "org/a.md", "aa", "-")),
            "a delete"
        );
        assert!(
            !can_revert(&line(t, "changed", "org/a.md", "aa", "bb")),
            "a refusal"
        );
        assert!(!can_revert(&line(t, "restored", "org/a.md", "aa", "bb")));
    }

    #[test]
    fn the_walk_stays_on_one_path_and_skips_refusals() {
        let t = "2026-11-10 09:14:00Z";
        let entries = vec![
            line(t, "written", "org/a.md", "-", "a1"),
            line(t, "written", "org/b.md", "-", "b1"),
            line(t, "changed", "org/a.md", "a0", "a1"),
            line(t, "patched", "ORG/a.md", "a1", "a2"),
            line(t, "written", "org/b.md", "b1", "b2"),
        ];
        assert_eq!(newer(&entries, 0), Some(3));
        assert_eq!(older(&entries, 3), Some(0));
        assert_eq!(older(&entries, 0), None);
        assert_eq!(newer(&entries, 3), None);
        assert_eq!(newer(&entries, 1), Some(4));
        assert_eq!(older(&entries, 2), Some(0), "from a refusal too");
    }

    #[test]
    fn the_counts_are_what_the_revert_takes_away_and_puts_back() {
        let before = "one\ntwo\n";
        let after = "one\n2\nthree\nfour\n";
        assert_eq!(counts(before, after), (3, 1));
    }

    #[test]
    fn the_question_names_the_path_the_time_the_who_and_the_size() {
        let e = line(
            "2026-11-10 09:14:26Z",
            "patched",
            "org/health/log.md",
            "aa",
            "bb",
        );
        let q = question(&e, Some((3, 1)), &jiff::tz::TimeZone::UTC);
        assert_eq!(q.path, "org/health/log.md");
        assert_eq!(q.outcome, "patched");
        assert_eq!(q.time, "2026-11-10 09:14");
        assert_eq!(q.who, "invented chat");
        assert_eq!(q.size(), "-3 +1 lines");
        let vienna = jiff::tz::TimeZone::get("Europe/Vienna").unwrap();
        let q = question(&e, None, &vienna);
        assert_eq!(q.time, "2026-11-10 10:14", "the local zone");
        assert!(q.size().starts_with("not known"));
    }

    #[test]
    fn the_owner_comes_from_its_setting_and_has_a_default() {
        assert_eq!(who_of(None).unwrap(), "owner (watch)");
        assert_eq!(who_of(Some("  ")).unwrap(), "owner (watch)");
        assert_eq!(who_of(Some(" Ana ")).unwrap(), "Ana (watch)");
        assert!(who_of(Some("Ana\tB")).is_err());
        assert!(who_of(Some("Ana\nB")).is_err());
    }

    fn place(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("orgthread-revert-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("root/.locks")).unwrap();
        fs::create_dir_all(dir.join("root/org")).unwrap();
        dir
    }

    #[test]
    fn a_copy_with_a_wrong_fingerprint_is_never_written() {
        let dir = place("wrong");
        let storage = LocalFolder::new(dir.join("root"), ".locks");
        fs::write(dir.join("root/org/a.md"), "new\n").unwrap();
        let plan = Plan {
            path: "org/a.md".into(),
            before: fingerprint(b"old\n"),
            after: fingerprint(b"new\n"),
            old: b"other\n".to_vec(),
        };
        let log = dir.join("writes.log");
        assert_eq!(
            revert(&storage, &plan, "owner (watch)", &log, None).unwrap_err(),
            NO_COPY
        );
        assert_eq!(
            fs::read_to_string(dir.join("root/org/a.md")).unwrap(),
            "new\n"
        );
        assert!(!log.exists(), "nothing was tried, so nothing is logged");
        fs::remove_dir_all(&dir).unwrap();
    }
}
