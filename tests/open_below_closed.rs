//! An opening rule of ORGTHREAD_DENY, on a local folder with invented files.
//! The value /notes/private,!/notes/private/shared closes notes/private and
//! opens notes/private/shared inside it again. Each test runs the program
//! itself, as an agent does.

use orgthread::fingerprint::fingerprint;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

const DENY: &str = "/notes/private,!/notes/private/shared";

const TODAY: &str = "notes/today.md";
const DIARY: &str = "notes/private/diary.md";
const PLAN: &str = "notes/private/shared/plan.md";
const BESIDE: &str = "notes/private/shared2/x.md";

/// Bytes that are not text, as a PDF starts: a NUL sign and no valid UTF-8.
fn pdf(seed: u8) -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    bytes.push(0);
    bytes.extend((0..2000u32).map(|i| (i * 7 + seed as u32) as u8 | 0x80));
    bytes
}

struct Place {
    root: PathBuf,
}

impl Place {
    /// A folder with only notes/today.md in it.
    fn bare() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let root =
            std::env::temp_dir().join(format!("orgthread-opening-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(root.with_extension("outside"));
        fs::create_dir_all(root.join("notes")).unwrap();
        fs::create_dir_all(root.with_extension("outside").join("locks")).unwrap();
        let place = Place {
            root: fs::canonicalize(root).unwrap(),
        };
        place.put(TODAY, b"tulip today\n");
        place
    }

    /// The closed folder, the open folder inside it and a closed one beside it.
    fn new() -> Self {
        let place = Place::bare();
        fs::create_dir_all(place.at("notes/private/shared")).unwrap();
        fs::create_dir_all(place.at("notes/private/shared2")).unwrap();
        place.put(DIARY, b"tulip diary\n");
        place.put(PLAN, b"tulip plan\n");
        place.put(BESIDE, b"tulip beside\n");
        place
    }

    fn at(&self, path: &str) -> PathBuf {
        self.root.join(path)
    }

    /// The full path, as an agent names a file.
    fn p(&self, path: &str) -> String {
        self.at(path).to_str().unwrap().to_string()
    }

    fn put(&self, path: &str, bytes: &[u8]) {
        fs::write(self.at(path), bytes).unwrap();
    }

    fn get(&self, path: &str) -> Vec<u8> {
        fs::read(self.at(path)).unwrap()
    }

    fn outside(&self, name: &str) -> PathBuf {
        self.root.with_extension("outside").join(name)
    }

    /// The program with ORGTHREAD_DENY set to `DENY`, and no other limit: a
    /// setting of the developer cannot leak in. `env` comes last, so it may
    /// change DENY or add a limit.
    fn run(&self, args: &[&str], stdin: &[u8], env: &[(&str, &str)]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_orgthread"));
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("ORGTHREAD_") {
                c.env_remove(name);
            }
        }
        c.args(args)
            .env("ORGTHREAD_ROOT", &self.root)
            .env("ORGTHREAD_LOCK_DIR", self.outside("locks"))
            .env("ORGTHREAD_LOG", self.outside("writes.log"))
            .env("ORGTHREAD_READS", self.outside("reads"))
            .env("ORGTHREAD_VERSIONS", self.outside("versions"))
            .env("ORGTHREAD_STORAGE", "local")
            .env("ORGTHREAD_DENY", DENY)
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        // A command that refuses early never reads its input and closes the pipe.
        match child.stdin.take().unwrap().write_all(stdin) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::BrokenPipe => {}
            Err(e) => panic!("{e}"),
        }
        child.wait_with_output().unwrap()
    }

    fn read(&self, path: &str, who: &str) -> Output {
        self.run(&["read", &self.p(path), "--who", who], b"", &[])
    }

    fn write(&self, path: &str, text: &[u8]) -> Output {
        self.run(&["write", &self.p(path), "--who", "chat"], text, &[])
    }

    fn patch(&self, path: &str, json: &str) -> Output {
        self.run(
            &["patch", &self.p(path), "--who", "chat"],
            json.as_bytes(),
            &[],
        )
    }

    fn list(&self, path: &str, more: &[&str]) -> Output {
        let folder = self.p(path);
        let mut args = vec!["list", folder.as_str()];
        args.extend_from_slice(more);
        self.run(&args, b"", &[])
    }

    fn search(&self, text: &str, path: &str, more: &[&str]) -> Output {
        let folder = self.p(path);
        let mut args = vec!["search", text, folder.as_str(), "--who", "chat"];
        args.extend_from_slice(more);
        self.run(&args, b"", &[])
    }

    fn two(&self, command: &str, from: &str, to: &str) -> Output {
        self.run(
            &[command, &self.p(from), &self.p(to), "--who", "chat"],
            b"",
            &[],
        )
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
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// The entries of a list, with no line that names the folder: the path of the
/// test folder may hold any word.
fn entries(o: &Output) -> String {
    out(o)
        .lines()
        .filter(|line| line.starts_with(' '))
        .map(|line| format!("{line}\n"))
        .collect()
}

#[test]
fn a_file_in_the_open_folder_reads_and_a_closed_one_does_not() {
    let p = Place::new();
    let o = p.read(PLAN, "chat");
    assert_eq!(code(&o), 0, "the open folder: {o:?}");
    assert_eq!(o.stdout, b"tulip plan\n");
    let o = p.read(TODAY, "chat");
    assert_eq!(code(&o), 0, "no rule holds it: {o:?}");
    for closed in [DIARY, BESIDE] {
        let o = p.read(closed, "chat");
        assert_eq!(code(&o), 2, "{closed}: {o:?}");
        assert!(o.stdout.is_empty(), "{closed}: {}", out(&o));
        assert!(
            err(&o).contains(&format!("{closed} is closed to this door")),
            "{closed}: {}",
            err(&o)
        );
    }
    // Capitals do not count in the rules.
    let capitals = [("ORGTHREAD_DENY", "/Notes/Private,!/NOTES/private/Shared")];
    let o = p.run(&["read", &p.p(PLAN), "--who", "chat"], b"", &capitals);
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(o.stdout, b"tulip plan\n");
    let o = p.run(&["read", &p.p(DIARY), "--who", "chat"], b"", &capitals);
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(o.stdout.is_empty());
}

#[test]
fn write_and_patch_work_in_the_open_folder_and_nowhere_closed() {
    let p = Place::new();
    let new = "notes/private/shared/new.md";
    let o = p.write(new, b"a new note\n");
    assert_eq!(code(&o), 0, "a new file needs no read: {o:?}");
    assert_eq!(p.get(new), b"a new note\n");

    assert_eq!(code(&p.read(PLAN, "chat")), 0);
    let o = p.write(PLAN, b"tulip plan\nsecond line\n");
    assert_eq!(code(&o), 0, "a replace after a read: {o:?}");
    assert_eq!(p.get(PLAN), b"tulip plan\nsecond line\n");

    assert_eq!(code(&p.read(PLAN, "chat")), 0);
    let o = p.patch(
        PLAN,
        r#"{"edits":[{"old":"second line\n","new":"third line\n"}],"append":"end\n"}"#,
    );
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(p.get(PLAN), b"tulip plan\nthird line\nend\n");

    // Closed: the parent of the open folder, and the folder beside it.
    for closed in [DIARY, BESIDE] {
        let before = p.get(closed);
        let o = p.write(closed, b"changed\n");
        assert_eq!(code(&o), 2, "write {closed}: {o:?}");
        assert!(err(&o).contains("is closed to this door"), "{}", err(&o));
        let o = p.patch(
            closed,
            r#"{"edits":[{"old":"changed\n","new":"x\n"}],"append":"y\n"}"#,
        );
        assert_eq!(code(&o), 2, "patch {closed}: {o:?}");
        assert_eq!(p.get(closed), before, "{closed} is unchanged");
    }
    for closed in ["notes/private/new.md", "notes/private/shared2/new.md"] {
        let o = p.write(closed, b"new\n");
        assert_eq!(code(&o), 2, "write {closed}: {o:?}");
        assert!(!p.at(closed).exists(), "{closed}");
    }
}

#[test]
fn a_list_shows_the_open_folder_and_hides_its_closed_parents() {
    let p = Place::new();
    fs::create_dir_all(p.at("notes/private/shared/2026")).unwrap();
    p.put("notes/private/shared/2026/deep.md", b"deep\n");

    let o = p.list("notes/private/shared", &[]);
    assert_eq!(code(&o), 0, "{o:?}");
    let text = out(&o);
    assert!(text.contains("  plan.md\n"), "{text}");
    assert!(text.contains("2026/"), "{text}");
    assert!(!text.contains("deep.md"), "depth 1 only: {text}");

    let o = p.list("notes/private/shared", &["--depth", "all"]);
    assert_eq!(code(&o), 0, "{o:?}");
    let text = out(&o);
    assert!(text.contains("plan.md"), "{text}");
    assert!(text.contains("deep.md"), "{text}");

    for (folder, more) in [
        ("", &[][..]),
        ("", &["--depth", "all"][..]),
        ("notes", &[][..]),
        ("notes", &["--depth", "all"][..]),
    ] {
        let o = p.list(folder, more);
        assert_eq!(code(&o), 0, "{folder:?} {more:?}: {o:?}");
        let text = entries(&o);
        assert!(text.contains("today.md") || folder.is_empty(), "{text}");
        for hidden in ["private", "shared", "diary.md", "plan.md", "x.md"] {
            assert!(!text.contains(hidden), "{folder:?} {more:?}: {text}");
        }
    }

    for closed in ["notes/private", "notes/private/shared2"] {
        let o = p.list(closed, &[]);
        assert_eq!(code(&o), 2, "{closed}: {o:?}");
        // The group of the folder holds the reason, and no entry.
        assert_eq!(
            entries(&o),
            format!("  {closed} is closed to this door\n"),
            "{o:?}"
        );
    }
}

#[test]
fn search_finds_the_open_file_and_nothing_closed() {
    let p = Place::new();
    let shared = p.p("notes/private/shared");
    let o = p.search("TULIP", "notes/private/shared", &[]);
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(out(&o), format!("{shared}/plan.md:1: tulip plan\n"));

    let o = p.search("plan", "notes/private/shared", &["--names"]);
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(out(&o), format!("{shared}/plan.md\n"));

    for closed in ["notes/private", "notes/private/shared2"] {
        for more in [&[][..], &["--names"][..]] {
            let o = p.search("tulip", closed, more);
            assert_eq!(code(&o), 2, "{closed} {more:?}: {o:?}");
            assert!(out(&o).is_empty(), "{}", out(&o));
        }
    }

    // The hits, with the path of the test folder taken out.
    let hits = |o: &Output| out(o).replace(p.root.to_str().unwrap(), "");
    let o = p.search("tulip", "", &[]);
    assert_eq!(code(&o), 0, "{o:?}");
    let text = hits(&o);
    assert!(text.contains("today.md:1: tulip today"), "{text}");
    for hidden in ["diary", "plan", "beside", "private"] {
        assert!(!text.contains(hidden), "{text}");
    }
    let o = p.search("a", "", &["--names"]);
    assert_eq!(code(&o), 0, "{o:?}");
    let text = hits(&o);
    assert!(text.contains("today.md"), "{text}");
    for hidden in ["diary", "plan", "private", "shared"] {
        assert!(!text.contains(hidden), "{text}");
    }
}

#[test]
fn download_and_upload_work_in_the_open_folder() {
    let p = Place::new();
    let o = p.run(&["download", &p.p(PLAN), "--who", "chat"], b"", &[]);
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(o.stdout, b"tulip plan\n");

    let scan = "notes/private/shared/scan.pdf";
    let binary = [("ORGTHREAD_BINARY_ALLOW", "/notes/private/shared")];
    let upload = |path: &str, who: &str, bytes: &[u8], env: &[(&str, &str)]| {
        p.run(&["upload", &p.p(path), "--who", who], bytes, env)
    };
    // A file that is not text needs ORGTHREAD_BINARY_ALLOW, also here.
    let o = upload(scan, "chat", &pdf(1), &[]);
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(!p.at(scan).exists());

    let o = upload(scan, "chat", &pdf(1), &binary);
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(p.get(scan), pdf(1));

    // A replace needs a download by the same name first.
    let o = upload(scan, "other chat", &pdf(2), &binary);
    assert_eq!(code(&o), 5, "{o:?}");
    assert_eq!(p.get(scan), pdf(1));
    let o = p.run(&["download", &p.p(scan), "--who", "chat"], b"", &binary);
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(o.stdout, pdf(1));
    let o = upload(scan, "chat", &pdf(2), &binary);
    assert_eq!(code(&o), 0, "a replace after a download: {o:?}");
    assert_eq!(p.get(scan), pdf(2));

    // Closed paths stay closed, also with ORGTHREAD_BINARY_ALLOW on them.
    let closed = [("ORGTHREAD_BINARY_ALLOW", "/notes/private")];
    for path in ["notes/private/scan.pdf", "notes/private/shared2/scan.pdf"] {
        let o = upload(path, "chat", &pdf(3), &closed);
        assert_eq!(code(&o), 2, "{path}: {o:?}");
        assert!(err(&o).contains("is closed to this door"), "{}", err(&o));
        assert!(!p.at(path).exists(), "{path}");
    }
    let o = upload(DIARY, "chat", b"changed\n", &[]);
    assert_eq!(code(&o), 2, "{o:?}");
    assert_eq!(p.get(DIARY), b"tulip diary\n");
    let o = p.run(&["download", &p.p(DIARY), "--who", "chat"], b"", &[]);
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(o.stdout.is_empty());
}

#[test]
fn copy_and_move_keep_inside_the_open_folder() {
    let p = Place::new();
    let refused = [
        (PLAN, "notes/private/plan.md"),
        (PLAN, "notes/private/shared2/plan.md"),
        (DIARY, "notes/private/shared/diary.md"),
        (BESIDE, "notes/private/shared/x.md"),
    ];
    for command in ["copy", "move"] {
        for (from, to) in refused {
            let before = p.get(from);
            let o = p.two(command, from, to);
            assert_eq!(code(&o), 2, "{command} {from} {to}: {o:?}");
            assert!(err(&o).contains("is closed to this door"), "{}", err(&o));
            assert_eq!(p.get(from), before, "{from} is unchanged");
            assert!(!p.at(to).exists(), "{to}");
        }
    }

    let copied = "notes/private/shared/plan-copy.md";
    let o = p.two("copy", PLAN, copied);
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(p.get(copied), b"tulip plan\n");
    assert_eq!(p.get(PLAN), b"tulip plan\n");

    let deeper = "notes/private/shared/2026/plan.md";
    let o = p.two("copy", PLAN, deeper);
    assert_eq!(code(&o), 0, "a new folder inside the open folder: {o:?}");
    assert_eq!(p.get(deeper), b"tulip plan\n");

    let moved = "notes/private/shared/renamed.md";
    let o = p.two("move", copied, moved);
    assert_eq!(code(&o), 0, "{o:?}");
    assert!(!p.at(copied).exists());
    assert_eq!(p.get(moved), b"tulip plan\n");

    let o = p.two("move", TODAY, "notes/private/shared/today.md");
    assert_eq!(code(&o), 0, "from a path no rule holds: {o:?}");
    assert_eq!(p.get("notes/private/shared/today.md"), b"tulip today\n");
}

#[test]
fn a_delete_in_the_open_folder_needs_delete_allow_and_a_closed_file_stays() {
    let p = Place::new();
    let delete = |path: &str, env: &[(&str, &str)], more: &[&str]| {
        let file = p.p(path);
        let mut args = vec!["delete", file.as_str(), "--who", "chat"];
        args.extend_from_slice(more);
        p.run(&args, b"", env)
    };
    let open = [("ORGTHREAD_DELETE_ALLOW", "/notes/private/shared")];
    assert_eq!(code(&p.read(PLAN, "chat")), 0);

    // With no ORGTHREAD_DELETE_ALLOW, every delete is refused.
    let o = delete(PLAN, &[], &[]);
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(
        err(&o).contains("ORGTHREAD_DELETE_ALLOW is empty"),
        "{}",
        err(&o)
    );
    assert!(p.at(PLAN).exists());

    let o = delete(PLAN, &open, &[]);
    assert_eq!(code(&o), 0, "{o:?}");
    assert!(!p.at(PLAN).exists());

    // A fingerprint from elsewhere does as well as a read.
    let other = "notes/private/shared/other.md";
    p.put(other, b"other\n");
    let o = delete(other, &open, &["--expect", &fingerprint(b"other\n")]);
    assert_eq!(code(&o), 0, "{o:?}");
    assert!(!p.at(other).exists());

    // A closed file stays, even when ORGTHREAD_DELETE_ALLOW holds it.
    let wide = [("ORGTHREAD_DELETE_ALLOW", "/notes")];
    for closed in [DIARY, BESIDE] {
        let print = fingerprint(&p.get(closed));
        let o = delete(closed, &wide, &["--expect", &print]);
        assert_eq!(code(&o), 2, "{closed}: {o:?}");
        assert!(err(&o).contains("is closed to this door"), "{}", err(&o));
        assert!(p.at(closed).exists(), "{closed}");
    }
}

/// Each way to make a file at `to`, with the code and what it said.
fn each_new_file(p: &Place, to: &str) -> Vec<(&'static str, Output)> {
    let file = p.p(to);
    let check = p.run(&["upload", &file, "--who", "chat", "--check"], b"", &[]);
    vec![
        ("write", p.write(to, b"new\n")),
        ("upload --check", check),
        (
            "upload",
            p.run(&["upload", &file, "--who", "chat"], b"new\n", &[]),
        ),
        ("copy", p.two("copy", TODAY, to)),
        ("move", p.two("move", TODAY, to)),
    ]
}

#[test]
fn new_folders_above_the_open_folder_are_refused_and_inside_it_are_made() {
    let p = Place::bare();
    let to = "notes/private/shared/new.md";
    // No notes/private yet: the first new folder is closed.
    for (how, o) in each_new_file(&p, to) {
        assert_eq!(code(&o), 2, "{how}: {o:?}");
        assert!(
            err(&o).contains(
                "notes/private would be a new folder, and it is closed to this door. Only the owner makes it, by hand."
            ),
            "{how}: {}",
            err(&o)
        );
        assert!(!p.at("notes/private").exists(), "{how} made no folder");
        assert_eq!(p.get(TODAY), b"tulip today\n", "{how}");
    }

    // notes/private is there, but the open folder itself would be new.
    fs::create_dir_all(p.at("notes/private")).unwrap();
    for (how, o) in each_new_file(&p, to) {
        assert_eq!(code(&o), 2, "{how}: {o:?}");
        assert!(
            err(&o).contains(
                "notes/private/shared would be a new folder, and it is the folder that !/notes/private/shared opens."
            ),
            "{how}: {}",
            err(&o)
        );
        assert!(
            !p.at("notes/private/shared").exists(),
            "{how} made no folder"
        );
        assert_eq!(p.get(TODAY), b"tulip today\n", "{how}");
    }

    // With the open folder there, new folders inside it are made.
    fs::create_dir_all(p.at("notes/private/shared")).unwrap();
    let deep = "notes/private/shared/2026/11/plan.md";
    let o = p.write(deep, b"a plan\n");
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(
        out(&o),
        "written: notes/private/shared/2026/11/plan.md, 7 bytes, new folders: notes/private/shared/2026, notes/private/shared/2026/11\n"
    );
    assert_eq!(p.get(deep), b"a plan\n");
    let file = p.p("notes/private/shared/2027/a.md");
    let o = p.run(&["upload", &file, "--who", "chat", "--check"], b"", &[]);
    assert_eq!(code(&o), 0, "{o:?}");
    assert!(!p.at("notes/private/shared/2027").exists(), "a check only");
    let o = p.run(&["upload", &file, "--who", "chat"], b"up\n", &[]);
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(p.get("notes/private/shared/2027/a.md"), b"up\n");
    let o = p.two("copy", TODAY, "notes/private/shared/2028/b.md");
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(p.get("notes/private/shared/2028/b.md"), b"tulip today\n");
    let o = p.two("move", TODAY, "notes/private/shared/2029/c.md");
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(p.get("notes/private/shared/2029/c.md"), b"tulip today\n");
    assert!(!p.at(TODAY).exists());
}

/// A copy of the program in its own folder, with its own `.env`, run with an
/// empty environment, for the check at start.
struct Program {
    folder: PathBuf,
    root: PathBuf,
}

impl Program {
    fn new() -> Program {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!(
            "orgthread-opening-check-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("program")).unwrap();
        fs::create_dir_all(base.join("notes/notes/private/shared")).unwrap();
        fs::create_dir_all(base.join("home")).unwrap();
        let base = fs::canonicalize(base).unwrap();
        fs::copy(
            env!("CARGO_BIN_EXE_orgthread"),
            base.join("program/orgthread"),
        )
        .unwrap();
        let program = Program {
            folder: base.join("program"),
            root: base.join("notes"),
        };
        fs::write(program.root.join(TODAY), "tulip today\n").unwrap();
        fs::write(program.root.join(PLAN), "tulip plan\n").unwrap();
        program
    }

    fn with_deny(&self, deny: &str) {
        let text = format!(
            "ORGTHREAD_ROOT={}\nORGTHREAD_OWNER=Ana\nORGTHREAD_DENY={deny}\n",
            self.root.display()
        );
        fs::write(self.folder.join(".env"), text).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(self.folder.join("orgthread"))
            .args(args)
            .env_clear()
            .env("HOME", self.root.with_file_name("home"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .unwrap()
    }

    fn p(&self, path: &str) -> String {
        self.root.join(path).to_str().unwrap().to_string()
    }
}

impl Drop for Program {
    fn drop(&mut self) {
        if let Some(base) = self.folder.parent() {
            let _ = fs::remove_dir_all(base);
        }
    }
}

#[test]
fn a_wrong_opening_rule_stops_each_command_and_config_check_names_it() {
    let program = Program::new();
    let wrong = [
        (
            "/notes,!/tasks/a",
            "ORGTHREAD_DENY: !/tasks/a lies inside no closing rule, so it opens nothing.",
        ),
        (
            "/notes,!/notes/../a",
            "ORGTHREAD_DENY: !/notes/../a holds an empty part, . or ..",
        ),
        (
            "/notes/private,!/notes/private",
            "ORGTHREAD_DENY: !/notes/private opens the same path that /notes/private closes.",
        ),
        ("/notes/private,!/", "ORGTHREAD_DENY: !/ opens no path."),
        (
            "/notes/private,!/notes/private/shared,/notes/private/shared//secret",
            "ORGTHREAD_DENY: /notes/private/shared//secret lies inside the opening rule !/notes/private/shared, and it holds an empty part, . or .., so it closes nothing.",
        ),
    ];
    let (today, plan, root) = (program.p(TODAY), program.p(PLAN), program.p(""));
    for (deny, reason) in wrong {
        program.with_deny(deny);
        let commands: [&[&str]; 4] = [
            &["read", &today, "--who", "chat"],
            &["read", &plan, "--who", "chat"],
            &["list", &root],
            &["search", "tulip", &root, "--who", "chat"],
        ];
        for args in commands {
            let o = program.run(args);
            assert_eq!(code(&o), 2, "{deny} {args:?}: {o:?}");
            assert!(o.stdout.is_empty(), "{deny} {args:?}: {}", out(&o));
            assert!(err(&o).contains(reason), "{deny} {args:?}: {}", err(&o));
        }
        let o = program.run(&["config", "--check"]);
        assert_eq!(code(&o), 2, "{deny}: {o:?}");
        assert!(
            out(&o).contains(&format!("wrong: {reason}")),
            "{deny}: {}",
            out(&o)
        );
    }

    // A typo in a closing rule inside no opening rule keeps its meaning.
    for deny in ["/notes//old", &format!("{DENY},/notes//old")] {
        program.with_deny(deny);
        let o = program.run(&["config", "--check"]);
        assert_eq!(code(&o), 0, "{deny}: {o:?}");
        assert!(!out(&o).contains("wrong: "), "{deny}: {}", out(&o));
        let o = program.run(&["read", &today, "--who", "chat"]);
        assert_eq!(code(&o), 0, "{deny}: {o:?}");
    }

    program.with_deny(DENY);
    let o = program.run(&["config", "--check"]);
    assert_eq!(code(&o), 0, "{o:?}");
    assert!(!out(&o).contains("wrong: "), "{}", out(&o));
    let o = program.run(&["read", &plan, "--who", "chat"]);
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(o.stdout, b"tulip plan\n");
}
