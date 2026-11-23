//! The sign-in log of the claude.ai door: one line per sign-in event and per call.
//!
//! Six fields with a tab between them, a dash for an empty field: time (UTC), word,
//! client, detail, exit code, ms. Each month has its own file, `door-YYYY-MM.log`,
//! in the folder of the write log. The door keeps 12 months. A line never holds a
//! token, a password, a login code, a hash of one, a path or a text: only fixed
//! words, the cleaned client id and the name of a known tool.

use crate::log;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// At most this many refused lines go in per clock hour.
const MAX_REFUSED: usize = 600;
/// A file whose month lies more than this many months back goes.
const KEEP_MONTHS: i64 = 12;
/// At most one line a minute goes to the journal when a line cannot be written.
const QUIET: u64 = 60;

/// What happened. The text of each is the word in the line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Word {
    Registered,
    SignedIn,
    WrongPassword,
    LockStarted,
    StillLocked,
    UnknownClient,
    BadRequest,
    CodeRefused,
    Renewed,
    RenewRefused,
    NoToken,
    BadToken,
    Call,
    DeletedOld,
    NotDeleted,
    Skipped,
}

impl Word {
    pub fn text(self) -> &'static str {
        match self {
            Word::Registered => "registered",
            Word::SignedIn => "signed-in",
            Word::WrongPassword => "wrong-password",
            Word::LockStarted => "lock-started",
            Word::StillLocked => "still-locked",
            Word::UnknownClient => "unknown-client",
            Word::BadRequest => "bad-request",
            Word::CodeRefused => "code-refused",
            Word::Renewed => "renewed",
            Word::RenewRefused => "renew-refused",
            Word::NoToken => "no-token",
            Word::BadToken => "bad-token",
            Word::Call => "call",
            Word::DeletedOld => "deleted-old",
            Word::NotDeleted => "not-deleted",
            Word::Skipped => "skipped",
        }
    }

    /// A refusal. A stranger can cause as many as he likes, so these count
    /// against the limit of the hour. The other words always go in.
    fn refused(self) -> bool {
        matches!(
            self,
            Word::WrongPassword
                | Word::StillLocked
                | Word::UnknownClient
                | Word::BadRequest
                | Word::CodeRefused
                | Word::RenewRefused
                | Word::NoToken
                | Word::BadToken
        )
    }
}

/// A client id as a line shows it: only the signs 0 to 9 and a to f, at most 8 of
/// them, or `?` when none is left. An id comes from a form, so it is not trusted.
pub fn client_tag(id: &str) -> String {
    let tag: String = id
        .chars()
        .filter(|c| matches!(c, '0'..='9' | 'a'..='f'))
        .take(8)
        .collect();
    if tag.is_empty() { "?".to_string() } else { tag }
}

/// The month of a time, for example `2026-11`.
fn month_of(seconds: u64) -> String {
    log::utc_text(seconds)[..7].to_string()
}

/// The months since year 0 of a name `door-YYYY-MM.log`, or nothing for any other name.
fn month_number(name: &str) -> Option<i64> {
    let month = name.strip_prefix("door-")?.strip_suffix(".log")?;
    let (year, month) = month.split_once('-')?;
    let digits = |t: &str, n: usize| t.len() == n && t.bytes().all(|b| b.is_ascii_digit());
    if !digits(year, 4) || !digits(month, 2) {
        return None;
    }
    let (year, month): (i64, i64) = (year.parse().ok()?, month.parse().ok()?);
    (1..=12).contains(&month).then_some(year * 12 + month - 1)
}

/// The folder of a write log, where the door log goes too. A bare file name means
/// the folder the program runs in.
pub fn folder_of(log: &Path) -> PathBuf {
    match log.parent() {
        Some(folder) if !folder.as_os_str().is_empty() => folder.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

pub type Clock = Box<dyn Fn() -> u64 + Send + Sync>;
pub type Journal = Box<dyn Fn(&str) + Send + Sync>;

#[derive(Default)]
struct State {
    /// The month of the last line, to notice a new one.
    month: String,
    /// The clock hour of the last line, in hours since 1970.
    hour: u64,
    /// The refused lines written in that hour.
    refused: usize,
    /// The refused lines left out in that hour.
    skipped: usize,
    /// When the journal last got a line.
    complained: Option<u64>,
}

pub struct DoorLog {
    dir: PathBuf,
    clock: Clock,
    journal: Journal,
    state: Mutex<State>,
}

fn system_clock() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl DoorLog {
    /// The log in `dir`, on the clock of the system. Problems go to standard error,
    /// which is the journal of the service. Deletes the files that are too old.
    pub fn new(dir: &Path) -> DoorLog {
        DoorLog::with(
            dir,
            Box::new(system_clock),
            Box::new(|text: &str| eprintln!("{text}")),
        )
    }

    /// The same, with a clock and a journal of the caller's choice.
    pub fn with(dir: &Path, clock: Clock, journal: Journal) -> DoorLog {
        let log = DoorLog {
            dir: dir.to_path_buf(),
            clock,
            journal,
            state: Mutex::new(State::default()),
        };
        let now = (log.clock)();
        let mut state = log.lock();
        state.month = month_of(now);
        state.hour = now / 3600;
        log.clean(&mut state, now);
        drop(state);
        log
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The file of the month of a time.
    pub fn file_of(&self, seconds: u64) -> PathBuf {
        self.dir.join(format!("door-{}.log", month_of(seconds)))
    }

    /// Writes one line. An empty client is a dash; any other is cleaned. A line that
    /// cannot be written never stops the request: the journal hears of it instead.
    pub fn write(&self, word: Word, client: &str, detail: &str, code: &str, ms: &str) {
        let now = (self.clock)();
        let mut state = self.lock();
        let month = month_of(now);
        if month != state.month {
            state.month = month;
            self.clean(&mut state, now);
        }
        let hour = now / 3600;
        if hour != state.hour {
            if state.skipped > 0 {
                let count = state.skipped.to_string();
                self.put(&mut state, now, Word::Skipped, "", &count, "", "");
            }
            state.hour = hour;
            state.refused = 0;
            state.skipped = 0;
        }
        if word.refused() {
            if state.refused >= MAX_REFUSED {
                state.skipped += 1;
                return;
            }
            state.refused += 1;
        }
        let client = if client.is_empty() {
            String::new()
        } else {
            client_tag(client)
        };
        self.put(&mut state, now, word, &client, detail, code, ms);
    }

    #[allow(clippy::too_many_arguments)]
    fn put(
        &self,
        state: &mut State,
        now: u64,
        word: Word,
        client: &str,
        detail: &str,
        code: &str,
        ms: &str,
    ) {
        let time = log::utc_text(now);
        let line = log::line(&[&time, word.text(), client, detail, code, ms]);
        let file = self.file_of(now);
        if let Err(e) = log::append(&file, &line) {
            self.complain(
                state,
                now,
                &format!("could not write {}: {e}", file.display()),
            );
        }
    }

    fn complain(&self, state: &mut State, now: u64, text: &str) {
        if state
            .complained
            .is_none_or(|then| now.saturating_sub(then) >= QUIET)
        {
            state.complained = Some(now);
            (self.journal)(&format!("door log: {text}"));
        }
    }

    /// Deletes each plain file `door-YYYY-MM.log` of the folder whose month lies more
    /// than 12 months before the month of `now`. Nothing else, never a link or a folder.
    fn clean(&self, state: &mut State, now: u64) {
        let Some(current) = month_number(&format!("door-{}.log", month_of(now))) else {
            return;
        };
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return,
            Err(e) => {
                let text = format!("could not look into {}: {e}", self.dir.display());
                return self.complain(state, now, &text);
            }
        };
        let mut old: Vec<(String, PathBuf)> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                let month = month_number(&name)?;
                (current - month > KEEP_MONTHS).then(|| (name, entry.path()))
            })
            .collect();
        old.sort();
        for (name, path) in old {
            let plain = fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file());
            let word = if plain && fs::remove_file(&path).is_ok() {
                Word::DeletedOld
            } else {
                Word::NotDeleted
            };
            self.put(state, now, word, "", &name, "", "");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// 2026-11-30 23:59:58 UTC.
    const NOV_END: u64 = 1_796_083_198;
    /// 2026-12-01 00:00:03 UTC.
    const DEC_START: u64 = 1_796_083_203;

    /// A folder of its own for each test, gone at the end.
    struct Place(PathBuf);
    impl Place {
        fn new(name: &str) -> Place {
            let dir = std::env::temp_dir()
                .join(format!("orgthread-door-log-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Place(dir)
        }
        fn lines(&self, month: &str) -> Vec<String> {
            fs::read_to_string(self.0.join(format!("door-{month}.log")))
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        }
    }
    impl Drop for Place {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A log on a clock the test sets, and the lines its journal got.
    fn log_at(dir: &Path, start: u64) -> (DoorLog, Arc<AtomicU64>, Arc<Mutex<Vec<String>>>) {
        let time = Arc::new(AtomicU64::new(start));
        let heard = Arc::new(Mutex::new(Vec::new()));
        let clock = {
            let time = time.clone();
            Box::new(move || time.load(Ordering::SeqCst))
        };
        let journal = {
            let heard = heard.clone();
            Box::new(move |text: &str| heard.lock().unwrap().push(text.to_string()))
        };
        (DoorLog::with(dir, clock, journal), time, heard)
    }

    #[test]
    fn the_clock_times_of_the_tests_are_what_they_say() {
        assert_eq!(log::utc_text(NOV_END), "2026-11-30 23:59:58Z");
        assert_eq!(log::utc_text(DEC_START), "2026-12-01 00:00:03Z");
    }

    #[test]
    fn a_line_has_six_fields_and_a_dash_for_each_empty_one() {
        let place = Place::new("line");
        let (log, _, _) = log_at(&place.0, NOV_END);
        log.write(Word::Call, "3f9a1c2e77", "read", "0", "140");
        log.write(Word::NoToken, "", "", "", "");
        assert_eq!(
            place.lines("2026-11"),
            [
                "2026-11-30 23:59:58Z\tcall\t3f9a1c2e\tread\t0\t140",
                "2026-11-30 23:59:58Z\tno-token\t-\t-\t-\t-",
            ]
        );
    }

    #[test]
    fn a_client_id_keeps_only_eight_signs_of_0_to_9_and_a_to_f() {
        assert_eq!(client_tag("3f9a1c2e77aa"), "3f9a1c2e");
        assert_eq!(client_tag("3f\t9a\n1c\r2e"), "3f9a1c2e");
        assert_eq!(client_tag("ab cd/../ZZ"), "abcd");
        assert_eq!(client_tag("xyz\t\n"), "?");
        assert_eq!(client_tag("?"), "?");
        assert_eq!(client_tag("ABCDEF"), "?");
    }

    #[test]
    fn a_dirty_client_id_cannot_break_a_line() {
        let place = Place::new("dirty");
        let (log, _, _) = log_at(&place.0, NOV_END);
        log.write(Word::UnknownClient, "a\tb\nc", "", "", "");
        log.write(Word::UnknownClient, "\t\n", "", "", "");
        let lines = place.lines("2026-11");
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            "2026-11-30 23:59:58Z\tunknown-client\tabc\t-\t-\t-"
        );
        assert_eq!(lines[1], "2026-11-30 23:59:58Z\tunknown-client\t?\t-\t-\t-");
    }

    #[test]
    fn line_600_of_refusals_goes_in_601_does_not_and_the_next_hour_says_how_many() {
        let place = Place::new("flood");
        // 2026-11-23 12:00:00 UTC, the start of an hour.
        let hour = 1_795_435_200;
        let (log, time, _) = log_at(&place.0, hour);
        for _ in 0..600 {
            log.write(Word::WrongPassword, "ab", "", "", "");
        }
        assert_eq!(place.lines("2026-11").len(), 600);
        time.store(hour + 3_599, Ordering::SeqCst);
        log.write(Word::BadToken, "", "", "", "");
        log.write(Word::NoToken, "", "", "", "");
        assert_eq!(place.lines("2026-11").len(), 600, "601 and 602 stay out");
        // The words that always go in still do.
        log.write(Word::Call, "ab", "read", "0", "5");
        log.write(Word::SignedIn, "ab", "password", "", "");
        assert_eq!(place.lines("2026-11").len(), 602);

        time.store(hour + 3_600 + 7, Ordering::SeqCst);
        log.write(Word::NoToken, "", "", "", "");
        let lines = place.lines("2026-11");
        assert_eq!(lines.len(), 604);
        assert_eq!(lines[602], "2026-11-23 13:00:07Z\tskipped\t-\t2\t-\t-");
        assert_eq!(lines[603], "2026-11-23 13:00:07Z\tno-token\t-\t-\t-\t-");

        // A new hour counts from zero and says nothing when nothing was left out.
        time.store(hour + 7_200, Ordering::SeqCst);
        log.write(Word::NoToken, "", "", "", "");
        assert_eq!(place.lines("2026-11").len(), 605);
    }

    #[test]
    fn the_end_of_a_month_and_the_start_of_the_next_land_in_two_files() {
        let place = Place::new("month");
        let (log, time, _) = log_at(&place.0, NOV_END);
        log.write(Word::Renewed, "ab", "", "", "");
        time.store(DEC_START, Ordering::SeqCst);
        log.write(Word::Renewed, "cd", "", "", "");
        assert_eq!(
            place.lines("2026-11"),
            ["2026-11-30 23:59:58Z\trenewed\tab\t-\t-\t-"]
        );
        assert_eq!(
            place.lines("2026-12"),
            ["2026-12-01 00:00:03Z\trenewed\tcd\t-\t-\t-"]
        );
    }

    /// The files of the test of the delete, and a link with a door name.
    fn old_files(place: &Place) {
        for name in [
            "door-2025-11.log",
            "door-2025-12.log",
            "door-2025-11.log.bak",
            "notes.txt",
        ] {
            fs::write(place.0.join(name), "old\n").unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(place.0.join("notes.txt"), place.0.join("door-2024-01.log"))
            .unwrap();
    }

    fn names(place: &Place) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&place.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[cfg(unix)]
    #[test]
    fn the_first_line_of_a_new_month_deletes_only_door_files_older_than_12_months() {
        let place = Place::new("delete-month");
        let (log, time, _) = log_at(&place.0, NOV_END);
        old_files(&place);
        log.write(Word::Renewed, "ab", "", "", "");
        assert!(place.0.join("door-2025-11.log").exists(), "not yet");
        time.store(DEC_START, Ordering::SeqCst);
        log.write(Word::Renewed, "ab", "", "", "");
        assert_eq!(
            names(&place),
            [
                "door-2024-01.log",
                "door-2025-11.log.bak",
                "door-2025-12.log",
                "door-2026-11.log",
                "door-2026-12.log",
                "notes.txt",
            ]
        );
        assert_eq!(
            fs::read_to_string(place.0.join("notes.txt")).unwrap(),
            "old\n"
        );
        assert_eq!(
            place.lines("2026-12"),
            [
                "2026-12-01 00:00:03Z\tnot-deleted\t-\tdoor-2024-01.log\t-\t-",
                "2026-12-01 00:00:03Z\tdeleted-old\t-\tdoor-2025-11.log\t-\t-",
                "2026-12-01 00:00:03Z\trenewed\tab\t-\t-\t-",
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_start_of_the_door_deletes_the_door_files_older_than_12_months() {
        let place = Place::new("delete-start");
        old_files(&place);
        let _ = log_at(&place.0, DEC_START);
        assert!(!place.0.join("door-2025-11.log").exists());
        assert!(place.0.join("door-2025-12.log").exists());
        assert!(place.0.join("door-2025-11.log.bak").exists());
        assert!(fs::symlink_metadata(place.0.join("door-2024-01.log")).is_ok());
        assert_eq!(place.lines("2026-12").len(), 2);
    }

    #[test]
    fn an_old_folder_with_a_door_name_stays_and_gets_a_line() {
        let place = Place::new("delete-folder");
        fs::create_dir(place.0.join("door-2020-01.log")).unwrap();
        let _ = log_at(&place.0, DEC_START);
        assert!(place.0.join("door-2020-01.log").is_dir());
        assert_eq!(
            place.lines("2026-12"),
            ["2026-12-01 00:00:03Z\tnot-deleted\t-\tdoor-2020-01.log\t-\t-"]
        );
    }

    #[test]
    fn the_door_log_goes_into_the_folder_of_the_write_log() {
        let folder = |log: &str| folder_of(Path::new(log));
        assert_eq!(folder("/srv/state/writes.log"), Path::new("/srv/state"));
        assert_eq!(folder("state/writes.log"), Path::new("state"));
        assert_eq!(folder("writes.log"), Path::new("."));
    }

    #[test]
    fn only_a_real_month_name_counts() {
        assert_eq!(month_number("door-2025-11.log"), Some(2025 * 12 + 10));
        for name in [
            "door-2025-13.log",
            "door-2025-00.log",
            "door-25-11.log",
            "door-2025-1.log",
            "door-2025-11.log.bak",
            "xdoor-2025-11.log",
            "door-2025-+1.log",
        ] {
            assert_eq!(month_number(name), None, "{name}");
        }
    }

    #[test]
    fn a_log_that_cannot_be_written_tells_the_journal_at_most_once_a_minute() {
        let place = Place::new("no-write");
        // A file stands where the folder of the log goes.
        let dir = place.0.join("state");
        fs::write(&dir, "").unwrap();
        let (log, time, heard) = log_at(&dir, NOV_END - 100);
        log.write(Word::Call, "ab", "read", "0", "1");
        log.write(Word::Call, "ab", "read", "0", "1");
        time.store(NOV_END - 41, Ordering::SeqCst);
        log.write(Word::Call, "ab", "read", "0", "1");
        assert_eq!(heard.lock().unwrap().len(), 1);
        time.store(NOV_END - 40, Ordering::SeqCst);
        log.write(Word::Call, "ab", "read", "0", "1");
        let heard = heard.lock().unwrap();
        assert_eq!(heard.len(), 2);
        // The first is the look for old files at the start, which fails the same way.
        assert!(
            heard[0].starts_with("door log: could not look into "),
            "{}",
            heard[0]
        );
        assert!(
            heard[1].starts_with("door log: could not write "),
            "{}",
            heard[1]
        );
    }
}
