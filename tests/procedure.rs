//! The procedure on invented files in a temporary folder.

use orgthread::fingerprint::fingerprint;
use orgthread::storage::{Expect, LocalFolder, Storage, WriteError};
use orgthread::write::{Outcome, check_text, exit, lock_name, write};
use std::cell::Cell;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, SystemTime};

const LINE: &str = "test writer 2026-11-07 10:00";

struct Folder(PathBuf);

impl Folder {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("orgthread-test-{}-{n}", std::process::id()));
        fs::create_dir_all(root.join("org/.locks")).unwrap();
        fs::create_dir_all(root.join("org/notes")).unwrap();
        Folder(root)
    }
    fn storage(&self) -> LocalFolder {
        LocalFolder::new(&self.0, "org/.locks")
    }
    fn put(&self, path: &str, bytes: &[u8]) {
        fs::write(self.0.join(path), bytes).unwrap();
    }
    fn get(&self, path: &str) -> Vec<u8> {
        fs::read(self.0.join(path)).unwrap()
    }
    fn lock(&self, path: &str) -> PathBuf {
        self.0.join("org/.locks").join(lock_name(path))
    }
    /// The lock folder must hold nothing after any write.
    fn assert_locks_empty(&self) {
        let left: Vec<_> = fs::read_dir(self.0.join("org/.locks"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(left.is_empty(), "left in the lock folder: {left:?}");
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn set_age(path: &Path, age: Duration) {
    let file = File::options().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() - age).unwrap();
}

const FILE: &str = "org/notes/invented.md";

fn expect(bytes: &[u8]) -> Expect {
    Expect::Fingerprint(fingerprint(bytes))
}

#[test]
fn lock_name_follows_the_written_procedure() {
    assert_eq!(
        lock_name("org/area/log/log-2026-10-22.md"),
        "org--area--log--log-2026-10-22.md.lock"
    );
    assert_eq!(lock_name("org/AGENTS.md"), "org--AGENTS.md.lock");
    assert_eq!(
        lock_name("org-skills/AGENTS.md"),
        "org-skills--AGENTS.md.lock"
    );
}

#[test]
fn a_write_replaces_the_file_and_releases_the_lock() {
    let f = Folder::new();
    f.put(FILE, b"old\n");
    let r = write(&f.storage(), FILE, &expect(b"old\n"), b"new\n", LINE);
    assert!(matches!(r.outcome, Outcome::Written { .. }), "{r:?}");
    assert_eq!(f.get(FILE), b"new\n");
    f.assert_locks_empty();
}

#[test]
fn a_changed_file_is_refused_and_stays_whole() {
    let f = Folder::new();
    f.put(FILE, b"someone edited this\n");
    let r = write(
        &f.storage(),
        FILE,
        &expect(b"what the chat read\n"),
        b"new\n",
        LINE,
    );
    assert!(matches!(r.outcome, Outcome::Changed { .. }), "{r:?}");
    assert_eq!(f.get(FILE), b"someone edited this\n");
    f.assert_locks_empty();
}

#[test]
fn a_held_lock_is_refused_and_stays() {
    let f = Folder::new();
    f.put(FILE, b"old\n");
    fs::write(f.lock(FILE), "other writer 2026-11-07 09:59\n").unwrap();
    let r = write(&f.storage(), FILE, &expect(b"old\n"), b"new\n", LINE);
    match r.outcome {
        Outcome::Locked { ref holder, .. } => assert_eq!(holder, "other writer 2026-11-07 09:59"),
        ref other => panic!("{other:?}"),
    }
    assert_eq!(f.get(FILE), b"old\n");
    assert_eq!(
        fs::read_to_string(f.lock(FILE)).unwrap(),
        "other writer 2026-11-07 09:59\n"
    );
}

#[test]
fn a_lock_just_under_15_minutes_is_still_held() {
    let f = Folder::new();
    f.put(FILE, b"old\n");
    fs::write(f.lock(FILE), "slow writer\n").unwrap();
    set_age(&f.lock(FILE), Duration::from_secs(14 * 60 + 50));
    let r = write(&f.storage(), FILE, &expect(b"old\n"), b"new\n", LINE);
    assert!(matches!(r.outcome, Outcome::Locked { .. }), "{r:?}");
    assert_eq!(f.get(FILE), b"old\n");
}

#[test]
fn a_stale_lock_is_taken_over_and_named() {
    let f = Folder::new();
    f.put(FILE, b"old\n");
    fs::write(f.lock(FILE), "gone writer\n").unwrap();
    set_age(&f.lock(FILE), Duration::from_secs(16 * 60));
    let r = write(&f.storage(), FILE, &expect(b"old\n"), b"new\n", LINE);
    assert!(matches!(r.outcome, Outcome::Written { .. }), "{r:?}");
    assert_eq!(r.took_over.as_deref(), Some("gone writer"));
    assert_eq!(f.get(FILE), b"new\n");
    f.assert_locks_empty();
}

#[test]
fn new_refuses_an_existing_file() {
    let f = Folder::new();
    f.put(FILE, b"here already\n");
    let r = write(&f.storage(), FILE, &Expect::Absent, b"new\n", LINE);
    assert!(matches!(r.outcome, Outcome::Exists { .. }), "{r:?}");
    assert_eq!(f.get(FILE), b"here already\n");
    f.assert_locks_empty();
}

#[test]
fn new_creates_a_file() {
    let f = Folder::new();
    let r = write(&f.storage(), FILE, &Expect::Absent, b"first\n", LINE);
    assert!(
        matches!(r.outcome, Outcome::Written { old: None, .. }),
        "{r:?}"
    );
    assert_eq!(f.get(FILE), b"first\n");
    f.assert_locks_empty();
}

#[test]
fn expect_on_a_missing_file_is_refused() {
    let f = Folder::new();
    let r = write(&f.storage(), FILE, &expect(b"old\n"), b"new\n", LINE);
    assert!(matches!(r.outcome, Outcome::Missing { .. }), "{r:?}");
    assert!(!f.0.join(FILE).exists());
    f.assert_locks_empty();
}

#[test]
fn every_byte_survives() {
    let samples: [&[u8]; 6] = [
        "Äpfel, Öl, Übung, Straße, äöüß\n".as_bytes(),
        b"no newline at the end",
        b"  odd\t spacing  \n\n\n   \t\n",
        b"windows endings\r\nand more\r\n",
        "\u{feff}a byte order mark, then \u{a0}a hard space\n".as_bytes(),
        "* TODO Org heading :tag:\n  SCHEDULED: <2026-11-07 Sat>\n".as_bytes(),
    ];
    for sample in samples {
        let f = Folder::new();
        f.put(FILE, b"old\n");
        let r = write(&f.storage(), FILE, &expect(b"old\n"), sample, LINE);
        assert!(matches!(r.outcome, Outcome::Written { .. }), "{r:?}");
        assert_eq!(f.get(FILE), sample);
        // And they survive as the old text of the next write too.
        let r = write(&f.storage(), FILE, &expect(sample), b"next\n", LINE);
        assert!(matches!(r.outcome, Outcome::Written { .. }), "{r:?}");
    }
}

#[cfg(unix)]
#[test]
fn file_permissions_stay() {
    use std::os::unix::fs::PermissionsExt;
    let f = Folder::new();
    f.put(FILE, b"old\n");
    fs::set_permissions(f.0.join(FILE), fs::Permissions::from_mode(0o640)).unwrap();
    let r = write(&f.storage(), FILE, &expect(b"old\n"), b"new\n", LINE);
    assert!(matches!(r.outcome, Outcome::Written { .. }), "{r:?}");
    let mode = fs::metadata(f.0.join(FILE)).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o640);
}

#[test]
fn not_plain_text_is_refused() {
    assert!(check_text(b"").is_err());
    assert!(check_text(b"bad \xff bytes").is_err());
    assert!(check_text(b"a\0b").is_err());
    assert!(check_text("Grüße\n".as_bytes()).is_ok());
}

#[test]
fn a_missing_lock_folder_is_a_failure_not_a_new_folder() {
    let f = Folder::new();
    f.put(FILE, b"old\n");
    let storage = LocalFolder::new(&f.0, "org/.no-such-locks");
    let r = write(&storage, FILE, &expect(b"old\n"), b"new\n", LINE);
    assert!(matches!(r.outcome, Outcome::Failed { .. }), "{r:?}");
    assert_eq!(f.get(FILE), b"old\n");
    assert!(!f.0.join("org/.no-such-locks").exists());
}

/// A storage that goes wrong on request, around the real local folder.
struct Faulty<'a> {
    inner: LocalFolder,
    folder: &'a Folder,
    write_fails: bool,
    read_panics: bool,
    move_writes_other_bytes: bool,
    restore_fails: bool,
    restore_writes_other_bytes: bool,
    rescue_fails: bool,
    lock_taken_after_write: bool,
    lock_taken_after_read: bool,
    release_fails: bool,
    edit_before_write: Option<&'a [u8]>,
    reads: Cell<u32>,
}

impl<'a> Faulty<'a> {
    fn new(folder: &'a Folder) -> Self {
        Faulty {
            inner: folder.storage(),
            folder,
            write_fails: false,
            read_panics: false,
            move_writes_other_bytes: false,
            restore_fails: false,
            restore_writes_other_bytes: false,
            rescue_fails: false,
            lock_taken_after_write: false,
            lock_taken_after_read: false,
            release_fails: false,
            edit_before_write: None,
            reads: Cell::new(0),
        }
    }
}

impl Storage for Faulty<'_> {
    fn read(&self, path: &str) -> io::Result<Option<Vec<u8>>> {
        self.reads.set(self.reads.get() + 1);
        assert!(!self.read_panics, "invented panic");
        let bytes = self.inner.read(path);
        if self.lock_taken_after_read {
            fs::write(self.folder.lock(path), "new owner\n").unwrap();
        }
        bytes
    }
    fn write_if_unchanged(&self, path: &str, e: &Expect, b: &[u8]) -> Result<(), WriteError> {
        if self.write_fails {
            return Err(WriteError::Io(io::Error::other("invented disk failure")));
        }
        if let Some(edit) = self.edit_before_write {
            self.folder.put(path, edit);
        }
        self.inner.write_if_unchanged(path, e, b)?;
        if self.move_writes_other_bytes {
            self.folder.put(path, b"damaged on the way");
        }
        if self.lock_taken_after_write {
            fs::write(self.folder.lock(path), "new owner\n").unwrap();
        }
        Ok(())
    }
    fn rescue(&self, path: &str, bytes: &[u8]) -> io::Result<String> {
        if self.rescue_fails {
            return Err(io::Error::other("invented rescue failure"));
        }
        self.inner.rescue(path, bytes)
    }
    fn restore(&self, path: &str, old: Option<&[u8]>) -> io::Result<()> {
        if self.restore_fails {
            return Err(io::Error::other("invented restore failure"));
        }
        self.inner.restore(path, old)?;
        if self.restore_writes_other_bytes {
            self.folder.put(path, b"still wrong");
        }
        Ok(())
    }
    fn lock_create(&self, n: &str, l: &str) -> io::Result<bool> {
        self.inner.lock_create(n, l)
    }
    fn lock_read(&self, n: &str) -> io::Result<Option<String>> {
        self.inner.lock_read(n)
    }
    fn lock_age(&self, n: &str) -> io::Result<Option<Duration>> {
        self.inner.lock_age(n)
    }
    fn lock_take_stale(&self, n: &str, a: Duration) -> io::Result<Option<String>> {
        self.inner.lock_take_stale(n, a)
    }
    fn lock_remove(&self, n: &str) -> io::Result<()> {
        if self.release_fails {
            return Err(io::Error::other("invented delete failure"));
        }
        self.inner.lock_remove(n)
    }
}

#[test]
fn a_failed_write_still_releases_the_lock() {
    let f = Folder::new();
    f.put(FILE, b"old\n");
    let mut s = Faulty::new(&f);
    s.write_fails = true;
    let r = write(&s, FILE, &expect(b"old\n"), b"new\n", LINE);
    assert!(matches!(r.outcome, Outcome::Failed { .. }), "{r:?}");
    assert_eq!(f.get(FILE), b"old\n");
    f.assert_locks_empty();
}

#[test]
fn a_panic_still_releases_the_lock() {
    let f = Folder::new();
    f.put(FILE, b"old\n");
    let mut s = Faulty::new(&f);
    s.read_panics = true;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        write(&s, FILE, &expect(b"old\n"), b"new\n", LINE)
    }));
    assert!(result.is_err());
    assert_eq!(f.get(FILE), b"old\n");
    f.assert_locks_empty();
}

#[test]
fn a_bad_read_back_puts_the_old_file_back() {
    let f = Folder::new();
    f.put(FILE, b"old\n");
    let mut s = Faulty::new(&f);
    s.move_writes_other_bytes = true;
    let r = write(&s, FILE, &expect(b"old\n"), b"new\n", LINE);
    let Outcome::Restored {
        rescued: Some(place),
        ..
    } = &r.outcome
    else {
        panic!("{r:?}");
    };
    assert_eq!(f.get(FILE), b"old\n");
    // What the file held instead may be a person's edit, and it is kept.
    assert_eq!(fs::read(place).unwrap(), b"damaged on the way");
    fs::remove_file(place).unwrap();
    f.assert_locks_empty();
}

#[test]
fn a_bad_read_back_of_a_new_file_removes_it_and_keeps_the_bytes() {
    let f = Folder::new();
    let mut s = Faulty::new(&f);
    s.move_writes_other_bytes = true;
    let r = write(&s, FILE, &Expect::Absent, b"new\n", LINE);
    let Outcome::Restored {
        rescued: Some(place),
        ..
    } = &r.outcome
    else {
        panic!("{r:?}");
    };
    assert!(!f.0.join(FILE).exists());
    assert_eq!(fs::read(place).unwrap(), b"damaged on the way");
    fs::remove_file(place).unwrap();
    f.assert_locks_empty();
}

#[test]
fn bytes_that_cannot_be_kept_are_not_overwritten() {
    let f = Folder::new();
    f.put(FILE, b"old\n");
    let mut s = Faulty::new(&f);
    s.move_writes_other_bytes = true;
    s.rescue_fails = true;
    let r = write(&s, FILE, &expect(b"old\n"), b"new\n", LINE);
    assert!(matches!(r.outcome, Outcome::NotRestored { .. }), "{r:?}");
    assert_eq!(f.get(FILE), b"damaged on the way");
    f.assert_locks_empty();
}

#[test]
fn a_restore_that_does_not_hold_is_named() {
    let f = Folder::new();
    f.put(FILE, b"old\n");
    let mut s = Faulty::new(&f);
    s.move_writes_other_bytes = true;
    s.restore_writes_other_bytes = true;
    let r = write(&s, FILE, &expect(b"old\n"), b"new\n", LINE);
    let Outcome::NotRestored {
        rescued: Some(place),
        ..
    } = &r.outcome
    else {
        panic!("{r:?}");
    };
    fs::remove_file(place).unwrap();
    f.assert_locks_empty();
}

#[test]
fn a_lock_lost_while_reading_means_nothing_is_written() {
    let f = Folder::new();
    f.put(FILE, b"old\n");
    let mut s = Faulty::new(&f);
    s.lock_taken_after_read = true;
    let r = write(&s, FILE, &expect(b"old\n"), b"new\n", LINE);
    assert!(matches!(r.outcome, Outcome::Failed { .. }), "{r:?}");
    assert_eq!(f.get(FILE), b"old\n");
    assert_eq!(fs::read_to_string(f.lock(FILE)).unwrap(), "new owner\n");
    assert_eq!(r.exit_code(true), exit::FAILED);
}

#[test]
fn a_lock_lost_during_the_write_stays_and_is_named() {
    let f = Folder::new();
    f.put(FILE, b"old\n");
    let mut s = Faulty::new(&f);
    s.lock_taken_after_write = true;
    let r = write(&s, FILE, &expect(b"old\n"), b"new\n", LINE);
    assert_eq!(r.lock_lost.as_deref(), Some("new owner"));
    assert_eq!(fs::read_to_string(f.lock(FILE)).unwrap(), "new owner\n");
    assert_eq!(r.exit_code(true), exit::LOCK_PROBLEM);
}

#[test]
fn exit_codes_of_the_bad_endings() {
    let f = Folder::new();
    f.put(FILE, b"old\n");
    let mut s = Faulty::new(&f);
    s.release_fails = true;
    let r = write(&s, FILE, &expect(b"old\n"), b"new\n", LINE);
    assert_eq!(r.exit_code(true), exit::LOCK_PROBLEM);
    assert_eq!(r.exit_code(false), exit::LOCK_PROBLEM);

    let g = Folder::new();
    g.put(FILE, b"old\n");
    let mut s = Faulty::new(&g);
    s.move_writes_other_bytes = true;
    s.restore_fails = true;
    s.release_fails = true;
    let r = write(&s, FILE, &expect(b"old\n"), b"new\n", LINE);
    // A possible damage is never hidden by a lock problem.
    assert_eq!(r.exit_code(true), exit::NOT_RESTORED);
}

#[test]
fn a_failed_restore_is_named_and_the_lock_goes() {
    let f = Folder::new();
    f.put(FILE, b"old\n");
    let mut s = Faulty::new(&f);
    s.move_writes_other_bytes = true;
    s.restore_fails = true;
    let r = write(&s, FILE, &expect(b"old\n"), b"new\n", LINE);
    let Outcome::NotRestored {
        rescued: Some(place),
        ..
    } = &r.outcome
    else {
        panic!("{r:?}");
    };
    assert_eq!(f.get(FILE), b"damaged on the way");
    fs::remove_file(place).unwrap();
    f.assert_locks_empty();
}

#[test]
fn an_edit_right_before_the_move_is_refused_and_survives() {
    let f = Folder::new();
    f.put(FILE, b"old\n");
    let mut s = Faulty::new(&f);
    s.edit_before_write = Some(b"typed in the editor just now\n");
    let r = write(&s, FILE, &expect(b"old\n"), b"new\n", LINE);
    assert!(matches!(r.outcome, Outcome::Changed { .. }), "{r:?}");
    assert_eq!(f.get(FILE), b"typed in the editor just now\n");
    f.assert_locks_empty();
}

#[test]
fn a_file_appearing_right_before_a_new_write_survives() {
    let f = Folder::new();
    let mut s = Faulty::new(&f);
    s.edit_before_write = Some(b"appeared just now\n");
    let r = write(&s, FILE, &Expect::Absent, b"new\n", LINE);
    assert!(matches!(r.outcome, Outcome::Exists { .. }), "{r:?}");
    assert_eq!(f.get(FILE), b"appeared just now\n");
    f.assert_locks_empty();
}

#[test]
fn a_lock_that_cannot_be_released_is_named() {
    let f = Folder::new();
    f.put(FILE, b"old\n");
    let mut s = Faulty::new(&f);
    s.release_fails = true;
    let r = write(&s, FILE, &expect(b"old\n"), b"new\n", LINE);
    assert!(matches!(r.outcome, Outcome::Written { .. }), "{r:?}");
    assert!(r.lock_left.is_some());
}

#[test]
fn of_two_writers_with_the_same_read_only_one_wins() {
    for _ in 0..50 {
        let f = Arc::new(Folder::new());
        f.put(FILE, b"old\n");
        let start = Arc::new(Barrier::new(2));
        let run = |text: &'static [u8], who: &'static str| {
            let (f, start) = (f.clone(), start.clone());
            std::thread::spawn(move || {
                start.wait();
                write(&f.storage(), FILE, &expect(b"old\n"), text, who)
            })
        };
        let a = run(b"from a\n", "a");
        let b = run(b"from b\n", "b");
        let (a, b) = (a.join().unwrap(), b.join().unwrap());
        let wins = [&a, &b]
            .iter()
            .filter(|r| matches!(r.outcome, Outcome::Written { .. }))
            .count();
        assert_eq!(wins, 1, "{a:?} {b:?}");
        let text = f.get(FILE);
        assert!(text == b"from a\n" || text == b"from b\n");
        f.assert_locks_empty();
    }
}

#[test]
fn a_stale_look_gone_fresh_hands_the_lock_back() {
    let f = Folder::new();
    let s = f.storage();
    fs::write(f.lock(FILE), "fresh owner\n").unwrap();
    let name = lock_name(FILE);
    assert_eq!(
        s.lock_take_stale(&name, Duration::from_secs(60)).unwrap(),
        None
    );
    assert_eq!(fs::read_to_string(f.lock(FILE)).unwrap(), "fresh owner\n");
    let left = fs::read_dir(f.0.join("org/.locks")).unwrap().count();
    assert_eq!(left, 1, "only the lock itself is there");
}

mod patch {
    use super::*;
    use orgthread::write::{Refusal, patch_unless_stopped};
    use std::sync::atomic::AtomicBool;

    fn no_stop() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn a_patch_builds_from_the_file_under_the_lock() {
        let f = Folder::new();
        f.put(FILE, b"a\nb\n");
        let build = |old: &[u8]| -> Result<(Vec<u8>, Vec<String>), Refusal> {
            assert_eq!(old, b"a\nb\n");
            Ok((b"a\nc\n".to_vec(), vec!["a note".into()]))
        };
        let r = patch_unless_stopped(&f.storage(), FILE, &build, LINE, &no_stop());
        match &r.outcome {
            Outcome::Written {
                notes, new_text, ..
            } => {
                assert_eq!(notes, &vec!["a note".to_string()]);
                assert_eq!(new_text, b"a\nc\n");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(f.get(FILE), b"a\nc\n");
        f.assert_locks_empty();
    }

    #[test]
    fn a_misfit_writes_nothing_and_gets_exit_11() {
        let f = Folder::new();
        f.put(FILE, b"a\n");
        let build = |_: &[u8]| -> Result<(Vec<u8>, Vec<String>), Refusal> {
            Err(Refusal::Misfit("Edit 1: no".into()))
        };
        let r = patch_unless_stopped(&f.storage(), FILE, &build, LINE, &no_stop());
        assert!(matches!(r.outcome, Outcome::Misfit { .. }), "{r:?}");
        assert_eq!(r.exit_code(true), 11);
        assert_eq!(f.get(FILE), b"a\n");
        f.assert_locks_empty();
    }

    #[test]
    fn a_missing_file_gets_exit_6() {
        let f = Folder::new();
        let build = |_: &[u8]| -> Result<(Vec<u8>, Vec<String>), Refusal> {
            panic!("no file, so nothing to build")
        };
        let r = patch_unless_stopped(&f.storage(), FILE, &build, LINE, &no_stop());
        assert!(matches!(r.outcome, Outcome::Missing { .. }), "{r:?}");
        assert_eq!(r.exit_code(true), exit::MISSING);
    }

    #[test]
    fn a_change_during_the_patch_is_refused_with_exit_4() {
        let f = Folder::new();
        f.put(FILE, b"a\n");
        let path = f.0.join(FILE);
        // A person saves while the patch is built: after the read, before the write.
        let build = |_: &[u8]| -> Result<(Vec<u8>, Vec<String>), Refusal> {
            fs::write(&path, b"a person's line\n").unwrap();
            Ok((b"b\n".to_vec(), Vec::new()))
        };
        let r = patch_unless_stopped(&f.storage(), FILE, &build, LINE, &no_stop());
        assert!(matches!(r.outcome, Outcome::Changed { .. }), "{r:?}");
        assert_eq!(r.exit_code(true), exit::CHANGED);
        assert_eq!(f.get(FILE), b"a person's line\n");
        f.assert_locks_empty();
    }
}

/// The program itself on a test folder, with its own state folder beside it.
struct Door {
    folder: Folder,
    state: PathBuf,
}

impl Door {
    fn new() -> Self {
        let mut folder = Folder::new();
        folder.0 = fs::canonicalize(&folder.0).unwrap();
        let state = folder.0.with_extension("state");
        fs::create_dir_all(&state).unwrap();
        Door { folder, state }
    }
    fn at(&self, path: &str) -> PathBuf {
        self.folder.0.join(path)
    }
    fn run(&self, args: &[&str], stdin: &[u8], fixed: Option<&str>) -> std::process::Output {
        self.run_as(args, stdin, fixed, None, &self.state.join("writes.log"))
    }
    /// The same, with a umask for the program, and a log of its own.
    fn run_as(
        &self,
        args: &[&str],
        stdin: &[u8],
        fixed: Option<&str>,
        umask: Option<&str>,
        log: &Path,
    ) -> std::process::Output {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let program = env!("CARGO_BIN_EXE_orgthread");
        let mut c = match umask {
            None => Command::new(program),
            Some(mask) => {
                let mut c = Command::new("/bin/sh");
                c.arg("-c")
                    .arg(format!("umask {mask}; exec \"$0\" \"$@\""))
                    .arg(program);
                c
            }
        };
        c.args(args)
            .env("ORGTHREAD_ROOT", &self.folder.0)
            .env("ORGTHREAD_LOCK_DIR", self.at("org/.locks"))
            .env("ORGTHREAD_LOG", log)
            .env("ORGTHREAD_READS", self.state.join("reads"))
            .env("ORGTHREAD_VERSIONS", self.state.join("versions"))
            .env_remove("ORGTHREAD_STORAGE")
            .env_remove("ORGTHREAD_ALLOW")
            .env_remove("ORGTHREAD_DENY")
            .env_remove("ORGTHREAD_READ_ONLY")
            .env_remove("ORGTHREAD_BINARY_ALLOW")
            .env_remove("ORGTHREAD_MOVE_ALLOW")
            .env_remove("ORGTHREAD_FIXED_FOLDERS")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(fixed) = fixed {
            c.env("ORGTHREAD_FIXED_FOLDERS", fixed);
        }
        let mut child = c.spawn().unwrap();
        let _ = child.stdin.take().unwrap().write_all(stdin);
        child.wait_with_output().unwrap()
    }
    fn p(&self, path: &str) -> String {
        self.at(path).to_str().unwrap().to_string()
    }
    fn write(&self, path: &str, fixed: Option<&str>) -> std::process::Output {
        self.run(&["write", &self.p(path), "--who", "chat"], b"abc", fixed)
    }
    /// Each log line as its fields.
    fn log(&self) -> Vec<Vec<String>> {
        fs::read_to_string(self.state.join("writes.log"))
            .unwrap_or_default()
            .lines()
            .map(|l| l.split('\t').map(str::to_string).collect())
            .collect()
    }
    /// The word and the path of each log line.
    fn words(&self) -> Vec<(String, String)> {
        self.log()
            .into_iter()
            .map(|f| (f[1].clone(), f[2].clone()))
            .collect()
    }
}

impl Drop for Door {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.state);
    }
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

fn out(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

#[test]
fn a_write_into_missing_folders_makes_names_and_logs_them() {
    let d = Door::new();
    let o = d.write("org/new/deep/a.md", None);
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    assert_eq!(
        out(&o),
        "written: org/new/deep/a.md, 3 bytes, new folders: org/new, org/new/deep\n"
    );
    assert_eq!(fs::read(d.at("org/new/deep/a.md")).unwrap(), b"abc");
    assert_eq!(
        d.words(),
        pairs(&[
            ("made-folder", "org/new"),
            ("made-folder", "org/new/deep"),
            ("written", "org/new/deep/a.md"),
        ]),
        "the outer folder first, right before the line of the file"
    );
    let line = &d.log()[0];
    assert_eq!(line.len(), 7, "the usual 7 fields: {line:?}");
    assert_eq!(line[3..], ["chat", "-", "-", "-"]);
    d.folder.assert_locks_empty();
}

#[test]
fn a_write_into_a_folder_that_exists_logs_no_new_folder() {
    let d = Door::new();
    let o = d.write("org/notes/b.md", None);
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    assert_eq!(out(&o), "written: org/notes/b.md, 3 bytes\n");
    assert_eq!(d.words(), pairs(&[("written", "org/notes/b.md")]));
    // A change to the file names no folder either.
    let o = d.write("org/notes/b.md", None);
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    assert_eq!(out(&o), "written: org/notes/b.md, 3 bytes\n");
    assert!(d.words().iter().all(|(word, _)| word == "written"));
}

#[test]
fn the_fixed_folders_refuse_a_new_first_level_folder_and_make_nothing() {
    let d = Door::new();
    for path in ["org/garden/notes.md", "org/helth/diet/x.md"] {
        let o = d.write(path, Some("/org"));
        assert_eq!(o.status.code(), Some(2), "{o:?}");
        let said = String::from_utf8_lossy(&o.stderr);
        assert!(
            said.contains("refused, nothing written.")
                && said.contains("directly in org, a folder of ORGTHREAD_FIXED_FOLDERS"),
            "{said}"
        );
    }
    assert!(!d.at("org/garden").exists());
    assert!(!d.at("org/helth").exists());
    assert!(d.log().is_empty(), "nothing made, nothing logged");
    d.folder.assert_locks_empty();
    // A file directly in a fixed folder is no new folder.
    let o = d.write("org/x.md", Some("/org"));
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    assert_eq!(out(&o), "written: org/x.md, 3 bytes\n");
}

#[test]
fn a_deeper_new_folder_below_a_fixed_folder_is_made() {
    let d = Door::new();
    let o = d.write("org/notes/items/2026/11/p.md", Some("/org"));
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    assert_eq!(
        out(&o),
        "written: org/notes/items/2026/11/p.md, 3 bytes, new folders: org/notes/items, org/notes/items/2026, org/notes/items/2026/11\n"
    );
    assert_eq!(
        fs::read(d.at("org/notes/items/2026/11/p.md")).unwrap(),
        b"abc"
    );
    assert_eq!(d.log().len(), 4);
}

#[test]
fn an_upload_makes_its_folders_and_the_guard_holds_for_it() {
    let d = Door::new();
    let upload = |path: &str, fixed| d.run(&["upload", &d.p(path), "--who", "chat"], b"abc", fixed);
    let o = upload("org/garden/a.md", Some("org"));
    assert_eq!(o.status.code(), Some(2), "{o:?}");
    assert!(!d.at("org/garden").exists());
    let o = d.run(
        &[
            "upload",
            "--check",
            &d.p("org/garden/a.md"),
            "--who",
            "chat",
        ],
        b"",
        Some("org"),
    );
    assert_eq!(
        o.status.code(),
        Some(2),
        "a check knows the guard too: {o:?}"
    );
    assert!(d.log().is_empty());
    let o = upload("org/notes/new/a.md", Some("org"));
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    assert_eq!(
        out(&o),
        "written: org/notes/new/a.md, 3 bytes, new folders: org/notes/new\n"
    );
    assert_eq!(
        d.words(),
        pairs(&[
            ("made-folder", "org/notes/new"),
            ("written", "org/notes/new/a.md"),
        ])
    );
}

#[test]
fn the_guard_holds_for_copy_and_move_and_makes_nothing() {
    let d = Door::new();
    d.folder.put(FILE, b"old\n");
    let from = d.p(FILE);
    for command in ["copy", "move"] {
        let to = d.p("org/garden/b.md");
        let o = d.run(&[command, &from, &to, "--who", "chat"], b"", Some("org"));
        assert_eq!(o.status.code(), Some(2), "{command}: {o:?}");
        let said = String::from_utf8_lossy(&o.stderr);
        assert!(
            said.contains("a folder of ORGTHREAD_FIXED_FOLDERS"),
            "{said}"
        );
        assert!(!d.at("org/garden").exists());
        assert_eq!(d.folder.get(FILE), b"old\n");
    }
    let words: Vec<String> = d.words().into_iter().map(|(w, _)| w).collect();
    assert_eq!(words, ["not-allowed", "move-not-allowed"]);
    d.folder.assert_locks_empty();
    // Deeper is fine, for both.
    let o = d.run(
        &["copy", &from, &d.p("org/notes/2026/c.md"), "--who", "chat"],
        b"",
        Some("org"),
    );
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    let o = d.run(
        &["move", &from, &d.p("org/notes/2027/m.md"), "--who", "chat"],
        b"",
        Some("org"),
    );
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    assert!(out(&o).contains(", new folders: org/notes/2027"), "{o:?}");
}

#[test]
fn a_copy_names_its_new_folders_and_logs_them_first() {
    let d = Door::new();
    d.folder.put(FILE, b"old\n");
    let o = d.run(
        &[
            "copy",
            &d.p(FILE),
            &d.p("org/new/deep/c.md"),
            "--who",
            "chat",
        ],
        b"",
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    assert_eq!(
        out(&o),
        format!(
            "copied: {FILE} -> org/new/deep/c.md, 4 bytes, {}, new folders: org/new, org/new/deep\n",
            fingerprint(b"old\n")
        )
    );
    assert_eq!(
        d.words(),
        pairs(&[
            ("made-folder", "org/new"),
            ("made-folder", "org/new/deep"),
            ("copied", "org/new/deep/c.md"),
        ])
    );
}

/// Gives the owner the right to write again, so that the test folder can go.
fn writable_again(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    for entry in fs::read_dir(path).into_iter().flatten().flatten() {
        let p = entry.path();
        if p.is_dir() {
            let _ = fs::set_permissions(&p, fs::Permissions::from_mode(0o755));
            writable_again(&p);
        }
    }
}

#[test]
fn a_write_that_fails_after_its_folders_logs_them_then_failed_and_they_stay() {
    let d = Door::new();
    // With umask 222 the first new folder comes without the right to write in
    // it, so the folder is made, and the file in it fails.
    fs::write(d.state.join("writes.log"), "").unwrap();
    let o = d.run_as(
        &["write", &d.p("org/new/a.md"), "--who", "chat"],
        b"abc",
        None,
        Some("222"),
        &d.state.join("writes.log"),
    );
    let made = d.at("org/new").is_dir();
    let file = d.at("org/new/a.md").exists();
    writable_again(&d.folder.0);
    assert_eq!(o.status.code(), Some(1), "{o:?}");
    assert!(made, "the folder stays");
    assert!(!file, "no file");
    assert_eq!(
        d.words(),
        pairs(&[("made-folder", "org/new"), ("failed", "org/new/a.md")]),
        "the made-folder line first, then failed"
    );
    let said = String::from_utf8_lossy(&o.stderr);
    assert!(
        said.contains("these new folders were made, and they stay: org/new"),
        "{said}"
    );
    d.folder.assert_locks_empty();
}

#[test]
fn a_move_that_fails_after_its_folders_logs_them_before_its_refusal() {
    let d = Door::new();
    d.folder.put(FILE, b"old\n");
    // With umask 222 the new folder comes without the right to write in it, so
    // the folder is made, and the link of the file into it fails. This works
    // on Linux and on macOS.
    fs::write(d.state.join("writes.log"), "").unwrap();
    let o = d.run_as(
        &["move", &d.p(FILE), &d.p("org/new/a.md"), "--who", "chat"],
        b"",
        None,
        Some("222"),
        &d.state.join("writes.log"),
    );
    let made = d.at("org/new").is_dir();
    let file = d.at("org/new/a.md").exists();
    writable_again(&d.folder.0);
    assert_eq!(o.status.code(), Some(1), "{o:?}");
    assert!(made, "the folder stays");
    assert!(!file, "nothing at the new name");
    assert_eq!(
        d.folder.get(FILE),
        b"old\n",
        "the file stays at its old name"
    );
    assert_eq!(
        d.words(),
        pairs(&[("made-folder", "org/new"), ("move-failed", "org/new/a.md")]),
        "the made-folder line first, then the refusal"
    );
    let said = String::from_utf8_lossy(&o.stderr);
    assert!(
        said.contains("these new folders were made, and they stay: org/new"),
        "{said}"
    );
    d.folder.assert_locks_empty();
}

#[test]
fn a_made_folder_line_that_fails_gives_exit_8_and_says_so() {
    let d = Door::new();
    // A folder in place of the log file: no line can go in.
    let log = d.state.join("log-is-a-folder");
    fs::create_dir_all(&log).unwrap();
    let o = d.run_as(
        &["write", &d.p("org/new/a.md"), "--who", "chat"],
        b"abc",
        None,
        None,
        &log,
    );
    assert_eq!(o.status.code(), Some(8), "{o:?}");
    assert_eq!(fs::read(d.at("org/new/a.md")).unwrap(), b"abc", "written");
    let said = String::from_utf8_lossy(&o.stderr);
    assert!(
        said.contains("the log line failed") && said.contains("\tmade-folder\torg/new\tchat"),
        "the failed made-folder line is named: {said}"
    );
}
