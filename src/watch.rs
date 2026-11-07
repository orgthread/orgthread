//! What the read-only screen shows: the locks and the log. This part only
//! reads, and it knows nothing about the terminal.

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

/// One log line, or `None` when it does not have the seven fields.
pub fn parse_line(line: &str) -> Option<Entry> {
    let f: Vec<&str> = line.split('\t').collect();
    let [time, outcome, path, who, before, after, size] = f[..] else {
        return None;
    };
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

/// True for an outcome that wrote what was asked.
pub fn is_written(outcome: &str) -> bool {
    outcome == "written"
}

/// The reason in plain words.
pub fn why(outcome: &str) -> &'static str {
    match outcome {
        "written" => "The file was written and read back. Every byte matches.",
        "locked" => "Another writer held the lock. Nothing was written.",
        "changed" => "The file changed after this chat read it. Nothing was written.",
        "exists" => "A new file was asked for, but a file is there. Nothing was written.",
        "missing" => "This chat expected a file, but none is there. Nothing was written.",
        "restored" => "The check after the write failed. The old state is back.",
        "not-restored" => "The check failed and the old state is NOT back. Look at this file now.",
        "failed" => "The write failed, and the file is unchanged.",
        _ => "This outcome is not known to this version.",
    }
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
    fn a_broken_line_is_skipped() {
        assert_eq!(parse_line("not a log line"), None);
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
