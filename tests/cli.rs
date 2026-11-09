//! The command itself: what an agent types, the exit codes, and the log.

use orgthread::fingerprint::fingerprint;
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
        let root = std::env::temp_dir().join(format!("orgthread-cli-{}-{n}", std::process::id()));
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
    fn outside(&self, name: &str) -> PathBuf {
        self.root.with_extension("outside").join(name)
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

#[test]
fn a_new_file_needs_no_read() {
    let p = Place::new();
    let o = p.write("chat", "Grüße\n".as_bytes());
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(p.text(), "Grüße\n".as_bytes());
    let told = String::from_utf8(o.stdout).unwrap();
    assert_eq!(told, "written: org/notes/invented.md, 8 bytes\n");
}

#[test]
fn read_then_write_is_all_an_agent_does() {
    let p = Place::new();
    fs::write(p.file(), "old\n").unwrap();
    let o = p.read("chat");
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(o.stdout, b"old\n", "only the text, and nothing else");
    assert_eq!(code(&p.write("chat", b"new\n")), 0);
    assert_eq!(p.text(), b"new\n");
}

#[test]
fn an_agent_sees_no_fingerprint() {
    let p = Place::new();
    fs::write(p.file(), "old\n").unwrap();
    let hashes = [fingerprint(b"old\n"), fingerprint(b"new\n")];
    let mut outputs = vec![p.read("chat"), p.write("chat", b"new\n")];
    fs::write(p.file(), "edited\n").unwrap();
    outputs.push(p.write("chat", b"third\n"));
    for o in outputs {
        for stream in [&o.stdout, &o.stderr] {
            let text = String::from_utf8_lossy(stream);
            for hash in &hashes {
                assert!(!text.contains(hash.as_str()), "{text}");
            }
        }
    }
}

#[test]
fn an_edit_after_the_read_is_refused_and_survives() {
    let p = Place::new();
    fs::write(p.file(), "old\n").unwrap();
    p.read("chat");
    fs::write(p.file(), "typed in the editor\n").unwrap();
    let o = p.write("chat", b"built on the old text\n");
    assert_eq!(code(&o), 4, "{o:?}");
    assert_eq!(p.text(), b"typed in the editor\n");
    assert!(String::from_utf8_lossy(&o.stderr).contains("Read it again"));
    // And the way out is a new read.
    assert_eq!(p.read("chat").stdout, b"typed in the editor\n");
    assert_eq!(code(&p.write("chat", b"built on the new text\n")), 0);
}

#[test]
fn an_existing_file_that_was_not_read_is_refused() {
    let p = Place::new();
    fs::write(p.file(), "here already\n").unwrap();
    let o = p.write("chat", b"new\n");
    assert_eq!(code(&o), 5, "{o:?}");
    assert_eq!(p.text(), b"here already\n");
    assert!(String::from_utf8_lossy(&o.stderr).contains("Read it first"));
}

#[test]
fn the_read_of_another_agent_does_not_count() {
    let p = Place::new();
    fs::write(p.file(), "old\n").unwrap();
    p.read("chat a");
    assert_eq!(code(&p.write("chat b", b"new\n")), 5);
    assert_eq!(p.text(), b"old\n");
}

#[test]
fn two_agents_who_read_the_same_version_one_wins() {
    let p = Place::new();
    fs::write(p.file(), "old\n").unwrap();
    p.read("chat a");
    p.read("chat b");
    assert_eq!(code(&p.write("chat a", b"from a\n")), 0);
    assert_eq!(code(&p.write("chat b", b"from b\n")), 4);
    assert_eq!(p.text(), b"from a\n");
}

#[test]
fn a_second_write_needs_no_new_read() {
    let p = Place::new();
    assert_eq!(code(&p.write("chat", b"first\n")), 0);
    assert_eq!(code(&p.write("chat", b"second\n")), 0);
    assert_eq!(p.text(), b"second\n");
    fs::write(p.file(), "edited\n").unwrap();
    assert_eq!(code(&p.write("chat", b"third\n")), 4);
    assert_eq!(p.text(), b"edited\n");
}

#[test]
fn a_file_gone_since_the_read_gives_exit_6() {
    let p = Place::new();
    fs::write(p.file(), "old\n").unwrap();
    p.read("chat");
    fs::remove_file(p.file()).unwrap();
    assert_eq!(code(&p.write("chat", b"new\n")), 6);
    assert!(!p.file().exists());
}

#[test]
fn a_held_lock_gives_exit_3() {
    let p = Place::new();
    fs::write(p.file(), "old\n").unwrap();
    p.read("chat");
    let lock = p.root.join("org/.locks/org--notes--invented.md.lock");
    fs::write(&lock, "other writer\n").unwrap();
    let o = p.write("chat", b"new\n");
    assert_eq!(code(&o), 3, "{o:?}");
    let said = String::from_utf8_lossy(&o.stderr).to_lowercase();
    assert!(said.contains("other writer"), "{said}");
    assert!(
        !said.contains("lock"),
        "a chat need not know about locks: {said}"
    );
    assert_eq!(p.text(), b"old\n");
    assert!(lock.exists());
}

#[test]
fn read_gives_the_exact_bytes() {
    let p = Place::new();
    let text = "Grüße,  odd spacing\r\nno last newline";
    fs::write(p.file(), text).unwrap();
    assert_eq!(p.read("chat").stdout, text.as_bytes());
    let copy = p.outside("copy.md");
    let o = p.run(
        &[
            "read",
            &p.f(),
            "--who",
            "chat",
            "--to",
            copy.to_str().unwrap(),
        ],
        b"",
    );
    assert_eq!(code(&o), 0, "{o:?}");
    assert!(o.stdout.is_empty());
    assert_eq!(fs::read(&copy).unwrap(), text.as_bytes());
    assert_eq!(code(&p.write("chat", b"new\n")), 0, "a copy counts as read");
}

#[test]
fn read_refuses_what_it_should() {
    let p = Place::new();
    assert_eq!(code(&p.read("chat")), 6);
    assert_eq!(
        code(&p.run(&["read", &p.f()], b"")),
        2,
        "the name is required"
    );

    fs::write(p.file(), b"bad \xff bytes").unwrap();
    assert_eq!(code(&p.read("chat")), 2);

    fs::write(p.file(), b"good\n").unwrap();
    let inside = p.root.join("org/notes/copy.md");
    let o = p.run(
        &[
            "read",
            &p.f(),
            "--who",
            "c",
            "--to",
            inside.to_str().unwrap(),
        ],
        b"",
    );
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(!inside.exists());

    let copy = p.outside("taken.md");
    fs::write(&copy, "mine\n").unwrap();
    let o = p.run(
        &["read", &p.f(), "--who", "c", "--to", copy.to_str().unwrap()],
        b"",
    );
    assert_eq!(code(&o), 5, "{o:?}");
    assert_eq!(fs::read(&copy).unwrap(), b"mine\n");
    assert_eq!(
        code(&p.write("c", b"new\n")),
        5,
        "a refused read does not count"
    );

    let elsewhere = p.outside("elsewhere.md");
    fs::write(&elsewhere, "x\n").unwrap();
    let o = p.run(&["read", elsewhere.to_str().unwrap(), "--who", "c"], b"");
    assert_eq!(code(&o), 2, "{o:?}");
}

#[test]
fn an_empty_file_can_be_read_and_replaced() {
    let p = Place::new();
    fs::write(p.file(), b"").unwrap();
    let o = p.read("chat");
    assert_eq!(code(&o), 0, "{o:?}");
    assert!(o.stdout.is_empty());
    assert_eq!(code(&p.write("chat", b"new\n")), 0);
}

#[test]
fn wrong_use_gives_exit_2_and_changes_nothing() {
    let p = Place::new();
    fs::write(p.file(), "old\n").unwrap();
    p.read("t");
    let f = p.f();
    let bad: Vec<(Vec<&str>, &[u8])> = vec![
        (vec![], b"x"),
        (vec!["write", &f], b"x"),
        (vec!["write", "--who", "t"], b"x"),
        (vec!["write", &f, "--who", "two\nlines"], b"x"),
        (vec!["write", &f, "--who", " "], b"x"),
        (vec!["write", &f, "--who", "t", "--expect", "abc"], b"x"),
        (vec!["write", &f, "--who", "t"], b""),
        (vec!["write", &f, "--who", "t"], b"bad \xff"),
        (vec!["write", &f, "--who", "t", "--size", "100"], b"cut"),
    ];
    for (args, stdin) in bad {
        let o = p.run(&args, stdin);
        assert_eq!(code(&o), 2, "{args:?} {o:?}");
    }
    assert_eq!(p.text(), b"old\n");
    assert_eq!(
        code(&p.run(&["write", &f, "--who", "t", "--size", "4"], b"new\n")),
        0
    );
}

#[test]
fn a_path_outside_the_root_is_refused() {
    let p = Place::new();
    let outside = p.outside("o.md");
    let o = p.run(&["write", outside.to_str().unwrap(), "--who", "t"], b"x");
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(!outside.exists());

    let climbing = p.root.join("org/notes/../../../orgthread-climb.md");
    let o = p.run(&["write", climbing.to_str().unwrap(), "--who", "t"], b"x");
    assert_eq!(code(&o), 2, "{o:?}");
}

#[cfg(unix)]
#[test]
fn a_link_to_a_file_outside_the_root_is_refused() {
    let p = Place::new();
    let outside = p.outside("target.md");
    fs::write(&outside, "outside\n").unwrap();
    std::os::unix::fs::symlink(&outside, p.file()).unwrap();
    assert_eq!(code(&p.read("t")), 2);
    assert_eq!(code(&p.write("t", b"x")), 2);
    assert_eq!(fs::read(&outside).unwrap(), b"outside\n");
}

#[test]
fn a_folder_is_refused() {
    let p = Place::new();
    let dir = p.root.join("org/notes");
    let o = p.run(&["write", dir.to_str().unwrap(), "--who", "t"], b"x");
    assert_eq!(code(&o), 2, "{o:?}");
}

#[test]
fn each_try_leaves_one_log_line_without_the_text() {
    let p = Place::new();
    assert_eq!(code(&p.write("test chat", "Grüße\n".as_bytes())), 0);
    fs::write(p.file(), "edited\n").unwrap();
    assert_eq!(code(&p.write("test chat", b"x\n")), 4);
    assert_eq!(code(&p.write("other chat", b"x\n")), 5);

    let lines = fs::read_to_string(p.log()).unwrap();
    let words: Vec<_> = lines
        .lines()
        .map(|l| l.split('\t').nth(1).unwrap())
        .collect();
    assert_eq!(words, ["written", "changed", "exists"]);
    let first: Vec<_> = lines.lines().next().unwrap().split('\t').collect();
    assert_eq!(first[2], "org/notes/invented.md");
    assert_eq!(first[3], "test chat");
    assert_eq!(first[4], "-");
    assert_eq!(
        first[5],
        fingerprint("Grüße\n".as_bytes()),
        "the log keeps it for a person"
    );
    assert_eq!(first[6], "8");
    assert!(!lines.contains("Grüße"), "the log never holds the text");
}

#[test]
fn a_log_that_cannot_be_written_gives_exit_8_after_the_write() {
    let p = Place::new();
    // A file stands where the log folder should be.
    fs::write(p.root.join("log"), "in the way").unwrap();
    assert_eq!(code(&p.write("t", b"new\n")), 8);
    assert_eq!(p.text(), b"new\n");
    assert!(
        fs::read_dir(p.root.join("org/.locks"))
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn a_closed_output_still_gives_the_real_outcome_and_the_log() {
    let p = Place::new();
    let f = p.f();
    let mut child = p
        .command(&["write", &f, "--who", "t"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    child.stdin.take().unwrap().write_all(b"new\n").unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(0));
    assert_eq!(p.text(), b"new\n");
    assert!(fs::read_to_string(p.log()).unwrap().contains("written"));
}

#[test]
fn watch_once_shows_the_writes_and_a_held_lock() {
    let p = Place::new();
    assert_eq!(code(&p.write("t", b"new\n")), 0);
    fs::write(
        p.root.join("org/.locks/org--notes--other.md.lock"),
        "other writer\n",
    )
    .unwrap();
    let o = p.run(&["watch", "--once"], b"");
    assert_eq!(code(&o), 0, "{o:?}");
    let text = String::from_utf8(o.stdout).unwrap();
    assert!(text.contains("org/notes/invented.md"), "{text}");
    assert!(text.contains("written"), "{text}");
    assert!(text.contains("org/notes/other.md"), "{text}");
    assert!(text.contains("other writer"), "{text}");
}

#[test]
fn watch_once_starts_with_the_version_and_the_storage_and_no_secret() {
    const URL: &str = "https://example.com/invented-secret-path";
    const HASH: &str = "invented-login-hash-7f3a";
    let p = Place::new();
    let version = p.run(&["--version"], b"");
    let version = String::from_utf8(version.stdout).unwrap();
    let version = version.trim();
    assert!(version.starts_with("orgthread "), "{version}");
    let o = p
        .command(&["watch", "--once"])
        .env("ORGTHREAD_PUBLIC_URL", URL)
        .env("ORGTHREAD_LOGIN_HASH", HASH)
        .output()
        .unwrap();
    assert_eq!(code(&o), 0, "{o:?}");
    let text = String::from_utf8(o.stdout).unwrap();
    let first = text.lines().next().unwrap();
    assert!(
        first.starts_with(&format!("{version} · local folder")),
        "{first}"
    );
    for secret in ["example.com", "invented-secret-path", HASH] {
        assert!(!text.contains(secret), "{secret} shows: {text}");
    }
}

#[test]
fn watch_without_a_terminal_says_what_to_do_and_changes_nothing() {
    let p = Place::new();
    let o = p.run(&["watch"], b"");
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("--once"));
    assert!(
        fs::read_dir(p.root.join("org/.locks"))
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn a_refusal_before_the_text_is_taken_in_changes_nothing() {
    // The command refuses a bad path without reading its input. Text larger than
    // a pipe holds makes the sender always find the pipe closed.
    let p = Place::new();
    let outside = p.outside("o.md");
    let text = vec![b'x'; 4 * 1024 * 1024];
    let o = p.run(&["write", outside.to_str().unwrap(), "--who", "t"], &text);
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(!outside.exists());
}

#[test]
fn a_write_keeps_a_copy_of_the_old_and_the_new_version() {
    let p = Place::new();
    fs::write(p.file(), "old\n").unwrap();
    p.read("chat");
    assert_eq!(code(&p.write("chat", b"new\n")), 0);
    let versions = p.outside("versions");
    assert_eq!(
        fs::read(versions.join(fingerprint(b"old\n"))).unwrap(),
        b"old\n"
    );
    assert_eq!(
        fs::read(versions.join(fingerprint(b"new\n"))).unwrap(),
        b"new\n"
    );
}

#[test]
fn a_refused_write_keeps_no_copy() {
    let p = Place::new();
    fs::write(p.file(), "here already\n").unwrap();
    assert_eq!(code(&p.write("chat", b"new\n")), 5);
    assert!(!p.outside("versions").join(fingerprint(b"new\n")).exists());
}

#[test]
fn a_copy_that_cannot_be_kept_does_not_fail_the_write() {
    let p = Place::new();
    // A file stands where the folder of copies should be.
    fs::write(p.outside("versions"), "in the way").unwrap();
    let o = p.write("chat", b"new\n");
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(p.text(), b"new\n");
    assert!(String::from_utf8_lossy(&o.stderr).contains("could not keep a copy"));
}

/// The program alone in a folder beside the notes, with an invented home.
fn program_beside(p: &Place) -> PathBuf {
    let folder = p.root.with_extension("outside").join("program");
    fs::create_dir_all(&folder).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_orgthread"), folder.join("orgthread")).unwrap();
    fs::write(
        folder.join(".env"),
        format!("ORGTHREAD_ROOT={}\n", p.root.display()),
    )
    .unwrap();
    fs::canonicalize(folder).unwrap()
}

#[test]
fn a_local_folder_keeps_locks_and_log_beside_the_program_too() {
    let p = Place::new();
    let folder = program_beside(&p);
    fs::remove_dir_all(p.root.join("org/.locks")).unwrap();
    let run = |args: &[&str], text: &[u8]| {
        let mut child = Command::new(folder.join("orgthread"))
            .args(args)
            .env_clear()
            .env("HOME", p.outside("home"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let _ = child.stdin.take().unwrap().write_all(text);
        child.wait_with_output().unwrap()
    };
    let f = p.f();
    let o = run(&["write", &f, "--who", "chat"], b"new\n");
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(p.text(), b"new\n");
    assert!(!p.root.join("org/.locks").exists(), "no lock in the notes");
    assert_eq!(fs::read_dir(folder.join(".locks")).unwrap().count(), 0);
    let log = fs::read_to_string(folder.join("state/writes.log")).unwrap();
    assert!(log.contains("org/notes/invented.md"), "{log}");

    // A lock in the old place means nothing, and the one beside the program holds.
    fs::create_dir_all(p.root.join("org/.locks")).unwrap();
    let name = "org--notes--invented.md.lock";
    fs::write(p.root.join("org/.locks").join(name), "old place\n").unwrap();
    let o = run(&["write", &f, "--who", "chat"], b"next\n");
    assert_eq!(code(&o), 0, "{o:?}");
    fs::write(folder.join(".locks").join(name), "other writer\n").unwrap();
    let o = run(&["write", &f, "--who", "chat"], b"third\n");
    assert_eq!(code(&o), 3, "{o:?}");
    assert_eq!(p.text(), b"next\n");
    let o = run(&["watch", "--once"], b"");
    assert!(String::from_utf8_lossy(&o.stdout).contains("other writer"));
}

#[test]
fn list_and_ls_give_the_folder_and_its_exit_code() {
    let p = Place::new();
    fs::write(p.file(), "four").unwrap();
    let notes = p.root.join("org/notes");
    let notes = notes.to_str().unwrap();
    for command in ["list", "ls"] {
        let o = p
            .command(&[command, notes])
            .env("TZ", "Europe/Vienna")
            .output()
            .unwrap();
        assert_eq!(code(&o), 0, "{o:?}");
        let text = String::from_utf8(o.stdout).unwrap();
        assert!(text.starts_with(&format!("{notes}:\n")), "{text}");
        assert!(text.ends_with("  invented.md\n"), "{text}");
    }
    let o = p.command(&["ls", "--fingerprint", notes]).output().unwrap();
    assert_eq!(code(&o), 0, "{o:?}");
    let text = String::from_utf8(o.stdout).unwrap();
    let line = format!("  {}  invented.md\n", fingerprint(b"four"));
    assert!(text.ends_with(&line), "{text}");
    // The same fingerprint as the command fingerprint gives.
    let one = p
        .command(&["fingerprint", p.file().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        text.contains(String::from_utf8(one.stdout).unwrap().trim()),
        "{text}"
    );
}

#[test]
fn list_refuses_wrong_use_and_paths_outside_the_root() {
    let p = Place::new();
    let notes = p.root.join("org/notes");
    let notes = notes.to_str().unwrap();
    for args in [
        vec!["list"],
        vec!["list", notes, "--depth"],
        vec!["list", notes, "--depth", "0"],
        vec!["list", notes, "--depth", "two"],
        vec!["list", notes, "--deep"],
    ] {
        let o = p.command(&args).output().unwrap();
        assert_eq!(code(&o), 2, "{args:?}: {o:?}");
    }
    let outside = p.outside("");
    let o = p
        .command(&["list", outside.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(code(&o), 2, "{o:?}");
    let o = p.command(&["list", "--", "--all"]).output().unwrap();
    assert_eq!(code(&o), 6, "a folder named like an option: {o:?}");
}

#[test]
fn list_keeps_to_the_limits_of_the_door() {
    let p = Place::new();
    fs::write(p.file(), "four").unwrap();
    fs::create_dir_all(p.root.join("org/notes/private")).unwrap();
    fs::write(p.root.join("org/notes/private/x.md"), "secret").unwrap();
    let notes = p.root.join("org/notes");
    let notes = notes.to_str().unwrap();
    let o = p
        .command(&["list", notes, "--depth", "all"])
        .env("ORGTHREAD_DENY", "org/notes/private")
        .env("ORGTHREAD_READ_ONLY", "org/notes/invented.md")
        .output()
        .unwrap();
    assert_eq!(code(&o), 0, "{o:?}");
    let text = String::from_utf8(o.stdout).unwrap();
    assert!(!text.contains("  private/"), "{text}");
    assert!(text.contains("  invented.md  (read only)\n"), "{text}");
    let private = p.root.join("org/notes/private");
    let o = p
        .command(&["list", private.to_str().unwrap()])
        .env("ORGTHREAD_DENY", "org/notes/private")
        .output()
        .unwrap();
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(!String::from_utf8_lossy(&o.stdout).contains("x.md"));
}
