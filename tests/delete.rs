//! The command `delete` on a local folder, on invented files.

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
            std::env::temp_dir().join(format!("orgthread-delete-cli-{}-{n}", std::process::id()));
        fs::create_dir_all(root.join("org/.locks")).unwrap();
        fs::create_dir_all(root.join("org/notes")).unwrap();
        fs::create_dir_all(root.join("org/keep")).unwrap();
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
    fn log(&self) -> String {
        fs::read_to_string(self.root.join("log/writes.log")).unwrap_or_default()
    }
    fn versions(&self) -> PathBuf {
        self.root.with_extension("outside").join("versions")
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
            .env("ORGTHREAD_VERSIONS", self.versions())
            .env("ORGTHREAD_DELETE_ALLOW", "org/notes")
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .unwrap()
    }
    fn run(&self, args: &[&str]) -> Output {
        self.run_with(args, &[])
    }
    fn read(&self, who: &str) {
        assert_eq!(code(&self.run(&["read", &self.f(), "--who", who])), 0);
    }
    fn delete(&self, who: &str) -> Output {
        self.run(&["delete", &self.f(), "--who", who])
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
fn read_then_delete_removes_the_file_keeps_it_and_logs_it() {
    let p = Place::new();
    fs::write(p.file(), "Grüße\n").unwrap();
    p.read("chat");
    let o = p.delete("chat");
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        "deleted: org/notes/invented.md, 8 bytes. The last version is kept.\n"
    );
    assert!(!p.file().exists());
    let print = fingerprint("Grüße\n".as_bytes());
    assert_eq!(
        fs::read(p.versions().join(&print)).unwrap(),
        "Grüße\n".as_bytes()
    );
    let line = p.log().lines().last().unwrap().to_string();
    let fields: Vec<&str> = line.split('\t').collect();
    assert_eq!(
        fields[1..],
        [
            "deleted",
            "org/notes/invented.md",
            "chat",
            &print,
            "-",
            "8",
            "-"
        ]
    );
    assert_eq!(fs::read_dir(p.root.join("org/.locks")).unwrap().count(), 0);
}

#[test]
fn with_no_read_and_no_expect_nothing_is_deleted() {
    let p = Place::new();
    fs::write(p.file(), "text\n").unwrap();
    p.read("another chat");
    let o = p.delete("chat");
    assert_eq!(code(&o), 4, "{o:?}");
    assert!(p.file().exists());
    assert!(p.log().contains("\tnot-read\t"));
}

#[test]
fn a_file_changed_since_the_read_stays() {
    let p = Place::new();
    fs::write(p.file(), "seen\n").unwrap();
    p.read("chat");
    fs::write(p.file(), "edited by a person\n").unwrap();
    let o = p.delete("chat");
    assert_eq!(code(&o), 4, "{o:?}");
    assert_eq!(fs::read(p.file()).unwrap(), b"edited by a person\n");
    let o = p.run(&[
        "delete",
        &p.f(),
        "--who",
        "x",
        "--expect",
        &fingerprint(b"seen\n"),
    ]);
    assert_eq!(code(&o), 4, "{o:?}");
    assert!(p.file().exists());
}

#[test]
fn expect_works_without_a_read() {
    let p = Place::new();
    fs::write(p.file(), "text\n").unwrap();
    let o = p.run(&[
        "delete",
        &p.f(),
        "--who",
        "x",
        "--expect",
        &fingerprint(b"text\n"),
    ]);
    assert_eq!(code(&o), 0, "{o:?}");
    assert!(!p.file().exists());
}

#[test]
fn check_deletes_nothing() {
    let p = Place::new();
    fs::write(p.file(), "text\n").unwrap();
    p.read("chat");
    let o = p.run(&["delete", &p.f(), "--who", "chat", "--check"]);
    assert_eq!(code(&o), 0, "{o:?}");
    assert!(String::from_utf8_lossy(&o.stdout).starts_with("check passed, nothing deleted"));
    assert!(p.file().exists());
    fs::write(p.file(), "edited\n").unwrap();
    let o = p.run(&["delete", &p.f(), "--who", "chat", "--check"]);
    assert_eq!(code(&o), 4, "{o:?}");
    assert!(p.file().exists());
    assert!(!p.log().contains("deleted"));
}

#[test]
fn only_the_folders_of_the_setting_allow_a_delete() {
    let p = Place::new();
    let other = p.root.join("org/keep/invented.md");
    fs::write(&other, "text\n").unwrap();
    let path = other.to_str().unwrap();
    let print = fingerprint(b"text\n");
    let o = p.run(&["delete", path, "--who", "x", "--expect", &print]);
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("ORGTHREAD_DELETE_ALLOW"));
    // With the setting empty, every delete is refused.
    fs::write(p.file(), "text\n").unwrap();
    let args = ["delete", &p.f(), "--who", "x", "--expect", &print];
    let o = p.run_with(&args, &[("ORGTHREAD_DELETE_ALLOW", "")]);
    assert_eq!(code(&o), 2, "{o:?}");
    // The deny list and the read-only files still count.
    let o = p.run_with(&args, &[("ORGTHREAD_READ_ONLY", "org/notes")]);
    assert_eq!(code(&o), 2, "{o:?}");
    let o = p.run_with(&args, &[("ORGTHREAD_DENY", "org/notes/invented.md")]);
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(other.exists() && p.file().exists());
}

#[test]
fn a_folder_is_never_deleted() {
    let p = Place::new();
    let folder = p.root.join("org/notes/sub");
    fs::create_dir_all(&folder).unwrap();
    let args = [
        "delete",
        folder.to_str().unwrap(),
        "--who",
        "x",
        "--expect",
        "aa",
    ];
    let o = p.run(&args);
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(folder.is_dir());
}

#[test]
fn a_file_gone_since_the_read_gives_exit_6() {
    let p = Place::new();
    fs::write(p.file(), "text\n").unwrap();
    p.read("chat");
    fs::remove_file(p.file()).unwrap();
    assert_eq!(code(&p.delete("chat")), 6);
}
