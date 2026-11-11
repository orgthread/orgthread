//! What the screen shows: the locks and the log. This part only reads, and it
//! knows nothing about the terminal. The revert is in `revert.rs`.

use crate::versions::Versions;
use similar::{ChangeTag, TextDiff};
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How much of the end of the log is read.
const LOG_TAIL: u64 = 256 * 1024;
/// A lock older than this is stale, as in the write procedure.
const STALE: Duration = crate::write::STALE_AFTER;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub time: String,
    pub outcome: String,
    pub path: String,
    pub who: String,
    pub before: String,
    pub after: String,
    pub size: String,
    /// The line comes from a delete: a `deleted`, or a refusal with the mark.
    pub delete: bool,
    /// The line comes from a copy: its 8th field, the source, starts with `/`.
    /// This is the source, with no `/` at its start.
    pub copy_from: Option<String>,
    /// The line comes from a move: its 8th field, the old name, starts with `/`,
    /// and its word starts with `move`. This is the old name, with no `/` at its
    /// start. A move line has no `copy_from`.
    pub move_from: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lock {
    /// The file it protects, with each `--` of the lock name shown as `/`.
    pub path: String,
    pub holder: String,
    pub age: Duration,
    pub stale: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub locks: Vec<Lock>,
    /// Temporary and rescue files that a write left in the lock folder.
    pub left_behind: Vec<String>,
    /// Oldest first.
    pub entries: Vec<Entry>,
    /// What could not be read, to show on the screen.
    pub problems: Vec<String>,
}

/// One log line, or `None` when it does not have the seven fields. An 8th field
/// is the rev of a delete, the mark of a refused delete, or the source of a copy
/// or the old name of a move, which start with `/`. The word of a move starts
/// with `move`.
pub fn parse_line(line: &str) -> Option<Entry> {
    let f: Vec<&str> = line.split('\t').collect();
    let (eighth, &[time, outcome, path, who, before, after, size, ..]) = (f.get(7), &f[..]) else {
        return None;
    };
    if f.len() > 8 {
        return None;
    }
    let from = eighth
        .and_then(|field| field.strip_prefix('/'))
        .map(str::to_string);
    // A copy or move line has a `/` at the start of its 8th field, so it never matches.
    let delete = outcome == "deleted" || eighth == Some(&crate::delete::REFUSED_MARK);
    let clean = |v: &str| {
        if v == "-" {
            String::new()
        } else {
            v.to_string()
        }
    };
    Some(Entry {
        time: time.to_string(),
        outcome: outcome.to_string(),
        path: path.to_string(),
        who: who.to_string(),
        before: clean(before),
        after: clean(after),
        size: clean(size),
        delete,
        copy_from: from.clone().filter(|_| !crate::mv::is_move_word(outcome)),
        move_from: from.filter(|_| crate::mv::is_move_word(outcome)),
    })
}

fn read_tail(log: &Path) -> io::Result<String> {
    let mut file = fs::File::open(log)?;
    let length = file.metadata()?.len();
    let start = length.saturating_sub(LOG_TAIL);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    // After a cut, the first line is only a part of a line.
    Ok(match (start > 0, text.split_once('\n')) {
        (true, Some((_, rest))) => rest.to_string(),
        _ => text.into_owned(),
    })
}

pub fn snapshot(lock_dir: &Path, log: &Path) -> Snapshot {
    let mut snap = Snapshot::default();
    match read_tail(log) {
        Ok(text) => snap.entries = text.lines().filter_map(parse_line).collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => snap
            .problems
            .push(format!("the log {}: {e}", log.display())),
    }
    match fs::read_dir(lock_dir) {
        Ok(dir) => {
            for item in dir.flatten() {
                let name = item.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue;
                }
                match name.strip_suffix(".lock") {
                    Some(stem) => {
                        let holder = fs::read(item.path())
                            .map(|b| String::from_utf8_lossy(&b).trim_end().to_string())
                            .unwrap_or_default();
                        let age = item
                            .metadata()
                            .and_then(|m| m.modified())
                            .ok()
                            .and_then(|t| t.elapsed().ok())
                            .unwrap_or_default();
                        snap.locks.push(Lock {
                            path: stem.replace("--", "/"),
                            holder,
                            age,
                            stale: age > STALE,
                        });
                    }
                    None => snap.left_behind.push(name),
                }
            }
        }
        Err(e) => snap
            .problems
            .push(format!("the lock folder {}: {e}", lock_dir.display())),
    }
    snap.locks.sort_by(|a, b| a.path.cmp(&b.path));
    snap.left_behind.sort();
    snap
}

/// Seconds since 1970 for a log time such as `2026-11-07 08:10:05Z`.
pub fn seconds_of(time: &str) -> Option<u64> {
    let t = time.strip_suffix('Z')?;
    let (date, clock) = t.split_once(' ')?;
    let mut d = date.split('-').map(str::parse::<i64>);
    let (y, m, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let mut c = clock.split(':').map(str::parse::<i64>);
    let (h, min, s) = (c.next()?.ok()?, c.next()?.ok()?, c.next()?.ok()?);
    // Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + h * 3_600 + min * 60 + s).ok()
}

/// A short age: `12 s`, `3 min`, `2 h`, `4 d`.
pub fn short_age(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds} s"),
        60..3_600 => format!("{} min", seconds / 60),
        3_600..86_400 => format!("{} h", seconds / 3_600),
        _ => format!("{} d", seconds / 86_400),
    }
}

/// How long ago a log time was, or a question mark if it cannot be read.
pub fn ago(time: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    match seconds_of(time) {
        Some(then) => short_age(now.saturating_sub(then)),
        None => "?".to_string(),
    }
}

/// True for an outcome that wrote a new text: a write, a patch or a revert.
pub fn is_written(outcome: &str) -> bool {
    outcome == "written" || outcome == "patched" || outcome == crate::revert::WORD
}

/// The reason in plain words, for this entry: a delete, a copy and a move have
/// their own texts.
pub fn why_of(entry: &Entry) -> &'static str {
    if entry.move_from.is_some() {
        why_move(&entry.outcome)
    } else if entry.copy_from.is_some() {
        why_copy(&entry.outcome)
    } else if entry.delete {
        why_delete(&entry.outcome)
    } else {
        why(&entry.outcome)
    }
}

/// The reason of a delete line in plain words.
pub fn why_delete(outcome: &str) -> &'static str {
    match outcome {
        "deleted" => why("deleted"),
        "not-allowed" => "The file is outside the folders open for deletes. Nothing was deleted.",
        "not-read" => {
            "A delete came from a name that had not read the file, with no fingerprint. Nothing was deleted."
        }
        "changed" => "The file changed after this chat saw it. Nothing was deleted.",
        "missing" => "This chat asked to delete a file that is not there. Nothing was deleted.",
        "folder" => "A delete named a folder, and only a file is deleted. Nothing was deleted.",
        "locked" => "Another writer held the lock. Nothing was deleted.",
        "not-restored" => "The delete ended in an unexpected state. Look at this file now.",
        "failed" => "The delete failed, and the file is unchanged.",
        _ => "This outcome is not known to this version.",
    }
}

/// The reason of a copy line in plain words.
pub fn why_copy(outcome: &str) -> &'static str {
    match outcome {
        "copied" => why("copied"),
        "exists" => "A file or folder is at the new name already. Nothing was copied.",
        "missing" => "There is no such source. Nothing was copied.",
        "folder" => "The source is a folder, and only a file is copied. Nothing was copied.",
        "changed" => "The source is not the version that was expected. Nothing was copied.",
        "not-allowed" => "The source or the new name is closed to this door. Nothing was copied.",
        "not-text" => {
            "The source is not text, and the new name is outside the folders open for such files. Nothing was copied."
        }
        "locked" => "Another writer held the lock of the new name. Nothing was copied.",
        "removed" => "The copy differed from the source, and it was removed again.",
        "not-removed" => {
            "The copy differed from the source, and it could not be removed. Look at this file now."
        }
        "failed" => "The copy failed. Nothing was copied, unless the message says otherwise.",
        _ => "This outcome is not known to this version.",
    }
}

/// The reason of a move line in plain words.
pub fn why_move(outcome: &str) -> &'static str {
    match outcome {
        "moved" => {
            "The file was moved to this new name, and checked there. The way back is a move to its old name."
        }
        "move-exists" => "A file or folder is at the new name already. Nothing was moved.",
        "move-missing" => "There is no file at the old name. Nothing was moved.",
        "move-folder" => "The old name is a folder, and only a file is moved. Nothing was moved.",
        "move-changed" => "The file is not the version that was expected. Nothing was moved.",
        "move-not-allowed" => {
            "The old or the new name is closed to this door, or outside the folders open for moves. Nothing was moved."
        }
        "move-not-text" => {
            "The file is not text, and the new name is outside the folders open for such files. Nothing was moved."
        }
        "move-locked" => "Another writer held the lock of one of the two names. Nothing was moved.",
        "moved-back" => "The file changed during the move, so it was moved back to its old name.",
        "move-unexpected" => {
            "The move ended in an unexpected state. Look at the old and the new name now."
        }
        "move-failed" => "The move failed. Nothing was moved, unless the message says otherwise.",
        _ => "This outcome is not known to this version.",
    }
}

/// The reason in plain words.
pub fn why(outcome: &str) -> &'static str {
    match outcome {
        "written" => "The file was written and read back. Every byte matches.",
        "patched" => "The patch was applied, written and read back. Every byte matches.",
        "misfit" => "An edit of the patch did not fit the file. Nothing was written.",
        "not-text" => {
            "The patch would have made a text that is not plain text. Nothing was written."
        }
        "not-read" => "A patch came from a name that had not read the file. Nothing was written.",
        "locked" => "Another writer held the lock. Nothing was written.",
        "changed" => "The file changed after this chat read it. Nothing was written.",
        "exists" => "A new file was asked for, but a file is there. Nothing was written.",
        "missing" => "This chat expected a file, but none is there. Nothing was written.",
        "reverted" => {
            "The file got its text from before an earlier change back, after r and y in watch. Every byte matches. r on this line brings that change back."
        }
        "restored" => "The check after the write failed. The old state is back.",
        "not-restored" => "The check failed and the old state is NOT back. Look at this file now.",
        "failed" => "The write failed, and the file is unchanged.",
        "downloaded" => "This chat got the exact bytes of the file. Nothing was written.",
        "copied" => "A new file was made with the exact bytes of the source.",
        "made-folder" => "A missing folder was made for the file on the next line.",
        "deleted" => {
            "The file was deleted. Dropbox keeps it in Deleted files, or the last version is kept beside the program."
        }
        _ => "This outcome is not known to this version.",
    }
}

/// One row of a diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    Same {
        line: usize,
        text: String,
    },
    Removed {
        line: usize,
        text: String,
    },
    Added {
        line: usize,
        text: String,
    },
    /// Unchanged lines left out between two changes.
    Gap,
}

/// What changed from one text to the next, with 3 lines around each change. A
/// line with no newline at its end says so, as that is a real change too.
pub fn diff_rows(old: &str, new: &str) -> Vec<Row> {
    let diff = TextDiff::from_lines(old, new);
    let mut rows = Vec::new();
    for (i, group) in diff.grouped_ops(3).iter().enumerate() {
        if i > 0 {
            rows.push(Row::Gap);
        }
        for op in group {
            for change in diff.iter_changes(op) {
                let mut text = change.value().trim_end_matches('\n').to_string();
                if change.missing_newline() {
                    text.push_str("  (no newline at the end)");
                }
                let line = change.new_index().or(change.old_index()).unwrap_or(0) + 1;
                rows.push(match change.tag() {
                    ChangeTag::Equal => Row::Same { line, text },
                    ChangeTag::Delete => Row::Removed {
                        line: change.old_index().unwrap_or(0) + 1,
                        text,
                    },
                    ChangeTag::Insert => Row::Added { line, text },
                });
            }
        }
    }
    rows
}

/// What the diff screen shows for one log entry: rows, or the reason there are none.
pub fn diff_of(entry: &Entry, versions: &Versions) -> Result<Vec<Row>, String> {
    if !is_written(&entry.outcome) {
        return Err(format!("{} There is nothing to compare.", why_of(entry)));
    }
    if !versions.keeps_copies() {
        return Err(
            "This server keeps no copies of what it writes. Dropbox keeps the old versions in its history."
                .to_string(),
        );
    }
    const NO_COPY: &str = "No copy of this version. Writes from before the diff screen have none, and copies go after 30 days.";
    let load = |print: &str| match versions.load(print) {
        Ok(Some(bytes)) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
        Ok(None) => Err(NO_COPY.to_string()),
        Err(e) => Err(format!("Could not read the copy: {e}")),
    };
    let new = load(&entry.after)?;
    let old = if entry.before.is_empty() {
        String::new()
    } else {
        load(&entry.before)?
    };
    let rows = diff_rows(&old, &new);
    if rows.is_empty() {
        return Err("The text did not change in this write.".to_string());
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_line_parses_and_a_dash_means_empty() {
        let e = parse_line("2026-11-07 07:28:26Z\twritten\torg/a.md\tchat\t-\tabc\t170").unwrap();
        assert_eq!(e.outcome, "written");
        assert_eq!(e.before, "");
        assert_eq!(e.after, "abc");
        assert_eq!(e.size, "170");
    }

    #[test]
    fn a_made_folder_line_parses_and_says_why() {
        let line =
            "2026-11-10 06:00:00Z\tmade-folder\torg/proposals/items/2026/11\tcareful run\t-\t-\t-";
        let e = parse_line(line).unwrap();
        assert_eq!(e.outcome, "made-folder");
        assert_eq!(e.path, "org/proposals/items/2026/11");
        assert_eq!(e.who, "careful run");
        assert_eq!(
            (e.before.as_str(), e.after.as_str(), e.size.as_str()),
            ("", "", "")
        );
        assert!(e.copy_from.is_none() && e.move_from.is_none() && !e.delete);
        assert_eq!(
            why_of(&e),
            "A missing folder was made for the file on the next line."
        );
        assert!(!is_written(&e.outcome), "a folder has no diff to show");
    }

    #[test]
    fn a_diff_shows_the_change_with_three_lines_around_it() {
        let old = "1\n2\n3\n4\n5\n6\n7\n8\n";
        let new = "1\n2\n3\n4\nfive\n6\n7\n8\n";
        let rows = diff_rows(old, new);
        assert_eq!(rows.len(), 8);
        assert_eq!(
            rows[0],
            Row::Same {
                line: 2,
                text: "2".into()
            }
        );
        assert_eq!(
            rows[3],
            Row::Removed {
                line: 5,
                text: "5".into()
            }
        );
        assert_eq!(
            rows[4],
            Row::Added {
                line: 5,
                text: "five".into()
            }
        );
        assert_eq!(
            rows[7],
            Row::Same {
                line: 8,
                text: "8".into()
            }
        );
    }

    #[test]
    fn two_far_changes_have_a_gap_between_them() {
        let old: String = (1..=20).map(|n| format!("{n}\n")).collect();
        let new = old
            .replace("2\n3", "two\n3")
            .replace("\n19\n", "\nnineteen\n");
        assert!(diff_rows(&old, &new).contains(&Row::Gap));
    }

    #[test]
    fn a_missing_last_newline_shows() {
        let rows = diff_rows("a\n", "a");
        assert_eq!(
            rows[0],
            Row::Removed {
                line: 1,
                text: "a".into()
            }
        );
        assert_eq!(
            rows[1],
            Row::Added {
                line: 1,
                text: "a  (no newline at the end)".into()
            }
        );
    }

    fn written(before: &str, after: &str) -> Entry {
        parse_line(&format!(
            "2026-11-07 07:28:26Z\twritten\torg/a.md\tchat\t{before}\t{after}\t3"
        ))
        .unwrap()
    }

    #[test]
    fn diff_of_an_entry_uses_its_copies() {
        use crate::fingerprint::fingerprint;
        let dir = std::env::temp_dir().join(format!("orgthread-diff-{}", std::process::id()));
        let v = Versions::new(&dir);
        v.save(b"old\n").unwrap();
        v.save(b"new\n").unwrap();
        let (o, n) = (fingerprint(b"old\n"), fingerprint(b"new\n"));

        let rows = diff_of(&written(&o, &n), &v).unwrap();
        assert_eq!(
            rows,
            [
                Row::Removed {
                    line: 1,
                    text: "old".into()
                },
                Row::Added {
                    line: 1,
                    text: "new".into()
                },
            ]
        );

        // A new file: every line is added.
        let rows = diff_of(&written("-", &n), &v).unwrap();
        assert_eq!(
            rows,
            [Row::Added {
                line: 1,
                text: "new".into()
            }]
        );

        // Nothing changed.
        assert!(diff_of(&written(&n, &n), &v).is_err());

        // A write from before the copies.
        let e = diff_of(&written(&o, &fingerprint(b"gone")), &v).unwrap_err();
        assert!(e.contains("No copy"), "{e}");

        // A refusal has nothing to compare.
        let mut refused = written(&o, &n);
        refused.outcome = "changed".to_string();
        let e = diff_of(&refused, &v).unwrap_err();
        assert!(e.contains("Nothing was written"), "{e}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_diff_on_a_server_says_why_there_is_no_copy() {
        let e = diff_of(&written("-", "abc"), &Versions::none()).unwrap_err();
        assert!(e.contains("keeps no copies"), "{e}");
        assert!(!e.contains("30 days"), "{e}");
    }

    #[test]
    fn a_reverted_line_says_why_and_has_a_diff() {
        use crate::fingerprint::fingerprint;
        let e = parse_line("2026-11-11 08:00:00Z\treverted\torg/a.md\towner (watch)\tbb\taa\t3")
            .unwrap();
        assert!(why_of(&e).contains("r on this line brings that change back"));
        assert_ne!(
            why("reverted"),
            why("restored"),
            "restored stays the rollback"
        );
        let dir = std::env::temp_dir().join(format!("orgthread-reverted-{}", std::process::id()));
        let v = Versions::new(&dir);
        v.save(b"new\n").unwrap();
        v.save(b"old\n").unwrap();
        let mut e = written(&fingerprint(b"new\n"), &fingerprint(b"old\n"));
        e.outcome = "reverted".into();
        assert!(diff_of(&e, &v).is_ok());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_broken_line_is_skipped() {
        assert_eq!(parse_line("not a log line"), None);
        let nine = "T\tdeleted\torg/a.md\tchat\taa\t-\t12\t015f\textra";
        assert_eq!(parse_line(nine), None);
    }

    #[test]
    fn a_delete_line_with_its_rev_as_the_8th_field_parses() {
        let e =
            parse_line("2026-11-09 11:55:02Z\tdeleted\torg/a.md\tchat\taa\t-\t12\t015f").unwrap();
        assert_eq!(e.outcome, "deleted");
        assert_eq!((e.before.as_str(), e.after.as_str()), ("aa", ""));
        assert_eq!(e.size, "12");
        assert!(!is_written(&e.outcome), "a delete has no diff to show");
        assert!(why("deleted").starts_with("The file was deleted"));
    }

    #[test]
    fn a_refused_delete_is_named_as_a_delete() {
        let line = "2026-11-09 11:55:02Z\tchanged\torg/a.md\tchat\taa\tbb\t-\tdelete";
        let e = parse_line(line).unwrap();
        assert!(e.delete);
        assert_eq!(
            why_of(&e),
            "The file changed after this chat saw it. Nothing was deleted."
        );
        let e = parse_line(&line.replace("changed", "not-allowed")).unwrap();
        assert!(
            why_of(&e).ends_with("Nothing was deleted."),
            "{}",
            why_of(&e)
        );
        // A refused write keeps its own text.
        let write = "2026-11-09 11:55:02Z\tchanged\torg/a.md\tchat\taa\tbb\t-";
        let e = parse_line(write).unwrap();
        assert!(!e.delete);
        assert!(why_of(&e).ends_with("Nothing was written."));
        // Every refusal word of a delete has a delete text.
        for word in ["not-read", "missing", "folder", "locked", "failed"] {
            assert!(why_delete(word).contains("delete"), "{word}");
        }
    }

    #[test]
    fn a_copy_line_is_never_a_delete_also_with_a_source_named_delete() {
        let line = "2026-11-09 11:55:02Z\tcopied\torg/b.md\tchat\t-\taa\t12\t/org/a.md";
        let e = parse_line(line).unwrap();
        assert_eq!(e.copy_from.as_deref(), Some("org/a.md"));
        assert!(!e.delete);
        assert_eq!(why_of(&e), why("copied"));
        assert!(!is_written(&e.outcome), "a copy has no diff to show");
        // A source named delete, refused or copied, stays a copy.
        for word in [
            "copied",
            "exists",
            "missing",
            "changed",
            "not-allowed",
            "failed",
        ] {
            let line = format!("T\t{word}\torg/b.md\tchat\t-\t-\t-\t/delete");
            let e = parse_line(&line).unwrap();
            assert!(!e.delete, "{word}");
            assert_eq!(e.copy_from.as_deref(), Some("delete"));
            assert!(!why_of(&e).contains("deleted"), "{word}: {}", why_of(&e));
        }
        assert!(why_copy("removed").contains("removed again"));
        assert!(why_copy("not-removed").contains("could not be removed"));
        // Every refusal word of a copy has a copy text.
        for word in [
            "exists",
            "missing",
            "folder",
            "changed",
            "not-allowed",
            "not-text",
            "locked",
        ] {
            assert!(why_copy(word).ends_with("Nothing was copied."), "{word}");
        }
        // The delete lines stay delete lines.
        assert!(
            parse_line("T\tmissing\torg/a.md\tchat\t-\t-\t-\tdelete")
                .unwrap()
                .delete
        );
        assert!(
            parse_line("T\tdeleted\torg/a.md\tchat\taa\t-\t12\t015f")
                .unwrap()
                .delete
        );
    }

    #[test]
    fn log_times_turn_into_seconds() {
        assert_eq!(seconds_of("1970-01-01 00:00:00Z"), Some(0));
        assert_eq!(seconds_of("2000-02-29 00:00:00Z"), Some(951_782_400));
        assert_eq!(seconds_of("2026-11-07 07:28:26Z"), Some(1_794_036_506));
        assert_eq!(seconds_of("no time"), None);
    }

    #[test]
    fn ages_read_short() {
        assert_eq!(short_age(12), "12 s");
        assert_eq!(short_age(180), "3 min");
        assert_eq!(short_age(7_200), "2 h");
        assert_eq!(short_age(345_600), "4 d");
    }

    #[test]
    fn a_snapshot_shows_locks_leftovers_and_entries() {
        let root = std::env::temp_dir().join(format!("orgthread-watch-{}", std::process::id()));
        let locks = root.join("locks");
        fs::create_dir_all(locks.join(".claude")).unwrap();
        fs::write(locks.join("org--notes--a.md.lock"), "some chat 10:00\n").unwrap();
        fs::write(locks.join("org--notes--a.md.77.rescue"), "kept\n").unwrap();
        let log = root.join("writes.log");
        fs::write(&log, "t\twritten\torg/b.md\tchat\t-\tabc\t3\nbroken\n").unwrap();

        let s = snapshot(&locks, &log);
        assert_eq!(s.locks.len(), 1);
        assert_eq!(s.locks[0].path, "org/notes/a.md");
        assert_eq!(s.locks[0].holder, "some chat 10:00");
        assert!(!s.locks[0].stale);
        assert_eq!(s.left_behind, ["org--notes--a.md.77.rescue"]);
        assert_eq!(s.entries.len(), 1);
        assert!(s.problems.is_empty());

        let empty = snapshot(&root.join("no-locks"), &root.join("no-log"));
        assert_eq!(empty.entries.len(), 0);
        assert_eq!(empty.problems.len(), 1, "a missing lock folder is named");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_long_log_shows_only_whole_lines_of_its_end() {
        let root = std::env::temp_dir().join(format!("orgthread-tail-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let log = root.join("writes.log");
        let line = "2026-11-07 07:28:26Z\twritten\torg/long-enough-name.md\tchat\t-\tabc\t3\n";
        fs::write(&log, line.repeat(10_000)).unwrap();
        let s = snapshot(&root, &log);
        assert!(s.entries.len() < 10_000);
        assert!(s.entries.iter().all(|e| e.outcome == "written"));
        fs::remove_dir_all(&root).unwrap();
    }
}
