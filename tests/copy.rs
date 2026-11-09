//! The command `copy` on a local folder, on invented files.

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
            std::env::temp_dir().join(format!("orgthread-copy-cli-{}-{n}", std::process::id()));
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
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .unwrap()
    }
    fn copy_with(&self, from: &str, to: &str, env: &[(&str, &str)]) -> Output {
        self.run_with(&["copy", &self.p(from), &self.p(to), "--who", "chat"], env)
    }
    fn copy(&self, from: &str, to: &str) -> Output {
        self.copy_with(from, to, &[])
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
fn a_copy_makes_its_folders_keeps_every_byte_and_logs_the_source() {
    let p = Place::new();
    p.put("org/notes/a.md", "Grüße\n".as_bytes());
    let o = p.copy("org/notes/a.md", "org/new/deep/b.md");
    assert_eq!(code(&o), 0, "{o:?}");
    let print = fingerprint("Grüße\n".as_bytes());
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        format!("copied: org/notes/a.md -> org/new/deep/b.md, 8 bytes, {print}\n")
    );
    assert_eq!(
        fs::read(p.at("org/new/deep/b.md")).unwrap(),
        "Grüße\n".as_bytes()
    );
    assert_eq!(
        p.last_log()[1..],
        [
            "copied",
            "org/new/deep/b.md",
            "chat",
            "-",
            print.as_str(),
            "8",
            "/org/notes/a.md"
        ]
    );
    assert!(p.locks_empty());
}

#[test]
fn a_copy_keeps_the_permissions_of_the_source() {
    use std::os::unix::fs::PermissionsExt;
    let p = Place::new();
    p.put("org/notes/run.sh", b"echo hi\n");
    fs::set_permissions(p.at("org/notes/run.sh"), fs::Permissions::from_mode(0o751)).unwrap();
    assert_eq!(code(&p.copy("org/notes/run.sh", "org/notes/run2.sh")), 0);
    let mode = fs::metadata(p.at("org/notes/run2.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o751);
}

#[test]
fn a_target_that_exists_is_never_replaced_and_gets_exit_5() {
    let p = Place::new();
    p.put("org/notes/a.md", b"new\n");
    p.put("org/notes/b.md", b"old\n");
    let o = p.copy("org/notes/a.md", "org/notes/b.md");
    assert_eq!(code(&o), 5, "{o:?}");
    assert!(
        said(&o).contains("refused, nothing copied."),
        "{}",
        said(&o)
    );
    assert_eq!(fs::read(p.at("org/notes/b.md")).unwrap(), b"old\n");
    assert_eq!(p.last_log()[1], "exists");
    // A folder at the new name counts the same.
    let o = p.copy("org/notes/a.md", "org/notes");
    assert_eq!(code(&o), 5, "{o:?}");
    assert!(p.locks_empty());
}

#[test]
fn no_source_gives_exit_6_and_a_folder_exit_2() {
    let p = Place::new();
    let o = p.copy("org/notes/gone.md", "org/notes/b.md");
    assert_eq!(code(&o), 6, "{o:?}");
    assert_eq!(p.last_log()[1], "missing");
    let o = p.copy("org/notes", "org/notes/b.md");
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(!p.at("org/notes/b.md").exists());
}

#[test]
fn a_source_of_another_version_is_refused_with_exit_4() {
    let p = Place::new();
    p.put("org/notes/a.md", b"edited\n");
    let seen = fingerprint(b"seen\n");
    let args = [
        "copy",
        &p.p("org/notes/a.md"),
        &p.p("org/notes/b.md"),
        "--who",
        "chat",
        "--fingerprint",
        &seen,
    ];
    let o = p.run_with(&args, &[]);
    assert_eq!(code(&o), 4, "{o:?}");
    assert!(!p.at("org/notes/b.md").exists());
    let line = p.last_log();
    assert_eq!(line[1], "changed");
    assert_eq!(line[4], seen);
    assert_eq!(line[5], fingerprint(b"edited\n"));
    // The right fingerprint copies.
    let right = fingerprint(b"edited\n");
    let mut args = args;
    args[6] = &right;
    assert_eq!(code(&p.run_with(&args, &[])), 0);
    assert!(p.locks_empty());
}

#[test]
fn the_limits_of_the_door_hold_for_both_ends() {
    let p = Place::new();
    p.put("org/notes/a.md", b"text\n");
    fs::create_dir_all(p.at("org/closed")).unwrap();
    p.put("org/closed/c.md", b"text\n");
    // A closed source.
    let o = p.copy_with(
        "org/closed/c.md",
        "org/notes/b.md",
        &[("ORGTHREAD_DENY", "org/closed")],
    );
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(said(&o).contains("closed to this door"), "{}", said(&o));
    // A read-only source may be copied, but not to a read-only target.
    let ro = [("ORGTHREAD_READ_ONLY", "org/closed")];
    assert_eq!(
        code(&p.copy_with("org/closed/c.md", "org/notes/b.md", &ro)),
        0
    );
    let o = p.copy_with("org/notes/a.md", "org/closed/new/d.md", &ro);
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(said(&o).contains("read only"), "{}", said(&o));
    assert!(!p.at("org/closed/new").exists());
    // Every refusal has its log line.
    assert_eq!(p.log().matches("\tnot-allowed\t").count(), 2);
}

#[test]
fn a_file_that_is_not_text_goes_only_to_the_open_folders() {
    let p = Place::new();
    fs::create_dir_all(p.at("org/pictures")).unwrap();
    let picture = [0x89, b'P', b'N', b'G', 0, 1, 2, 3];
    p.put("org/notes/photo.jpg", &picture);
    let o = p.copy("org/notes/photo.jpg", "org/notes/b.jpg");
    assert_eq!(code(&o), 2, "{o:?}");
    assert_eq!(p.last_log()[1], "not-text");
    let open = [("ORGTHREAD_BINARY_ALLOW", "org/pictures")];
    assert_eq!(
        code(&p.copy_with("org/notes/photo.jpg", "org/pictures/b.jpg", &open)),
        0
    );
    assert_eq!(fs::read(p.at("org/pictures/b.jpg")).unwrap(), picture);
    // Locally the bytes count too: a picture with no extension stays out.
    p.put("org/notes/photo", &picture);
    let o = p.copy("org/notes/photo", "org/notes/copy-of-photo");
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(!p.at("org/notes/copy-of-photo").exists());
}

#[test]
fn a_new_name_must_stay_in_the_root_with_plain_parts() {
    let p = Place::new();
    p.put("org/notes/a.md", b"text\n");
    let outside = p.root.with_extension("outside").join("b.md");
    let o = p.run_with(
        &[
            "copy",
            &p.p("org/notes/a.md"),
            outside.to_str().unwrap(),
            "--who",
            "chat",
        ],
        &[],
    );
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(!outside.exists());
    let sneaky = format!("{}/org/missing/../../../b.md", p.root.display());
    let o = p.run_with(
        &["copy", &p.p("org/notes/a.md"), &sneaky, "--who", "chat"],
        &[],
    );
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(!p.at("org/missing").exists());
    // Two files, never one or three, and a name.
    for args in [
        vec!["copy", "a", "--who", "chat"],
        vec!["copy", "a", "b", "c", "--who", "chat"],
        vec!["copy", "a", "b"],
    ] {
        assert_eq!(code(&p.run_with(&args, &[])), 2, "{args:?}");
    }
}

#[test]
fn a_busy_target_gives_exit_3() {
    let p = Place::new();
    p.put("org/notes/a.md", b"text\n");
    fs::write(
        p.root.join("org/.locks/org--notes--b.md.lock"),
        "a writer\n",
    )
    .unwrap();
    let o = p.copy("org/notes/a.md", "org/notes/b.md");
    assert_eq!(code(&o), 3, "{o:?}");
    assert!(!p.at("org/notes/b.md").exists());
}

#[test]
fn watch_shows_a_copy_line_as_a_copy() {
    let p = Place::new();
    p.put("delete", b"text\n");
    assert_eq!(code(&p.copy("delete", "org/notes/b.md")), 0);
    assert_eq!(code(&p.copy("delete", "org/notes/b.md")), 5);
    let screen = p.run_with(&["watch", "--once"], &[]);
    let text = String::from_utf8_lossy(&screen.stdout).to_string();
    assert!(text.contains("copied"), "{text}");
    assert!(!text.contains("Nothing was deleted"), "{text}");
}

#[test]
fn an_empty_fingerprint_is_wrong_use_and_logs_nothing() {
    let p = Place::new();
    p.put("org/notes/a.md", b"text\n");
    for empty in ["", "   "] {
        let args = [
            "copy",
            &p.p("org/notes/a.md"),
            &p.p("org/notes/b.md"),
            "--who",
            "x",
            "--fingerprint",
            empty,
        ];
        let o = p.run_with(&args, &[]);
        assert_eq!(code(&o), 2, "{o:?}");
        assert!(
            said(&o).contains("--fingerprint needs the fingerprint of the source, and it is empty"),
            "{}",
            said(&o)
        );
    }
    assert!(!p.at("org/notes/b.md").exists());
    assert_eq!(p.log(), "", "no log line");
    assert!(p.locks_empty());
}
