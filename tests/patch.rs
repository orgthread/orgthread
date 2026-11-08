//! The patch command: what an agent sends, the exit codes, and the log.

use std::fs;
use std::io::{ErrorKind, Write};
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
        let root = std::env::temp_dir().join(format!("orgthread-patch-{}-{n}", std::process::id()));
        fs::create_dir_all(root.join("org/.locks")).unwrap();
        fs::create_dir_all(root.join("org/notes")).unwrap();
        fs::create_dir_all(root.with_extension("outside")).unwrap();
        Place {
            root: fs::canonicalize(root).unwrap(),
        }
    }
    fn file(&self) -> PathBuf {
        self.root.join("org/notes/invented.md")
    }
    fn f(&self) -> String {
        self.file().to_str().unwrap().to_string()
    }
    fn log(&self) -> PathBuf {
        self.root.join("log/writes.log")
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_orgthread"));
        c.args(args)
            .env("ORGTHREAD_ROOT", &self.root)
            .env("ORGTHREAD_LOCK_DIR", self.root.join("org/.locks"))
            .env("ORGTHREAD_LOG", self.log())
            .env(
                "ORGTHREAD_READS",
                self.root.with_extension("outside").join("reads"),
            )
            .env(
                "ORGTHREAD_VERSIONS",
                self.root.with_extension("outside").join("versions"),
            );
        c
    }
    fn run(&self, args: &[&str], stdin: &[u8]) -> Output {
        let mut child = self
            .command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // A command that refuses early never reads its input and closes the pipe.
        // That is fine: the exit code says what happened.
        match child.stdin.take().unwrap().write_all(stdin) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::BrokenPipe => {}
            Err(e) => panic!("{e}"),
        }
        child.wait_with_output().unwrap()
    }
    fn read(&self, who: &str) -> Output {
        self.run(&["read", &self.f(), "--who", who], b"")
    }
    fn write(&self, who: &str, text: &[u8]) -> Output {
        self.run(&["write", &self.f(), "--who", who], text)
    }
    fn text(&self) -> Vec<u8> {
        fs::read(self.file()).unwrap()
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
        let _ = fs::remove_dir_all(self.root.with_extension("outside"));
    }
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

impl Place {
    fn patch(&self, who: &str, json: &str) -> Output {
        self.run(&["patch", &self.f(), "--who", who], json.as_bytes())
    }
    fn put(&self, text: &str) {
        fs::write(self.file(), text).unwrap();
    }
}

const CSV: &str = "date,place,km,notes\n2026-11-05,park,4.2,\n2026-11-06,river,6.0,\"windy, cold\"\n2026-11-07,park,3.8,\n";

const EDIT_AND_APPEND: &str = r#"{"edits":[{"old":"2026-11-07,park,3.8,\n","new":"2026-11-07,park,4.1,\n"}],"append":"2026-11-08,forest,7.5,\n"}"#;

#[test]
fn read_then_patch_changes_only_what_it_names() {
    let p = Place::new();
    p.put(CSV);
    assert_eq!(code(&p.read("a")), 0);
    let o = p.patch("a", EDIT_AND_APPEND);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(
        out(&o),
        "patched: org/notes/invented.md, 1 edit, 1 append, 120 bytes\n"
    );
    assert_eq!(
        String::from_utf8(p.text()).unwrap(),
        "date,place,km,notes\n2026-11-05,park,4.2,\n2026-11-06,river,6.0,\"windy, cold\"\n2026-11-07,park,4.1,\n2026-11-08,forest,7.5,\n"
    );
    let log = fs::read_to_string(p.log()).unwrap();
    assert!(
        log.contains("\tpatched\t") || log.contains(" patched "),
        "{log}"
    );
}

#[test]
fn an_unread_file_is_refused_with_exit_5() {
    let p = Place::new();
    p.put(CSV);
    let o = p.patch("a", EDIT_AND_APPEND);
    assert_eq!(code(&o), 5, "{}", err(&o));
    assert!(err(&o).contains("has not read"), "{}", err(&o));
    assert_eq!(p.text(), CSV.as_bytes());
    assert!(fs::read_to_string(p.log()).unwrap().contains("not-read"));
}

#[test]
fn the_read_of_another_name_does_not_count() {
    let p = Place::new();
    p.put(CSV);
    p.read("b");
    assert_eq!(code(&p.patch("a", EDIT_AND_APPEND)), 5);
}

#[test]
fn an_edit_that_does_not_fit_gets_exit_11_and_writes_nothing() {
    let p = Place::new();
    p.put(CSV);
    p.read("a");
    let o = p.patch(
        "a",
        r#"{"edits":[{"old":"2026-11-05,park,4.2,\n","new":"2026-11-05,park,4.3,\n"},{"old":"gone\n","new":"x\n"}],"append":"y\n"}"#,
    );
    assert_eq!(code(&o), 11, "{}", err(&o));
    assert!(
        err(&o).contains("refused, nothing written. Edit 2: the old text is not in the file."),
        "{}",
        err(&o)
    );
    assert_eq!(p.text(), CSV.as_bytes());
    assert!(fs::read_to_string(p.log()).unwrap().contains("misfit"));
}

#[test]
fn a_change_elsewhere_since_the_read_does_no_harm() {
    let p = Place::new();
    p.put(CSV);
    p.read("a");
    // A person changes another row after the read.
    p.put(&CSV.replace("4.2", "4.4"));
    let o = p.patch("a", EDIT_AND_APPEND);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let text = String::from_utf8(p.text()).unwrap();
    assert!(
        text.contains("2026-11-05,park,4.4,") && text.contains("2026-11-07,park,4.1,"),
        "{text}"
    );
}

#[test]
fn after_a_patch_a_write_and_a_patch_need_no_new_read() {
    let p = Place::new();
    p.put("a\n");
    p.read("a");
    assert_eq!(code(&p.patch("a", r#"{"append":"b\n"}"#)), 0);
    assert_eq!(code(&p.patch("a", r#"{"append":"c\n"}"#)), 0);
    assert_eq!(code(&p.write("a", b"d\n")), 0);
    assert_eq!(p.text(), b"d\n");
}

#[test]
fn the_same_patch_twice_is_refused_with_a_hint() {
    let p = Place::new();
    p.put(CSV);
    p.read("a");
    let twice = r#"{"edits":[{"old":"2026-11-07,park,3.8,\n","new":"2026-11-07,park,3.8,\n2026-11-08,forest,7.5,\n"}]}"#;
    assert_eq!(code(&p.patch("a", twice)), 0);
    let o = p.patch("a", twice);
    assert_eq!(code(&o), 11);
    assert!(
        err(&o).contains("may have gone through already"),
        "{}",
        err(&o)
    );
    assert_eq!(
        String::from_utf8(p.text())
            .unwrap()
            .matches("2026-11-08")
            .count(),
        1
    );
}

#[test]
fn a_line_that_differs_gives_a_note() {
    let p = Place::new();
    p.put(CSV);
    p.read("a");
    let o = p.patch(
        "a",
        r#"{"edits":[{"old":"2026-11-05,park,4.2,\n","new":"2026-11-05,park,4.3,\n","line":9}]}"#,
    );
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(
        out(&o).ends_with("note: edit 1 found at line 2, not 9\n"),
        "{}",
        out(&o)
    );
}

#[test]
fn a_file_gone_since_the_read_gives_exit_6() {
    let p = Place::new();
    p.put(CSV);
    p.read("a");
    fs::remove_file(p.file()).unwrap();
    let o = p.patch("a", EDIT_AND_APPEND);
    assert_eq!(code(&o), 6, "{}", err(&o));
    assert!(!p.file().exists());
}

#[test]
fn a_patch_that_empties_the_file_is_refused() {
    let p = Place::new();
    p.put("only\n");
    p.read("a");
    let o = p.patch("a", r#"{"edits":[{"old":"only\n","new":""}]}"#);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert!(
        err(&o).contains("the text after the patch is empty"),
        "{}",
        err(&o)
    );
    assert_eq!(p.text(), b"only\n");
}

#[test]
fn wrong_use_gives_exit_2_and_changes_nothing() {
    let p = Place::new();
    p.put(CSV);
    p.read("a");
    for json in ["", "{}", "not json", r#"{"edits":[{"old":"a"}]}"#] {
        let o = p.patch("a", json);
        assert_eq!(code(&o), 2, "{json}: {}", err(&o));
    }
    let o = p.run(
        &["patch", &p.f(), "--who", "a", "--size", "3"],
        EDIT_AND_APPEND.as_bytes(),
    );
    assert_eq!(code(&o), 2);
    assert!(err(&o).contains("may be cut off"), "{}", err(&o));
    let o = p.run(&["patch", "--who", "a"], b"");
    assert!(err(&o).contains("patch needs a file"), "{}", err(&o));
    assert_eq!(p.text(), CSV.as_bytes());
}

#[test]
fn a_held_lock_gives_exit_3() {
    let p = Place::new();
    p.put(CSV);
    p.read("a");
    let lock = p
        .root
        .join("org/.locks")
        .join(orgthread::write::lock_name("org/notes/invented.md"));
    fs::write(&lock, "another writer\n").unwrap();
    assert_eq!(code(&p.patch("a", EDIT_AND_APPEND)), 3);
    assert_eq!(p.text(), CSV.as_bytes());
}
