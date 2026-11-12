//! The settings as a person meets them: the check at start, `config --check`,
//! and `config` with no terminal. Each test runs a copy of the program in its
//! own folder, with its own `.env`, and an empty environment.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

struct Place {
    /// The folder of the copy of the program, with its `.env`.
    program: PathBuf,
    /// The notes.
    root: PathBuf,
}

impl Place {
    fn new(env: &str) -> Place {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let base =
            std::env::temp_dir().join(format!("orgthread-config-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("program")).unwrap();
        fs::create_dir_all(base.join("notes/org")).unwrap();
        fs::create_dir_all(base.join("home")).unwrap();
        let base = fs::canonicalize(base).unwrap();
        fs::copy(
            env!("CARGO_BIN_EXE_orgthread"),
            base.join("program/orgthread"),
        )
        .unwrap();
        let place = Place {
            program: base.join("program"),
            root: base.join("notes"),
        };
        place.write_env(env);
        place
    }

    fn write_env(&self, env: &str) {
        let text = env.replace("{root}", self.root.to_str().unwrap());
        fs::write(self.program.join(".env"), text).unwrap();
    }

    fn env_text(&self) -> String {
        fs::read_to_string(self.program.join(".env")).unwrap()
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(self.program.join("orgthread"));
        c.args(args)
            .env_clear()
            .env("HOME", self.root.with_file_name("home"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }

    fn run(&self, args: &[&str], input: &[u8]) -> Output {
        let mut child = self.command(args).spawn().unwrap();
        let _ = child.stdin.take().unwrap().write_all(input);
        child.wait_with_output().unwrap()
    }

    fn note(&self) -> String {
        self.root.join("org/a.md").to_str().unwrap().to_string()
    }
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

const GOOD: &str = "ORGTHREAD_ROOT={root}\nORGTHREAD_OWNER=Ana\n";

#[test]
fn a_wrong_value_in_env_stops_every_command_with_code_2_and_names_it() {
    let p = Place::new(&format!("{GOOD}ORGTHREAD_UPLOAD_MB=abc\n"));
    let note = p.note();
    let commands: [&[&str]; 7] = [
        &["write", &note, "--who", "chat"],
        &["read", &note, "--who", "chat"],
        &["list", p.root.to_str().unwrap()],
        &["watch", "--once"],
        &["serve"],
        &["fingerprint", &note],
        &["password-hash"],
    ];
    for args in commands {
        let o = p.run(args, b"new\n");
        assert_eq!(code(&o), 2, "{args:?}: {o:?}");
        assert!(
            err(&o).contains("ORGTHREAD_UPLOAD_MB needs a whole number of MB, not abc"),
            "{args:?}: {}",
            err(&o)
        );
        assert!(err(&o).contains("orgthread config --check"), "{}", err(&o));
    }
    assert!(!p.root.join("org/a.md").exists(), "nothing was written");
    // Help and version read no setting, so they still run.
    for args in [["--help"], ["--version"]] {
        assert_eq!(code(&p.run(&args, b"")), 0, "{args:?}");
    }
}

#[test]
fn a_wrong_value_in_the_environment_stops_too() {
    let p = Place::new(GOOD);
    let o = p
        .command(&["read", &p.note(), "--who", "chat"])
        .env("ORGTHREAD_STORAGE", "Dropbox")
        .output()
        .unwrap();
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(err(&o).contains("ORGTHREAD_STORAGE needs dropbox or local, not Dropbox"));
}

#[test]
fn a_wrong_secret_is_named_but_never_shown() {
    let p = Place::new(&format!(
        "{GOOD}ORGTHREAD_LOGIN_HASH=invented-secret-hash\n"
    ));
    let o = p.run(&["list", p.root.to_str().unwrap()], b"");
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(err(&o).contains("ORGTHREAD_LOGIN_HASH"), "{}", err(&o));
    assert!(!err(&o).contains("invented-secret-hash"), "{}", err(&o));
    let o = p.run(&["config", "--check"], b"");
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(!out(&o).contains("invented-secret-hash"), "{}", out(&o));
}

#[test]
fn right_values_in_env_work_as_before() {
    let p = Place::new(&format!(
        "{GOOD}ORGTHREAD_UPLOAD_MB=5\nORGTHREAD_READ_ONLY=org/fixed\nORGTHREAD_DENY=org/private\n# a comment\nORGTHREAD_OWNR=typo\n"
    ));
    let o = p.run(&["write", &p.note(), "--who", "chat"], b"new\n");
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(
        fs::read_to_string(p.root.join("org/a.md")).unwrap(),
        "new\n"
    );
    // The limits of .env hold, as before.
    fs::create_dir_all(p.root.join("org/fixed")).unwrap();
    let fixed = p.root.join("org/fixed/b.md");
    let o = p.run(&["write", fixed.to_str().unwrap(), "--who", "chat"], b"x\n");
    assert_eq!(code(&o), 2, "{o:?}");
    fs::create_dir_all(p.root.join("org/private")).unwrap();
    fs::write(p.root.join("org/private/c.md"), "closed\n").unwrap();
    let closed = p.root.join("org/private/c.md");
    let o = p.run(&["read", closed.to_str().unwrap(), "--who", "chat"], b"");
    assert_eq!(code(&o), 2, "{o:?}");
    // The environment wins over .env, as before.
    let o = p
        .command(&["read", closed.to_str().unwrap(), "--who", "chat"])
        .env("ORGTHREAD_DENY", "")
        .output()
        .unwrap();
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(out(&o), "closed\n");
}

#[test]
fn the_check_lists_the_settings_of_its_door_and_where_they_come_from() {
    let p = Place::new(&format!(
        "{GOOD}ORGTHREAD_DROPBOX_TOKEN=invented-token-55aa\nORGTHREAD_OWNR=typo\n"
    ));
    let o = p
        .command(&["config", "--check"])
        .env("ORGTHREAD_UPLOAD_MB", "30")
        .output()
        .unwrap();
    assert_eq!(code(&o), 0, "{o:?}");
    let text = out(&o);
    for name in [
        "ORGTHREAD_ROOT",
        "ORGTHREAD_LOCK_DIR",
        "ORGTHREAD_SEARCH_MB",
        "ORGTHREAD_LOGIN_HASH",
    ] {
        assert!(text.contains(name), "{name}: {text}");
    }
    // A local door shows no Dropbox setting, and counts the one that is set.
    assert!(!text.contains("ORGTHREAD_DROPBOX"), "{text}");
    assert!(
        text.contains("1 Dropbox setting is set but not used with local"),
        "{text}"
    );
    assert!(text.contains("Ana  (.env)"), "{text}");
    assert!(text.contains("30  (environment)"), "{text}");
    assert!(text.contains("250  (default)"), "{text}");
    assert!(!text.contains("invented-token"), "{text}");
    assert!(text.contains("ORGTHREAD_OWNR"), "{text}");
    assert!(
        text.ends_with("All settings of this door are fine.\n"),
        "{text}"
    );
    // It changes nothing.
    assert!(
        p.env_text()
            .contains("ORGTHREAD_DROPBOX_TOKEN=invented-token-55aa\n")
    );
    let names: Vec<_> = fs::read_dir(&p.program)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().all(|n| !n.starts_with(".env.")),
        "no copy: {names:?}"
    );
}

/// Decision 5: a wrong value of the other door stops no command, and the
/// check names it as a warning, with code 0.
#[test]
fn a_wrong_value_of_the_other_door_stops_nothing() {
    let p = Place::new(&format!("{GOOD}ORGTHREAD_DROPBOX_API=ftp://invented\n"));
    let o = p.run(&["write", &p.note(), "--who", "chat"], b"new\n");
    assert_eq!(code(&o), 0, "{o:?}");
    let o = p.run(&["config", "--check"], b"");
    assert_eq!(code(&o), 0, "{o:?}");
    assert!(
        out(&o).contains("warning: ORGTHREAD_DROPBOX_API needs an address"),
        "{}",
        out(&o)
    );
    // With dropbox, the same value stops every command.
    p.write_env(
        "ORGTHREAD_STORAGE=dropbox\nORGTHREAD_DROPBOX_API=ftp://invented\nORGTHREAD_ROOT=\n",
    );
    let o = p.run(&["list", "/org"], b"");
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(
        err(&o).contains("ORGTHREAD_DROPBOX_API needs an address"),
        "{}",
        err(&o)
    );
    // And the empty root of the local door is only a warning there.
    assert!(!err(&o).contains("ORGTHREAD_ROOT"), "{}", err(&o));
    let o = p.run(&["config", "--check"], b"");
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(out(&o).contains("warning: ORGTHREAD_ROOT"), "{}", out(&o));
}

#[test]
fn the_check_ends_with_code_2_on_a_wrong_value() {
    let p = Place::new(&format!("{GOOD}ORGTHREAD_LISTEN_ANYWHERE=true\n"));
    let o = p.run(&["config", "--check"], b"");
    assert_eq!(code(&o), 2, "{o:?}");
    let text = out(&o);
    assert!(
        text.contains("wrong: ORGTHREAD_LISTEN_ANYWHERE needs yes or no, not true"),
        "{text}"
    );
    assert!(text.contains("1 wrong."), "{text}");
}

#[test]
fn the_screen_needs_a_terminal_and_changes_nothing_without_one() {
    let p = Place::new(GOOD);
    let o = p.run(&["config"], b"s\nq\n");
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(err(&o).contains("config needs a terminal"), "{}", err(&o));
    assert!(err(&o).contains("config --check"), "{}", err(&o));
    assert_eq!(
        p.env_text(),
        GOOD.replace("{root}", p.root.to_str().unwrap())
    );
    // The screen opens with a wrong value too, to fix it: no stop at start.
    p.write_env("ORGTHREAD_UPLOAD_MB=abc\n");
    let o = p.run(&["config"], b"");
    assert!(err(&o).contains("config needs a terminal"), "{}", err(&o));
    let o = p.run(&["config", "--wrong"], b"");
    assert_eq!(code(&o), 2, "{o:?}");
}

#[test]
fn the_help_lists_the_settings_from_the_list() {
    let p = Place::new(GOOD);
    let o = p.run(&["--help"], b"");
    assert_eq!(code(&o), 0, "{o:?}");
    let text = out(&o);
    for name in [
        "ORGTHREAD_ROOT",
        "ORGTHREAD_DROPBOX_API",
        "ORGTHREAD_DROPBOX_TOKEN",
        "ORGTHREAD_LOGIN_HASH",
    ] {
        assert!(text.contains(name), "{name}");
    }
    assert!(text.contains("orgthread config --check"), "{text}");
    assert!(text.contains("Folders and logs:"), "{text}");
    assert!(text.contains("Exit codes:"), "{text}");
}
