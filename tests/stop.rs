//! A real signal during a write. This test file has one test, as a signal goes
//! to the whole test process.

use orgthread::fingerprint::fingerprint;
use orgthread::stop;
use orgthread::storage::{Expect, LocalFolder, Storage, WriteError};
use orgthread::write::{Outcome, lock_name, write_unless_stopped};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::low_level::raise;
use std::fs;
use std::io;
use std::sync::atomic::Ordering;
use std::time::Duration;

const FILE: &str = "org/notes/invented.md";

/// Raises a signal when the file is read, as if it came during a slow read.
struct SignalOnRead {
    inner: LocalFolder,
    signal: i32,
}

impl Storage for SignalOnRead {
    fn read(&self, path: &str) -> io::Result<Option<Vec<u8>>> {
        raise(self.signal).unwrap();
        self.inner.read(path)
    }
    fn write_if_unchanged(&self, p: &str, e: &Expect, b: &[u8]) -> Result<(), WriteError> {
        self.inner.write_if_unchanged(p, e, b)
    }
    fn restore(&self, p: &str, o: Option<&[u8]>) -> io::Result<()> {
        self.inner.restore(p, o)
    }
    fn rescue(&self, p: &str, b: &[u8]) -> io::Result<String> {
        self.inner.rescue(p, b)
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
        self.inner.lock_remove(n)
    }
}

#[test]
fn a_stop_during_a_write_changes_nothing_and_releases_the_lock() {
    let root = std::env::temp_dir().join(format!("orgthread-stop-{}", std::process::id()));
    fs::create_dir_all(root.join("org/.locks")).unwrap();
    fs::create_dir_all(root.join("org/notes")).unwrap();
    fs::write(root.join(FILE), "old\n").unwrap();

    let flag = stop::arm().unwrap();
    for signal in [SIGINT, SIGTERM, SIGHUP] {
        flag.store(false, Ordering::SeqCst);
        let storage = SignalOnRead {
            inner: LocalFolder::new(&root, "org/.locks"),
            signal,
        };
        let expect = Expect::Fingerprint(fingerprint(b"old\n"));
        let r = write_unless_stopped(&storage, FILE, &expect, b"new\n", "test", &flag);
        assert!(matches!(r.outcome, Outcome::Failed { .. }), "{r:?}");
        assert_eq!(fs::read(root.join(FILE)).unwrap(), b"old\n");
        assert!(!root.join("org/.locks").join(lock_name(FILE)).exists());
    }
    fs::remove_dir_all(&root).unwrap();
}
