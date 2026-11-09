//! The command `move` on a local folder, on invented files.

use orgthread::fingerprint::fingerprint;
use std::fs;
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
        let root =
            std::env::temp_dir().join(format!("orgthread-move-cli-{}-{n}", std::process::id()));
        fs::create_dir_all(root.join("org/.locks")).unwrap();
        fs::create_dir_all(root.join("org/notes")).unwrap();
        fs::create_dir_all(root.with_extension("outside")).unwrap();
        Place {
            root: fs::canonicalize(root).unwrap(),
        }
    }
    fn at(&self, path: &str) -> PathBuf {
        self.root.join(path)
    }
    fn p(&self, path: &str) -> String {
        self.at(path).to_str().unwrap().to_string()
    }
    fn put(&self, path: &str, bytes: &[u8]) {
        fs::write(self.at(path), bytes).unwrap();
    }
    fn log(&self) -> String {
        fs::read_to_string(self.root.join("log/writes.log")).unwrap_or_default()
    }
    fn last_log(&self) -> Vec<String> {
        let log = self.log();
        let line = log.lines().last().expect("no log line");
        line.split('\t').map(str::to_string).collect()
    }
    fn run_with(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_orgthread"))
            .args(args)
            .env("ORGTHREAD_ROOT", &self.root)
            .env("ORGTHREAD_LOCK_DIR", self.root.join("org/.locks"))
            .env("ORGTHREAD_LOG", self.root.join("log/writes.log"))
            .env(
                "ORGTHREAD_READS",
                self.root.with_extension("outside").join("reads"),
            )
            .env(
                "ORGTHREAD_VERSIONS",
                self.root.with_extension("outside").join("versions"),
            )
            .env_remove("ORGTHREAD_ALLOW")
            .env_remove("ORGTHREAD_DENY")
            .env_remove("ORGTHREAD_READ_ONLY")
            .env_remove("ORGTHREAD_BINARY_ALLOW")
            .env_remove("ORGTHREAD_MOVE_ALLOW")
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .unwrap()
    }
    fn mv_with(&self, from: &str, to: &str, env: &[(&str, &str)]) -> Output {
        self.run_with(&["move", &self.p(from), &self.p(to), "--who", "chat"], env)
    }
    fn mv(&self, from: &str, to: &str) -> Output {
        self.mv_with(from, to, &[])
    }
    fn with_print(&self, from: &str, to: &str, print: &str) -> Output {
        let args = [
            "move",
            &self.p(from),
            &self.p(to),
            "--who",
            "chat",
            "--fingerprint",
            print,
        ];
        self.run_with(&args, &[])
    }
    fn locks_empty(&self) -> bool {
        fs::read_dir(self.root.join("org/.locks")).unwrap().count() == 0
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

fn said(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

#[test]
fn a_move_makes_and_names_its_folders_keeps_every_byte_and_logs_the_old_name() {
    let p = Place::new();
    p.put("org/notes/a.md", "Grüße\n".as_bytes());
    let o = p.mv("org/notes/a.md", "org/new/deep/a.md");
    assert_eq!(code(&o), 0, "{o:?}");
    let print = fingerprint("Grüße\n".as_bytes());
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        format!(
            "moved: org/notes/a.md -> org/new/deep/a.md, 8 bytes, {print}, new folders: org/new, org/new/deep\n"
        )
    );
    assert_eq!(
        fs::read(p.at("org/new/deep/a.md")).unwrap(),
        "Grüße\n".as_bytes()
    );
    assert!(!p.at("org/notes/a.md").exists(), "the old name is gone");
    assert_eq!(
        p.last_log()[1..],
        [
            "moved",
            "org/new/deep/a.md",
            "chat",
            "-",
            print.as_str(),
            "8",
            "/org/notes/a.md"
        ]
    );
    assert!(p.locks_empty());
    // The way back is a move back, into a folder that exists: no folder named.
    let o = p.mv("org/new/deep/a.md", "org/notes/a.md");
    assert_eq!(code(&o), 0, "{o:?}");
    assert!(
        !String::from_utf8_lossy(&o.stdout).contains("new folders"),
        "{o:?}"
    );
    assert_eq!(
        fs::read(p.at("org/notes/a.md")).unwrap(),
        "Grüße\n".as_bytes()
    );
}

#[test]
fn a_new_name_that_exists_is_never_replaced_and_gets_exit_5() {
    let p = Place::new();
    p.put("org/notes/a.md", b"new\n");
    p.put("org/notes/b.md", b"old\n");
    let o = p.mv("org/notes/a.md", "org/notes/b.md");
    assert_eq!(code(&o), 5, "{o:?}");
    assert!(said(&o).contains("refused, nothing moved."), "{}", said(&o));
    assert_eq!(fs::read(p.at("org/notes/b.md")).unwrap(), b"old\n");
    assert_eq!(fs::read(p.at("org/notes/a.md")).unwrap(), b"new\n");
    assert_eq!(p.last_log()[1], "move-exists");
    // A folder at the new name counts the same, and so does the same name.
    fs::create_dir_all(p.at("org/notes/sub")).unwrap();
    assert_eq!(code(&p.mv("org/notes/a.md", "org/notes/sub")), 5);
    assert_eq!(code(&p.mv("org/notes/a.md", "org/notes/a.md")), 5);
    assert_eq!(fs::read(p.at("org/notes/a.md")).unwrap(), b"new\n");
    assert_eq!(p.log().lines().count(), 3, "every refusal logs a line");
    assert!(p.locks_empty());
}

#[test]
fn a_fingerprint_of_another_version_moves_nothing_and_gets_exit_4() {
    let p = Place::new();
    p.put("org/notes/a.md", b"edited\n");
    let seen = fingerprint(b"seen\n");
    let o = p.with_print("org/notes/a.md", "org/done/a.md", &seen);
    assert_eq!(code(&o), 4, "{o:?}");
    assert!(p.at("org/notes/a.md").exists());
    assert!(!p.at("org/done").exists(), "a refusal makes no folder");
    let line = p.last_log();
    assert_eq!(line[1], "move-changed");
    assert_eq!(line[4], seen);
    assert_eq!(line[5], fingerprint(b"edited\n"));
    // The right fingerprint moves, and without one the file moves as it is.
    let right = fingerprint(b"edited\n");
    assert_eq!(
        code(&p.with_print("org/notes/a.md", "org/done/a.md", &right)),
        0
    );
    assert_eq!(code(&p.mv("org/done/a.md", "org/notes/a.md")), 0);
    assert!(p.locks_empty());
}

#[test]
fn an_empty_fingerprint_is_wrong_use_and_logs_nothing() {
    let p = Place::new();
    p.put("org/notes/a.md", b"text\n");
    for empty in ["", "   "] {
        let o = p.with_print("org/notes/a.md", "org/notes/b.md", empty);
        assert_eq!(code(&o), 2, "{o:?}");
        assert!(
            said(&o).contains("--fingerprint needs the fingerprint of the file, and it is empty"),
            "{}",
            said(&o)
        );
    }
    assert!(p.at("org/notes/a.md").exists());
    assert_eq!(p.log(), "", "no log line");
}

#[test]
fn no_file_gives_exit_6_and_a_folder_exit_2_with_a_log_line() {
    let p = Place::new();
    let o = p.mv("org/notes/gone.md", "org/notes/b.md");
    assert_eq!(code(&o), 6, "{o:?}");
    assert_eq!(p.last_log()[1], "move-missing");
    fs::create_dir_all(p.at("org/notes/sub")).unwrap();
    p.put("org/notes/sub/x.md", b"text\n");
    let o = p.mv("org/notes/sub", "org/other");
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(said(&o).contains("only a file is moved"), "{}", said(&o));
    assert_eq!(p.last_log()[1], "move-folder");
    assert!(p.at("org/notes/sub/x.md").exists());
    assert!(!p.at("org/other").exists());
}

#[test]
fn the_setting_needs_both_names_in_its_folders_and_without_it_there_is_no_limit() {
    let p = Place::new();
    for folder in ["org/health/diet", "org/health/log", "org/notes"] {
        fs::create_dir_all(p.at(folder)).unwrap();
    }
    p.put("org/health/diet/d.md", b"diet\n");
    p.put("org/notes/n.md", b"note\n");
    let allow = [("ORGTHREAD_MOVE_ALLOW", "/org/health/diet,/org/health/log")];
    // Inside: the old name in one folder, the new name in another, and a new folder.
    let o = p.mv_with("org/health/diet/d.md", "org/health/log/2026/d.md", &allow);
    assert_eq!(code(&o), 0, "{o:?}");
    // The old name outside, or the new name outside: refused, exit 2, and logged.
    let o = p.mv_with("org/notes/n.md", "org/health/diet/n.md", &allow);
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(said(&o).contains("ORGTHREAD_MOVE_ALLOW"), "{}", said(&o));
    let o = p.mv_with("org/health/log/2026/d.md", "org/notes/d.md", &allow);
    assert_eq!(code(&o), 2, "{o:?}");
    assert_eq!(p.last_log()[1], "move-not-allowed");
    assert!(p.at("org/notes/n.md").exists());
    assert!(p.at("org/health/log/2026/d.md").exists());
    assert!(!p.at("org/notes/d.md").exists());
    // Not set: no limit beyond the door.
    assert_eq!(code(&p.mv("org/notes/n.md", "org/elsewhere/n.md")), 0);
}

#[test]
fn a_renamed_markdown_file_adds_a_line_on_wikilinks() {
    let p = Place::new();
    p.put("org/notes/plan.md", b"text\n");
    let o = p.mv("org/notes/plan.md", "org/notes/goal.md");
    assert_eq!(code(&o), 0, "{o:?}");
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert_eq!(
        lines[1],
        "note: the file name changed, so wikilinks to [[plan]] break. Fix them to the new name."
    );
    // The same name in a new folder keeps its links.
    let o = p.mv("org/notes/goal.md", "org/2026/goal.md");
    assert_eq!(String::from_utf8_lossy(&o.stdout).lines().count(), 1);
}

#[test]
fn both_names_pass_the_write_limits_of_the_door() {
    let p = Place::new();
    p.put("org/notes/a.md", b"text\n");
    fs::create_dir_all(p.at("org/closed")).unwrap();
    p.put("org/closed/c.md", b"text\n");
    let ro = [("ORGTHREAD_READ_ONLY", "org/closed")];
    // A read-only file may be copied, but never moved away: its name would go.
    let o = p.mv_with("org/closed/c.md", "org/notes/c.md", &ro);
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(said(&o).contains("read only"), "{}", said(&o));
    assert!(p.at("org/closed/c.md").exists());
    let o = p.mv_with("org/notes/a.md", "org/closed/new/a.md", &ro);
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(!p.at("org/closed/new").exists());
    let deny = [("ORGTHREAD_DENY", "org/closed")];
    assert_eq!(
        code(&p.mv_with("org/closed/c.md", "org/notes/c.md", &deny)),
        2
    );
    assert_eq!(p.log().matches("\tmove-not-allowed\t").count(), 3);
    assert!(p.at("org/notes/a.md").exists());
}

#[test]
fn a_file_that_is_not_text_goes_only_to_the_open_folders() {
    let p = Place::new();
    fs::create_dir_all(p.at("org/pictures")).unwrap();
    let picture = [0x89, b'P', b'N', b'G', 0, 1, 2, 3];
    p.put("org/notes/photo.jpg", &picture);
    let o = p.mv("org/notes/photo.jpg", "org/notes/b.jpg");
    assert_eq!(code(&o), 2, "{o:?}");
    assert_eq!(p.last_log()[1], "move-not-text");
    let open = [("ORGTHREAD_BINARY_ALLOW", "org/pictures")];
    assert_eq!(
        code(&p.mv_with("org/notes/photo.jpg", "org/pictures/b.jpg", &open)),
        0
    );
    assert_eq!(fs::read(p.at("org/pictures/b.jpg")).unwrap(), picture);
    // Locally the bytes count too: a picture with no extension stays out.
    p.put("org/notes/photo", &picture);
    assert_eq!(code(&p.mv("org/notes/photo", "org/notes/renamed")), 2);
    assert!(p.at("org/notes/photo").exists());
}

#[test]
fn a_busy_old_or_new_name_gives_exit_3() {
    let p = Place::new();
    p.put("org/notes/a.md", b"text\n");
    for lock in ["org--notes--a.md.lock", "org--notes--b.md.lock"] {
        let path = p.root.join("org/.locks").join(lock);
        fs::write(&path, "a writer\n").unwrap();
        let o = p.mv("org/notes/a.md", "org/notes/b.md");
        assert_eq!(code(&o), 3, "{lock}: {o:?}");
        assert_eq!(p.last_log()[1], "move-locked");
        fs::remove_file(&path).unwrap();
    }
    assert!(p.at("org/notes/a.md").exists());
    assert!(!p.at("org/notes/b.md").exists());
    assert!(p.locks_empty());
}

#[test]
fn two_names_and_a_who_are_needed() {
    let p = Place::new();
    for args in [
        vec!["move", "a", "--who", "chat"],
        vec!["move", "a", "b", "c", "--who", "chat"],
        vec!["move", "a", "b"],
        vec!["move", "a", "b", "--who", "chat", "--force"],
    ] {
        assert_eq!(code(&p.run_with(&args, &[])), 2, "{args:?}");
    }
    assert_eq!(p.log(), "");
}

#[test]
fn watch_shows_a_move_line_as_a_move() {
    let p = Place::new();
    p.put("delete", b"text\n");
    p.put("org/notes/b.md", b"old\n");
    assert_eq!(code(&p.mv("delete", "org/notes/b.md")), 5);
    assert_eq!(code(&p.mv("delete", "org/notes/c.md")), 0);
    let screen = p.run_with(&["watch", "--once"], &[]);
    let text = String::from_utf8_lossy(&screen.stdout).to_string();
    assert!(text.contains("moved"), "{text}");
    assert!(text.contains("move-exists"), "{text}");
    assert!(!text.contains("Nothing was deleted"), "{text}");
    assert!(!text.contains("Nothing was copied"), "{text}");
}
