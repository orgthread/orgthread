//! The command itself: its exit codes, its output and its log line.

use orgthread::fingerprint::fingerprint;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

struct Place {
    root: PathBuf,
}

impl Place {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("orgthread-cli-{}-{n}", std::process::id()));
        fs::create_dir_all(root.join("org/.locks")).unwrap();
        fs::create_dir_all(root.join("org/notes")).unwrap();
        Place {
            root: fs::canonicalize(root).unwrap(),
        }
    }
    fn file(&self) -> PathBuf {
        self.root.join("org/notes/invented.md")
    }
    fn log(&self) -> PathBuf {
        self.root.join("log/writes.log")
    }
    fn run(&self, args: &[&str], stdin: &[u8]) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_orgthread"))
            .args(args)
            .env("ORGTHREAD_ROOT", &self.root)
            .env("ORGTHREAD_LOG", self.log())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin).unwrap();
        child.wait_with_output().unwrap()
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap()
}

#[test]
fn new_then_change_then_refusals() {
    let p = Place::new();
    let file = p.file();
    let f = file.to_str().unwrap();

    let o = p.run(
        &["write", f, "--new", "--who", "test chat"],
        "Grüße\n".as_bytes(),
    );
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(fs::read(&file).unwrap(), "Grüße\n".as_bytes());

    let o = p.run(&["fingerprint", f], b"");
    let print = String::from_utf8(o.stdout).unwrap();
    assert_eq!(print.trim(), fingerprint("Grüße\n".as_bytes()));

    let o = p.run(
        &["write", f, "--expect", print.trim(), "--who", "test chat"],
        b"second",
    );
    assert_eq!(code(&o), 0, "{o:?}");

    // The same fingerprint again: the file changed since.
    let o = p.run(
        &["write", f, "--expect", print.trim(), "--who", "test chat"],
        b"third",
    );
    assert_eq!(code(&o), 4, "{o:?}");
    assert_eq!(fs::read(&file).unwrap(), b"second");

    let o = p.run(&["write", f, "--new", "--who", "test chat"], b"third");
    assert_eq!(code(&o), 5, "{o:?}");

    let lines = fs::read_to_string(p.log()).unwrap();
    let words: Vec<_> = lines
        .lines()
        .map(|l| l.split('\t').nth(1).unwrap())
        .collect();
    assert_eq!(words, ["written", "written", "changed", "exists"]);
    let first: Vec<_> = lines.lines().next().unwrap().split('\t').collect();
    assert_eq!(first[2], "org/notes/invented.md");
    assert_eq!(first[3], "test chat");
    assert_eq!(first[4], "-");
    assert_eq!(first[6], "8");
    assert!(!lines.contains("Grüße"), "the log never holds the text");
}

#[test]
fn a_held_lock_gives_exit_3() {
    let p = Place::new();
    let file = p.file();
    fs::write(&file, "old\n").unwrap();
    let lock = p.root.join("org/.locks/org--notes--invented.md.lock");
    fs::write(&lock, "other writer\n").unwrap();
    let print = fingerprint(b"old\n");
    let o = p.run(
        &[
            "write",
            file.to_str().unwrap(),
            "--expect",
            &print,
            "--who",
            "t",
        ],
        b"new\n",
    );
    assert_eq!(code(&o), 3, "{o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("other writer"));
    assert_eq!(fs::read(&file).unwrap(), b"old\n");
    assert!(lock.exists());
}

#[test]
fn a_missing_file_gives_exit_6() {
    let p = Place::new();
    let f = p.file();
    let print = fingerprint(b"old\n");
    let o = p.run(
        &[
            "write",
            f.to_str().unwrap(),
            "--expect",
            &print,
            "--who",
            "t",
        ],
        b"new\n",
    );
    assert_eq!(code(&o), 6, "{o:?}");
    assert_eq!(code(&p.run(&["fingerprint", f.to_str().unwrap()], b"")), 6);
}

#[test]
fn wrong_use_gives_exit_2_and_changes_nothing() {
    let p = Place::new();
    let file = p.file();
    fs::write(&file, "old\n").unwrap();
    let f = file.to_str().unwrap();
    let print = fingerprint(b"old\n");
    let bad: Vec<(Vec<&str>, &[u8])> = vec![
        (vec![], b"x"),
        (vec!["write", f, "--who", "t"], b"x"),
        (vec!["write", f, "--new"], b"x"),
        (
            vec!["write", f, "--new", "--expect", &print, "--who", "t"],
            b"x",
        ),
        (vec!["write", f, "--expect", "short", "--who", "t"], b"x"),
        (
            vec!["write", f, "--expect", &print, "--who", "two\nlines"],
            b"x",
        ),
        (vec!["write", f, "--expect", &print, "--who", "t"], b""),
        (
            vec!["write", f, "--expect", &print, "--who", "t"],
            b"bad \xff",
        ),
    ];
    for (args, stdin) in bad {
        let o = p.run(&args, stdin);
        assert_eq!(code(&o), 2, "{args:?} {o:?}");
    }
    assert_eq!(fs::read(&file).unwrap(), b"old\n");
}

#[test]
fn a_path_outside_the_root_is_refused() {
    let p = Place::new();
    let outside = std::env::temp_dir().join(format!("orgthread-outside-{}.md", std::process::id()));
    let o = p.run(
        &["write", outside.to_str().unwrap(), "--new", "--who", "t"],
        b"x",
    );
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(!outside.exists());

    let climbing = p.root.join("org/notes/../../../orgthread-climb.md");
    let o = p.run(
        &["write", climbing.to_str().unwrap(), "--new", "--who", "t"],
        b"x",
    );
    assert_eq!(code(&o), 2, "{o:?}");
}

#[cfg(unix)]
#[test]
fn a_link_to_a_file_outside_the_root_is_refused() {
    let p = Place::new();
    let outside = std::env::temp_dir().join(format!("orgthread-target-{}.md", std::process::id()));
    fs::write(&outside, "outside\n").unwrap();
    let link = p.file();
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let print = fingerprint(b"outside\n");
    let o = p.run(
        &[
            "write",
            link.to_str().unwrap(),
            "--expect",
            &print,
            "--who",
            "t",
        ],
        b"x",
    );
    assert_eq!(code(&o), 2, "{o:?}");
    assert_eq!(fs::read(&outside).unwrap(), b"outside\n");
    fs::remove_file(&outside).unwrap();
}

#[test]
fn a_folder_is_refused() {
    let p = Place::new();
    let dir = p.root.join("org/notes");
    let o = p.run(
        &["write", dir.to_str().unwrap(), "--new", "--who", "t"],
        b"x",
    );
    assert_eq!(code(&o), 2, "{o:?}");
}

#[test]
fn a_log_that_cannot_be_written_gives_exit_8_after_the_write() {
    let p = Place::new();
    let file = p.file();
    // A file stands where the log folder should be.
    fs::write(p.root.join("log"), "in the way").unwrap();
    let o = p.run(
        &["write", file.to_str().unwrap(), "--new", "--who", "t"],
        b"new\n",
    );
    assert_eq!(code(&o), 8, "{o:?}");
    assert_eq!(fs::read(&file).unwrap(), b"new\n");
    assert!(
        fs::read_dir(p.root.join("org/.locks"))
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn a_wrong_size_is_refused() {
    let p = Place::new();
    let file = p.file();
    fs::write(&file, "the whole long text\n").unwrap();
    let print = fingerprint(b"the whole long text\n");
    let f = file.to_str().unwrap();
    let o = p.run(
        &[
            "write", f, "--expect", &print, "--who", "t", "--size", "100",
        ],
        b"cut",
    );
    assert_eq!(code(&o), 2, "{o:?}");
    assert_eq!(fs::read(&file).unwrap(), b"the whole long text\n");
    let o = p.run(
        &["write", f, "--expect", &print, "--who", "t", "--size", "3"],
        b"cut",
    );
    assert_eq!(code(&o), 0, "{o:?}");
}

#[test]
fn a_closed_output_still_gives_the_real_outcome_and_the_log() {
    let p = Place::new();
    let file = p.file();
    let mut child = Command::new(env!("CARGO_BIN_EXE_orgthread"))
        .args(["write", file.to_str().unwrap(), "--new", "--who", "t"])
        .env("ORGTHREAD_ROOT", &p.root)
        .env("ORGTHREAD_LOG", p.log())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    child.stdin.take().unwrap().write_all(b"new\n").unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(0));
    assert_eq!(fs::read(&file).unwrap(), b"new\n");
    assert!(fs::read_to_string(p.log()).unwrap().contains("written"));
}
