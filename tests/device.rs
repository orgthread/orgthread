//! The device door over HTTP, on an invented sample folder: the pairing, the
//! three tools, the path rules, the read cap with its push, the revoke, and
//! T6, the search for a marker word on the disk and in the output.

use orgthread::device::{self, CODE_LIFE, Store, TOKEN_LIFE};
use serde_json::{Value, json};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

/// An invented word that stands only in the invented chapter. T6 looks for it.
const MARKER: &str = "quendelmorgenrot";
const CHAPTER_1: &str = "Kapitel eins. Der quendelmorgenrot Himmel war still.\nEin Satz.\n";
const CHAPTER_2: &str = "Kapitel zwei. Ein erfundener Text.\n";
const CHAPTER_3: &str = "Kapitel drei. Noch ein erfundener Text.\n";
const WHOLE_BOOK: &str = "an invented whole book, kept beside the book folders\n";
const PRIVATE: &str = "an invented private note, never for a device\n";
const PROGRESS: &str = "# Reading: an invented book\n\n- Place: chapter 1, page 1\n";
const PREFIX: &str = "/an-invented-secret";
const BOOK: &str = "/books/an-invented-book";
const PROGRESS_FILE: &str = "/reading/progress-an-invented-book.md";
/// The invented lessons of a list-only door, each with a marker word that
/// must never leave the sample folder.
const LESSON_MARKER: &str = "fliederwolkenbank";
const LESSON: &str = "/lessons/Lesson 1 Mon.pdf";
const LESSONS: [(&str, &str); 4] = [
    (
        "Lesson 1 Mon.pdf",
        "%PDF-1.4 an invented lesson fliederwolkenbank\n",
    ),
    (
        "Lesson 1 Mon.pdf.mark",
        "an invented ink file fliederwolkenbank\n",
    ),
    (
        "00 Week.json",
        "{\"week\": \"an invented week fliederwolkenbank\"}\n",
    ),
    (
        "Archive/2026/11/Lesson 0 Sun.pdf",
        "%PDF-1.4 an old invented lesson fliederwolkenbank\n",
    ),
];

/// Invented bytes of a note, not text: every byte value, NUL included, in a
/// pattern that differs with `seed`.
fn note_bytes(seed: u8) -> Vec<u8> {
    (0..3000u32)
        .map(|i| (i as u8).wrapping_mul(7).wrapping_add(seed))
        .collect()
}

/// One door at a time, as for the other doors over HTTP.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

struct Door {
    _turn: MutexGuard<'static, ()>,
    port: u16,
    dir: PathBuf,
    child: Child,
}

struct Reply {
    status: u16,
    body: String,
}

/// An answer with its headers and its exact bytes.
struct Bytes {
    status: u16,
    head: String,
    body: Vec<u8>,
}

impl Bytes {
    fn header(&self, name: &str) -> Option<String> {
        self.head.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name)
                .then(|| value.trim().to_string())
        })
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// The sample folder: a book, its glossary, its summary, the progress file, a
/// private file, and two links that point at the private file.
fn sample(dir: &Path) {
    let root = dir.join("root");
    let book = root.join("books/an-invented-book");
    fs::create_dir_all(&book).unwrap();
    fs::write(book.join("chapter-01.txt"), CHAPTER_1).unwrap();
    fs::write(book.join("chapter-02.txt"), CHAPTER_2).unwrap();
    fs::write(book.join("chapter-03.txt"), CHAPTER_3).unwrap();
    fs::write(
        book.join("contents.txt"),
        "1 Kapitel eins\n2 Kapitel zwei\n",
    )
    .unwrap();
    // A whole book beside the book folders: never for a device.
    fs::write(root.join("books/an-invented-book.epub"), WHOLE_BOOK).unwrap();
    let reading = root.join("reading");
    fs::create_dir_all(reading.join("glossary/an-invented-book")).unwrap();
    fs::create_dir_all(reading.join("summaries/an-invented-book")).unwrap();
    fs::write(
        reading.join("glossary/an-invented-book/glossary-01.md"),
        "# Glossar\n\n- still: quiet\n",
    )
    .unwrap();
    fs::write(
        reading.join("summaries/an-invented-book/summary-01.md"),
        "# Zusammenfassung\n\n¶1 | Es ist still. | It is quiet.\n",
    )
    .unwrap();
    fs::write(reading.join("progress-an-invented-book.md"), PROGRESS).unwrap();
    fs::write(reading.join("notes.md"), PRIVATE).unwrap();
    fs::create_dir_all(root.join("health")).unwrap();
    fs::write(root.join("health/invented-health.md"), PRIVATE).unwrap();
    for (name, text) in LESSONS {
        let file = root.join("lessons").join(name);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }
    let notes = root.join("notes");
    fs::create_dir_all(notes.join("sub")).unwrap();
    fs::write(notes.join("a.note"), note_bytes(1)).unwrap();
    fs::write(notes.join("sub/b.note"), note_bytes(2)).unwrap();
    std::os::unix::fs::symlink(root.join("health"), book.join("sneaky")).unwrap();
    std::os::unix::fs::symlink(
        root.join("health/invented-health.md"),
        root.join("books/link.txt"),
    )
    .unwrap();
    let notify = dir.join("notify.sh");
    fs::write(
        &notify,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$1\" >> '{}'\n",
            dir.join("notified.txt").display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&notify, fs::Permissions::from_mode(0o700)).unwrap();
}

/// The settings of the device door, all in the environment.
fn settings(dir: &Path, cap_hour: &str) -> Vec<(&'static str, String)> {
    let state = dir.join("state");
    let at = |name: &str| state.join(name).display().to_string();
    vec![
        ("ORGTHREAD_STORAGE", "local".into()),
        ("ORGTHREAD_ROOT", dir.join("root").display().to_string()),
        ("ORGTHREAD_LOCK_DIR", at("locks")),
        ("ORGTHREAD_LOG", at("writes.log")),
        ("ORGTHREAD_READS", at("reads")),
        ("ORGTHREAD_VERSIONS", at("versions")),
        ("ORGTHREAD_DEVICES", at("devices.json")),
        ("ORGTHREAD_LOGIN_HASH", String::new()),
        (
            "ORGTHREAD_PUBLIC_URL",
            format!("https://reader.example.com{PREFIX}"),
        ),
        ("ORGTHREAD_DEVICE_APP", "reader".into()),
        (
            "ORGTHREAD_DEVICE_READ",
            "/books/*/, /reading/glossary/, /reading/summaries/".into(),
        ),
        ("ORGTHREAD_DEVICE_PATCH", "/reading/progress-*.md".into()),
        ("ORGTHREAD_DEVICE_CAPPED", "/books/*/chapter-*.txt".into()),
        ("ORGTHREAD_DEVICE_CAP_HOUR", cap_hour.into()),
        ("ORGTHREAD_DEVICE_CAP_DAY", "40".into()),
        (
            "ORGTHREAD_DEVICE_NOTIFY",
            dir.join("notify.sh").display().to_string(),
        ),
    ]
}

/// The program with the settings of the door, and none from the test run.
fn program(dir: &Path, cap_hour: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orgthread"));
    for (name, _) in std::env::vars() {
        if name.starts_with("ORGTHREAD_") {
            command.env_remove(name);
        }
    }
    command.envs(settings(dir, cap_hour));
    command
}

fn new_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orgthread-device-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::canonicalize(dir).unwrap()
}

impl Door {
    fn open(name: &str) -> Self {
        Self::open_with(name, "8")
    }

    fn open_with(name: &str, cap_hour: &str) -> Self {
        Self::open_changed(name, cap_hour, &[])
    }

    /// A door with some settings changed from those of `settings`.
    fn open_changed(name: &str, cap_hour: &str, change: &[(&str, &str)]) -> Self {
        let turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let dir = new_dir(name);
        sample(&dir);
        static NEXT: AtomicU16 = AtomicU16::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let port = 41_000 + (std::process::id() % 20_000) as u16 + n;
        assert!(
            TcpListener::bind(("127.0.0.1", port)).is_ok(),
            "port {port} is busy"
        );
        // The output of the door goes to a file, so T6 can search it.
        let out = fs::File::create(dir.join("door-output.txt")).unwrap();
        let mut child = program(&dir, cap_hour)
            .envs(change.iter().copied())
            .args(["serve", "--http", &format!("127.0.0.1:{port}"), "--device"])
            .stdout(Stdio::from(out.try_clone().unwrap()))
            .stderr(Stdio::from(out))
            .spawn()
            .unwrap();
        for _ in 0..200 {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            if let Ok(Some(status)) = child.try_wait() {
                let said = fs::read_to_string(dir.join("door-output.txt")).unwrap_or_default();
                panic!("the door stopped at its start, {status}: {said}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Door {
            _turn: turn,
            port,
            dir,
            child,
        }
    }

    fn store(&self) -> Store {
        Store::new(self.dir.join("state/devices.json"))
    }

    fn raw(&self, method: &str, path: &str, extra: &str, body: &str) -> Reply {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        let text = format!(
            "{method} {path} HTTP/1.1\r\nHost: x\r\n{extra}Content-Length: {}\r\n\r\n{body}",
            body.len()
        );
        s.write_all(text.as_bytes()).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        let (head, body) = out.split_once("\r\n\r\n").unwrap();
        Reply {
            status: head.split_whitespace().nth(1).unwrap().parse().unwrap(),
            body: body.to_string(),
        }
    }

    fn raw_bytes(&self, method: &str, path: &str, extra: &str, body: &[u8]) -> Bytes {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        let text = format!(
            "{method} {path} HTTP/1.1\r\nHost: x\r\n{extra}Content-Length: {}\r\n\r\n",
            body.len()
        );
        s.write_all(text.as_bytes()).unwrap();
        // A refusal may come before the whole body is read.
        let _ = s.write_all(body);
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out);
        let at = out.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8_lossy(&out[..at]).to_string();
        Bytes {
            status: head.split_whitespace().nth(1).unwrap().parse().unwrap(),
            head,
            body: out[at + 4..].to_vec(),
        }
    }

    fn download(&self, token: &str, file: &str) -> Bytes {
        let extra = format!("Authorization: Bearer {token}\r\n");
        let path = format!("{PREFIX}/download?file={}", encoded(file));
        self.raw_bytes("GET", &path, &extra, b"")
    }

    fn upload(&self, token: &str, file: &str, seen: Option<&str>, bytes: &[u8]) -> Bytes {
        let extra = format!("Authorization: Bearer {token}\r\n");
        let mut path = format!("{PREFIX}/upload?file={}", encoded(file));
        if let Some(seen) = seen {
            path.push_str(&format!("&fingerprint={seen}"));
        }
        self.raw_bytes("PUT", &path, &extra, bytes)
    }

    fn root_bytes(&self, path: &str) -> Option<Vec<u8>> {
        fs::read(self.dir.join("root").join(path.trim_start_matches('/'))).ok()
    }

    fn pair_with(&self, code: &str) -> Reply {
        let body = json!({ "code": code }).to_string();
        self.raw(
            "POST",
            &format!("{PREFIX}/pair"),
            "Content-Type: application/json\r\n",
            &body,
        )
    }

    /// A code from the owner's command, and the token the device gets for it.
    fn pair(&self) -> String {
        let code = self.store().issue("tablet", device::now()).unwrap();
        let reply = self.pair_with(&device::shown_code(&code));
        assert_eq!(reply.status, 200, "{}", reply.body);
        let answer: Value = serde_json::from_str(&reply.body).unwrap();
        answer["token"].as_str().unwrap().to_string()
    }

    fn mcp(&self, token: &str, method: &str, params: Value) -> Reply {
        let message = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let extra = format!("Authorization: Bearer {token}\r\nContent-Type: application/json\r\n");
        self.raw(
            "POST",
            &format!("{PREFIX}/mcp"),
            &extra,
            &message.to_string(),
        )
    }

    /// A tool call: true when it was refused, and the text.
    fn call(&self, token: &str, tool: &str, arguments: Value) -> (bool, String) {
        let reply = self.mcp(
            token,
            "tools/call",
            json!({"name": tool, "arguments": arguments}),
        );
        assert_eq!(reply.status, 200, "{}", reply.body);
        let answer: Value = serde_json::from_str(&reply.body).unwrap();
        let result = &answer["result"];
        assert!(result.is_object(), "{answer}");
        (
            result["isError"].as_bool().unwrap(),
            result["content"][0]["text"].as_str().unwrap().to_string(),
        )
    }

    fn read(&self, token: &str, file: &str) -> (bool, String) {
        self.call(token, "read", json!({ "file": file }))
    }

    fn log(&self) -> String {
        fs::read_to_string(self.dir.join("state/writes.log")).unwrap_or_default()
    }

    fn notified(&self) -> String {
        fs::read_to_string(self.dir.join("notified.txt")).unwrap_or_default()
    }

    /// The command line with the same settings as the door.
    fn command(&self, args: &[&str]) -> Output {
        program(&self.dir, "8").args(args).output().unwrap()
    }

    fn root_file(&self, path: &str) -> String {
        fs::read_to_string(self.dir.join("root").join(path.trim_start_matches('/'))).unwrap()
    }
}

impl Drop for Door {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// The notify program runs in the background, so its line may come a little later.
fn wait_for(what: impl Fn() -> bool) {
    for _ in 0..100 {
        if what() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_code_gives_a_token_once_and_the_token_opens_three_tools() {
    let door = Door::open("pair");
    let code = door.store().issue("tablet", device::now()).unwrap();
    let before = device::date_text(device::now() + TOKEN_LIFE);
    // Typed with small letters and a dash, as on the device.
    let reply = door.pair_with(&device::shown_code(&code).to_lowercase());
    let after = device::date_text(device::now() + TOKEN_LIFE);
    assert_eq!(reply.status, 200, "{}", reply.body);
    let answer: Value = serde_json::from_str(&reply.body).unwrap();
    let token = answer["token"].as_str().unwrap();
    assert_eq!(token.len(), 64);
    let expires = answer["expires"].as_str().unwrap();
    assert!(expires == before || expires == after, "{expires}");
    assert_eq!(
        answer.as_object().unwrap().len(),
        2,
        "only token and expires"
    );

    let again = door.pair_with(&code);
    assert_eq!(again.status, 400, "a code works once");
    assert_eq!(again.body, r#"{"error":"wrong_code"}"#);

    let tools = door.mcp(token, "tools/list", json!({}));
    let tools: Value = serde_json::from_str(&tools.body).unwrap();
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["read", "patch", "list"]);
    assert!(door.log().contains("\tpaired\t-\ttablet (reader)\t"));
    let file = fs::read_to_string(door.dir.join("state/devices.json")).unwrap();
    assert!(
        !file.contains(token) && !file.contains(&code),
        "hashes only"
    );
}

#[test]
fn an_old_code_or_a_wrong_one_gets_no_token_and_five_close_the_pairing() {
    let door = Door::open("old-code");
    let old = door
        .store()
        .issue("tablet", device::now() - CODE_LIFE - 1)
        .unwrap();
    assert_eq!(door.pair_with(&old).status, 400, "older than 5 minutes");
    for _ in 0..3 {
        assert_eq!(door.pair_with("ABCD-EFGH").status, 400);
    }
    assert_eq!(door.pair_with("no code").status, 400);
    let fresh = door.store().issue("tablet", device::now()).unwrap();
    let closed = door.pair_with(&fresh);
    assert_eq!(closed.status, 429, "{}", closed.body);
    assert_eq!(door.pair_with("{").status, 429);
    assert!(door.log().contains("\tpair-refused\t"));
}

#[test]
fn without_a_token_that_works_the_door_is_closed() {
    let door = Door::open("closed");
    let token = door.pair();
    assert_eq!(door.mcp("", "tools/list", json!({})).status, 401);
    let wrong = "0".repeat(64);
    assert_eq!(door.mcp(&wrong, "tools/list", json!({})).status, 401);
    assert_eq!(
        door.mcp(&device::hash(&token), "tools/list", json!({}))
            .status,
        401,
        "the hash is no token"
    );
    // No OAuth, no other place, and nothing outside the secret path.
    for (method, path) in [
        (
            "GET",
            format!("{PREFIX}/.well-known/oauth-authorization-server"),
        ),
        ("GET", "/.well-known/oauth-authorization-server".to_string()),
        ("POST", format!("{PREFIX}/register")),
        ("POST", format!("{PREFIX}/token")),
        ("GET", format!("{PREFIX}/authorize")),
        ("POST", "/mcp".to_string()),
        ("POST", "/pair".to_string()),
    ] {
        let extra = format!("Authorization: Bearer {token}\r\n");
        assert_eq!(
            door.raw(method, &path, &extra, "{}").status,
            404,
            "{method} {path}"
        );
    }
    assert_eq!(door.mcp(&token, "tools/list", json!({})).status, 200);
    let extra = format!("Authorization: Bearer {token}\r\n");
    assert_eq!(
        door.raw("GET", &format!("{PREFIX}/mcp"), &extra, "").status,
        405
    );
}

#[test]
fn every_other_tool_is_refused_with_the_policy_error() {
    let door = Door::open("tools");
    let token = door.pair();
    for (tool, arguments) in [
        (
            "write",
            json!({"file": PROGRESS_FILE, "who": "x", "text": "new\n"}),
        ),
        ("delete", json!({"file": PROGRESS_FILE, "who": "x"})),
        (
            "search",
            json!({"text": "Kapitel", "folder": "/books/", "who": "x"}),
        ),
        (
            "copy",
            json!({"pairs": [{"from": PROGRESS_FILE, "to": "/books/x.md"}], "who": "x"}),
        ),
        (
            "move",
            json!({"pairs": [{"from": PROGRESS_FILE, "to": "/books/x.md"}], "who": "x"}),
        ),
        ("download", json!({"files": [PROGRESS_FILE], "who": "x"})),
        ("upload", json!({"files": [PROGRESS_FILE], "who": "x"})),
        ("version", json!({})),
        ("an-invented-tool", json!({})),
    ] {
        let (refused, text) = door.call(&token, tool, arguments);
        assert!(refused, "{tool}");
        assert!(text.starts_with("exit code 2\n"), "{tool}: {text}");
        assert!(
            text.contains("offers only list, read, patch"),
            "{tool}: {text}"
        );
    }
    assert_eq!(door.root_file(PROGRESS_FILE), PROGRESS, "unchanged");
    assert!(!door.dir.join("root/books/x.md").exists());
    assert_eq!(door.log().matches("\trefused\t").count(), 9);
}

#[test]
fn reads_lists_and_patches_work_on_the_named_paths() {
    let door = Door::open("work");
    let token = door.pair();
    assert_eq!(
        door.read(&token, &format!("{BOOK}/chapter-01.txt")),
        (false, CHAPTER_1.to_string())
    );
    for file in [
        "/reading/glossary/an-invented-book/glossary-01.md",
        "/reading/summaries/an-invented-book/summary-01.md",
    ] {
        let (refused, text) = door.read(&token, file);
        assert!(!refused, "{file}: {text}");
    }

    let (refused, list) = door.call(&token, "list", json!({"folders": ["/books/"]}));
    assert!(!refused, "{list}");
    assert!(
        list.starts_with("/books/:\n"),
        "the device sees its own path: {list}"
    );
    assert!(list.contains("an-invented-book/"));
    assert!(
        !list.contains(&door.dir.display().to_string()),
        "no path of the disk"
    );
    let (refused, list) = door.call(&token, "list", json!({"folders": [BOOK]}));
    assert!(!refused && list.contains("chapter-01.txt"), "{list}");

    // A patch needs the read of the same device first, as on every door.
    let edit =
        json!({"old": "- Place: chapter 1, page 1\n", "new": "- Place: chapter 2, page 3\n"});
    let patch = json!({"file": PROGRESS_FILE, "edits": [edit], "who": "anything"});
    let (refused, text) = door.call(&token, "patch", patch.clone());
    assert!(refused && text.starts_with("exit code"), "{text}");
    assert_eq!(
        door.read(&token, PROGRESS_FILE),
        (false, PROGRESS.to_string())
    );
    let (refused, text) = door.call(&token, "patch", patch);
    assert!(!refused, "{text}");
    assert!(text.starts_with("patched: "), "{text}");
    assert!(door.root_file(PROGRESS_FILE).contains("chapter 2, page 3"));
    assert!(
        door.log().contains("\ttablet (reader)\t"),
        "the door names the device, not the who it was sent"
    );
    assert!(!door.log().contains("anything"));
}

#[test]
fn a_patch_never_makes_a_file() {
    let door = Door::open("no-new");
    let token = door.pair();
    let file = "/reading/progress-another-invented-book.md";
    let (refused, text) = door.read(&token, file);
    assert!(refused && text.starts_with("exit code 6\n"), "{text}");
    let (refused, text) = door.call(
        &token,
        "patch",
        json!({"file": file, "append": "- Place: 1\n"}),
    );
    assert!(refused, "{text}");
    assert!(
        text.starts_with("exit code 5\n"),
        "nothing read, nothing patched: {text}"
    );
    assert!(
        !door
            .dir
            .join("root")
            .join(file.trim_start_matches('/'))
            .exists()
    );
}

#[test]
fn path_tricks_and_closed_paths_are_refused() {
    let door = Door::open("tricks");
    let token = door.pair();
    for file in [
        "/books/../health/invented-health.md",
        "/books/an-invented-book/../../health/invented-health.md",
        "/books/./an-invented-book/chapter-01.txt",
        "/Books/an-invented-book/chapter-01.txt",
        "/BOOKS/an-invented-book/chapter-01.txt",
        "books/an-invented-book/chapter-01.txt",
        "/books//an-invented-book/chapter-01.txt",
        "//books/an-invented-book/chapter-01.txt",
        "/booksx/an-invented-book/chapter-01.txt",
        "/books",
        "/health/invented-health.md",
        "/reading/notes.md",
        "/reading/Progress-an-invented-book.md",
        "/books/an-invented-book/sneaky/invented-health.md",
        "/books/link.txt",
        "/",
        "",
    ] {
        let (refused, text) = door.read(&token, file);
        assert!(refused, "{file}: {text}");
        assert!(text.starts_with("exit code 2\n"), "{file}: {text}");
        assert!(!text.contains("invented private note"), "{file}");
    }
    for file in [
        "/reading/glossary/an-invented-book/glossary-01.md",
        "/books/an-invented-book/chapter-01.txt",
        "/reading/notes.md",
        "/reading//progress-an-invented-book.md",
        "/reading/../reading/progress-an-invented-book.md",
        "/reading/PROGRESS-an-invented-book.md",
    ] {
        let patch = json!({"file": file, "append": "an invented line\n"});
        let (refused, text) = door.call(&token, "patch", patch);
        assert!(
            refused && text.starts_with("exit code 2\n"),
            "{file}: {text}"
        );
    }
    for folder in [
        "/",
        "/reading/",
        "/notes/",
        "/health/",
        "/books/an-invented-book/sneaky/",
        "/books/../health/",
        "books/",
        "/Books/",
    ] {
        let (refused, text) = door.call(&token, "list", json!({"folders": [folder]}));
        assert!(
            refused && text.starts_with("exit code 2\n"),
            "{folder}: {text}"
        );
        assert!(!text.contains("invented-health"), "{folder}: {text}");
    }
    // One closed folder refuses the whole list.
    let both = json!({"folders": ["/books/", "/health/"]});
    let (refused, text) = door.call(&token, "list", both);
    assert!(refused && !text.contains("an-invented-book"), "{text}");
    assert_eq!(
        fs::read_to_string(door.dir.join("root/health/invented-health.md")).unwrap(),
        PRIVATE
    );
    assert!(door.log().contains(
        "\trefused\t/books/an-invented-book/sneaky/invented-health.md\ttablet (reader)\t"
    ));
}

#[test]
fn the_read_cap_refuses_logs_and_pushes_once() {
    let door = Door::open_with("cap", "2");
    let token = door.pair();
    for n in ["01", "02"] {
        let (refused, text) = door.read(&token, &format!("{BOOK}/chapter-{n}.txt"));
        assert!(!refused, "{text}");
    }
    // A file that is not capped still opens.
    let (refused, _) = door.read(&token, &format!("{BOOK}/contents.txt"));
    assert!(!refused);
    for _ in 0..3 {
        let (refused, text) = door.read(&token, &format!("{BOOK}/chapter-03.txt"));
        assert!(refused, "{text}");
        assert!(text.starts_with("exit code 2\n"), "{text}");
        assert!(text.contains("read cap of 2 in 60 minutes"), "{text}");
        assert!(!text.contains("Kapitel drei"));
    }
    assert_eq!(door.log().matches("\tcapped\t").count(), 3);
    wait_for(|| door.notified().contains("read cap"));
    std::thread::sleep(Duration::from_millis(200));
    let notified = door.notified();
    assert_eq!(
        notified.matches("read cap").count(),
        1,
        "one push an hour: {notified}"
    );
    assert!(notified.contains("tablet (reader)"), "{notified}");
    assert!(
        notified.contains("orgthread device revoke tablet"),
        "{notified}"
    );
    assert!(
        notified.contains("paired"),
        "a push at the pairing too: {notified}"
    );
    assert!(
        !notified.contains("chapter") && !notified.contains("Kapitel"),
        "no path, no text"
    );
}

#[test]
fn a_revoke_by_the_command_closes_the_door_at_once() {
    let door = Door::open("revoke");
    let token = door.pair();
    let listed = door.command(&["device", "list"]);
    let text = String::from_utf8_lossy(&listed.stdout);
    assert_eq!(listed.status.code(), Some(0));
    assert!(text.contains("tablet (reader)  paired"), "{text}");
    assert!(!text.contains(&token) && !text.contains(&device::hash(&token)));

    assert!(!door.read(&token, PROGRESS_FILE).0);
    let revoked = door.command(&["device", "revoke", "tablet"]);
    assert_eq!(revoked.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&revoked.stdout),
        "revoked: tablet (reader)\n"
    );
    assert_eq!(door.mcp(&token, "tools/list", json!({})).status, 401);
    assert!(door.log().contains("\trevoked\t-\ttablet (reader)\t"));
    let again = door.command(&["device", "revoke", "tablet"]);
    assert_eq!(again.status.code(), Some(6));
}

#[test]
fn the_pairing_code_shows_only_in_a_terminal() {
    let door = Door::open("no-terminal");
    let out = door.command(&["device", "pair", "tablet"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("only in a terminal"));
    let listed = door.command(&["device", "list"]);
    assert_eq!(
        String::from_utf8_lossy(&listed.stdout),
        "no device is paired\n"
    );
}

#[test]
fn the_doors_refuse_to_start_with_the_wrong_settings() {
    let dir = new_dir("start");
    sample(&dir);
    let port = 61_000 + (std::process::id() % 4_000) as u16;
    let address = format!("127.0.0.1:{port}");
    let device = ["serve", "--http", address.as_str(), "--device"];
    let run = |change: &[(&str, &str)], args: &[&str]| -> (Option<i32>, String) {
        let mut command = program(&dir, "8");
        for (name, value) in change {
            command.env(name, value);
        }
        // A door that starts by mistake would run for ever: stop it after 10 s.
        let err = dir.join("start-output.txt");
        let file = fs::File::create(&err).unwrap();
        let mut child = command
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::from(file))
            .spawn()
            .unwrap();
        let mut code = None;
        for _ in 0..500 {
            if let Some(status) = child.try_wait().unwrap() {
                code = status.code();
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if code.is_none() {
            let _ = child.kill();
            let _ = child.wait();
        }
        (code, fs::read_to_string(&err).unwrap_or_default())
    };
    let hash = "a".repeat(64);
    for (change, says) in [
        (vec![("ORGTHREAD_DEVICE_APP", "")], "ORGTHREAD_DEVICE_APP"),
        (
            vec![("ORGTHREAD_DEVICE_APP", "Reader App")],
            "ORGTHREAD_DEVICE_APP",
        ),
        (
            vec![("ORGTHREAD_LOGIN_HASH", hash.as_str())],
            "its own folder",
        ),
        (
            vec![
                ("ORGTHREAD_DEVICE_READ", ""),
                ("ORGTHREAD_DEVICE_PATCH", ""),
            ],
            "ORGTHREAD_DEVICE_READ, ORGTHREAD_DEVICE_PATCH or ORGTHREAD_DEVICE_LIST",
        ),
        (
            vec![("ORGTHREAD_DEVICE_LIST", "/lessons")],
            "ORGTHREAD_DEVICE_LIST",
        ),
        (
            vec![("ORGTHREAD_DEVICE_LIST", "lessons/")],
            "ORGTHREAD_DEVICE_LIST",
        ),
        (
            vec![("ORGTHREAD_DEVICE_LIST", "/lessons/../")],
            "ORGTHREAD_DEVICE_LIST",
        ),
        (
            vec![("ORGTHREAD_DEVICE_LIST", "/lessons//")],
            "ORGTHREAD_DEVICE_LIST",
        ),
        (
            vec![("ORGTHREAD_DEVICE_READ", "/books")],
            "ORGTHREAD_DEVICE_READ",
        ),
        (
            vec![("ORGTHREAD_DEVICE_READ", "/books/../")],
            "ORGTHREAD_DEVICE_READ",
        ),
        (
            vec![("ORGTHREAD_DEVICE_PATCH", "notes/a.md")],
            "ORGTHREAD_DEVICE_PATCH",
        ),
        (
            vec![("ORGTHREAD_DEVICE_CAP_HOUR", "0")],
            "ORGTHREAD_DEVICE_CAP_HOUR",
        ),
        (
            vec![("ORGTHREAD_DEVICE_CAP_DAY", "many")],
            "ORGTHREAD_DEVICE_CAP_DAY",
        ),
    ] {
        let (code, said) = run(&change, &device);
        assert_eq!(code, Some(2), "{change:?}: {said}");
        assert!(said.contains(says), "{change:?}: {said}");
    }
    // The full door does not start in the folder of a device door.
    let full = ["serve", "--http", address.as_str()];
    let (code, said) = run(&[("ORGTHREAD_LOGIN_HASH", hash.as_str())], &full);
    assert_eq!(code, Some(2), "{said}");
    assert!(said.contains("--device"), "{said}");
    // The same with only a list folder set.
    let list_only = [
        ("ORGTHREAD_LOGIN_HASH", hash.as_str()),
        ("ORGTHREAD_DEVICE_READ", ""),
        ("ORGTHREAD_DEVICE_PATCH", ""),
        ("ORGTHREAD_DEVICE_LIST", "/lessons/"),
    ];
    let (code, said) = run(&list_only, &full);
    assert_eq!(code, Some(2), "{said}");
    assert!(
        said.contains("--device") && said.contains("ORGTHREAD_DEVICE_LIST"),
        "{said}"
    );
    // A broken file of tokens stops the door at its start.
    fs::create_dir_all(dir.join("state")).unwrap();
    fs::write(dir.join("state/devices.json"), "{ broken").unwrap();
    let (code, said) = run(&[], &device);
    assert_eq!(code, Some(2), "{said}");
    assert!(said.contains("could not read the tokens"), "{said}");
    let _ = fs::remove_dir_all(dir);
}

/// T6: after a device read a chapter, no word of it is on the disk of the door,
/// in its log, in its file of tokens, in its notes, or in its output, which a
/// server sends to the journal.
#[test]
fn t6_no_text_of_a_book_reaches_the_disk_the_log_or_the_output() {
    let door = Door::open_with("t6", "1");
    let token = door.pair();
    let (refused, text) = door.read(&token, &format!("{BOOK}/chapter-01.txt"));
    assert!(!refused && text.contains(MARKER));
    // The cap, a refusal and a patch, so that every kind of line is written.
    assert!(door.read(&token, &format!("{BOOK}/chapter-02.txt")).0);
    assert!(door.read(&token, "/health/invented-health.md").0);
    assert!(!door.read(&token, PROGRESS_FILE).0);
    let edit =
        json!({"old": "- Place: chapter 1, page 1\n", "new": "- Place: chapter 1, page 2\n"});
    assert!(
        !door
            .call(
                &token,
                "patch",
                json!({"file": PROGRESS_FILE, "edits": [edit]})
            )
            .0
    );
    assert!(!door.call(&token, "list", json!({"folders": [BOOK]})).0);
    wait_for(|| door.notified().contains("read cap"));

    let mut searched = 0;
    let mut stack = vec![door.dir.clone()];
    while let Some(place) = stack.pop() {
        for entry in fs::read_dir(&place).unwrap() {
            let path = entry.unwrap().path();
            // The sample folder stands for Dropbox: the book is meant to be there.
            if path == door.dir.join("root") {
                continue;
            }
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                searched += 1;
                let bytes = fs::read(&path).unwrap();
                let text = String::from_utf8_lossy(&bytes);
                assert!(
                    !text.contains(MARKER),
                    "the marker is in {}",
                    path.display()
                );
                assert!(
                    !text.contains("Kapitel"),
                    "book text is in {}",
                    path.display()
                );
            }
        }
    }
    // The log, the tokens, the notes of the reads, the push and the output.
    assert!(searched >= 5, "searched {searched} files");
    assert!(door.dir.join("door-output.txt").exists());
    assert!(door.log().contains("\tcapped\t"));
}

/// The one rule for the shapes of the errors, which the reader page and the
/// Android app rely on: only a token problem gets HTTP 401, nothing gets 403,
/// a refused tool, a refused path and the read cap are tool results with
/// `isError` and `exit code N`, and a wrong or used code gets HTTP 400.
#[test]
fn only_a_token_problem_gets_401_and_every_refusal_is_a_tool_result() {
    let door = Door::open_with("shapes", "1");
    // A missing, unknown, expired and revoked token: 401, with an empty body.
    let call = json!({"name": "read", "arguments": {"file": PROGRESS_FILE}});
    assert_eq!(door.mcp("", "tools/call", call.clone()).status, 401);
    assert_eq!(
        door.mcp(&"1".repeat(64), "tools/call", call.clone()).status,
        401
    );
    let old = door
        .store()
        .issue("old-device", device::now() - TOKEN_LIFE - 10)
        .unwrap();
    let expired = door
        .store()
        .pair("reader", &old, device::now() - TOKEN_LIFE - 5)
        .unwrap()
        .unwrap();
    let reply = door.mcp(&expired.token, "tools/call", call.clone());
    assert_eq!((reply.status, reply.body.as_str()), (401, ""), "expired");
    let token = door.pair();

    // Each refusal of a tool, a path and the cap: 200, isError and exit code 2.
    let first = format!("{BOOK}/chapter-01.txt");
    assert!(!door.read(&token, &first).0);
    for (tool, arguments) in [
        ("write", json!({"file": PROGRESS_FILE, "text": "x\n"})),
        ("read", json!({"file": "/health/invented-health.md"})),
        (
            "read",
            json!({"file": "/books/../health/invented-health.md"}),
        ),
        ("patch", json!({"file": first, "append": "x\n"})),
        ("list", json!({"folders": ["/health/"]})),
        ("read", json!({"file": format!("{BOOK}/chapter-02.txt")})),
    ] {
        let reply = door.mcp(
            &token,
            "tools/call",
            json!({"name": tool, "arguments": arguments}),
        );
        assert_eq!(reply.status, 200, "{tool}: {}", reply.body);
        let answer: Value = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(answer["result"]["isError"], true, "{tool}: {answer}");
        let text = answer["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("exit code 2\n"), "{tool}: {text}");
    }
    assert!(
        door.log().contains("\tcapped\t"),
        "the last one was the cap"
    );

    // A wrong and a used code: 400 with an error in JSON.
    let code = door.store().issue("phone", device::now()).unwrap();
    assert_eq!(door.pair_with(&code).status, 200);
    for typed in [code.as_str(), "ABCD-EFGH"] {
        let reply = door.pair_with(typed);
        assert_eq!(reply.status, 400, "{typed}");
        let answer: Value = serde_json::from_str(&reply.body).unwrap();
        assert!(answer["error"].is_string(), "{answer}");
    }

    // A revoked token: 401.
    assert_eq!(
        door.command(&["device", "revoke", "tablet"]).status.code(),
        Some(0)
    );
    assert_eq!(door.mcp(&token, "tools/call", call).status, 401);
}

/// A `*` in a read folder stands for one folder name: `/books/*/` opens each
/// book folder and all it holds, never a file directly in `/books/`, such as a
/// whole book, and a list of `/books/` shows only the folder names.
#[test]
fn a_star_opens_the_book_folders_and_never_a_file_beside_them() {
    let door = Door::open("star");
    let token = door.pair();
    let (refused, text) = door.read(&token, "/books/an-invented-book.epub");
    assert!(refused, "{text}");
    assert!(text.starts_with("exit code 2\n"), "{text}");
    assert!(!text.contains("whole book"), "{text}");
    assert!(
        door.log()
            .contains("\trefused\t/books/an-invented-book.epub\t")
    );

    let (refused, text) = door.read(&token, &format!("{BOOK}/contents.txt"));
    assert!(!refused, "a file in a book folder opens: {text}");

    // The * never covers a /: a file directly in /books/, or one reached by a
    // way around, stays closed.
    for closed in [
        "/books/chapter-01.txt",
        "/books/an-invented-book/../an-invented-book.epub",
        "/books//an-invented-book.epub",
    ] {
        let (refused, text) = door.read(&token, closed);
        assert!(
            refused && text.starts_with("exit code 2\n"),
            "{closed}: {text}"
        );
    }

    for folder in ["/books/", "/books"] {
        let (refused, list) = door.call(&token, "list", json!({"folders": [folder]}));
        assert!(!refused, "{list}");
        assert!(list.starts_with(&format!("{folder}:\n")), "{list}");
        assert!(list.contains("an-invented-book/"), "{list}");
        assert!(!list.contains(".epub"), "no file name at the root: {list}");
        assert!(!list.contains("link.txt"), "{list}");
    }
    let both = json!({"folders": ["/books/", BOOK]});
    let (refused, list) = door.call(&token, "list", both);
    assert!(!refused, "{list}");
    assert!(
        !list.contains(".epub") && list.contains("chapter-01.txt"),
        "{list}"
    );
}

/// The settings of a door for an app `lessons` that may only list `/lessons/`.
const LIST_ONLY: [(&str, &str); 5] = [
    ("ORGTHREAD_DEVICE_APP", "lessons"),
    ("ORGTHREAD_DEVICE_READ", ""),
    ("ORGTHREAD_DEVICE_PATCH", ""),
    ("ORGTHREAD_DEVICE_CAPPED", ""),
    ("ORGTHREAD_DEVICE_LIST", "/lessons/"),
];

/// The files outside the sample folder that hold `word`, and how many files
/// were searched.
fn files_with(dir: &Path, word: &str) -> (usize, Vec<PathBuf>) {
    let (mut searched, mut hits) = (0, Vec::new());
    let mut stack = vec![dir.to_path_buf()];
    while let Some(place) = stack.pop() {
        for entry in fs::read_dir(&place).unwrap() {
            let path = entry.unwrap().path();
            if path == dir.join("root") {
                continue;
            }
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                searched += 1;
                if String::from_utf8_lossy(&fs::read(&path).unwrap()).contains(word) {
                    hits.push(path);
                }
            }
        }
    }
    (searched, hits)
}

#[test]
fn a_door_with_only_a_list_folder_starts_lists_and_reads_nothing() {
    // The door starts with ORGTHREAD_DEVICE_LIST alone: open_changed stops the
    // test when the door stops at its start.
    let door = Door::open_changed("list-only", "8", &LIST_ONLY);
    let token = door.pair();

    let (refused, list) = door.call(&token, "list", json!({"folders": ["/lessons/"]}));
    assert!(!refused, "{list}");
    assert!(list.starts_with("/lessons/:\n"), "{list}");
    for name in [
        "Lesson 1 Mon.pdf",
        "Lesson 1 Mon.pdf.mark",
        "00 Week.json",
        "Archive/",
    ] {
        assert!(list.contains(name), "{name}: {list}");
    }
    let (refused, deeper) = door.call(
        &token,
        "list",
        json!({"folders": ["/lessons/Archive/2026/11/"]}),
    );
    assert!(!refused && deeper.contains("Lesson 0 Sun.pdf"), "{deeper}");

    // With fingerprint, each file shows the content hash that Dropbox reports.
    let (refused, printed) = door.call(
        &token,
        "list",
        json!({"folders": ["/lessons/", "/lessons/Archive/2026/11/"], "fingerprint": true}),
    );
    assert!(!refused, "{printed}");
    for (name, text) in LESSONS {
        let file_name = name.rsplit('/').next().unwrap();
        let print = orgthread::fingerprint::fingerprint(text.as_bytes());
        let line = printed
            .lines()
            .find(|l| l.ends_with(&format!("  {file_name}")))
            .unwrap_or_else(|| panic!("no line for {name}: {printed}"));
        assert!(
            line.ends_with(&format!("{print}  {file_name}")),
            "{name}: {line}"
        );
    }
    let print = orgthread::fingerprint::fingerprint(LESSONS[0].1.as_bytes());
    let shown = door.command(&[
        "fingerprint",
        &format!("{}{LESSON}", door.dir.join("root").display()),
    ]);
    assert_eq!(
        String::from_utf8_lossy(&shown.stdout).trim(),
        print,
        "the same as orgthread fingerprint"
    );

    // A read or a patch of any file in it is refused, as a closed path is.
    let mut refusals = 0;
    for (name, text) in LESSONS {
        let file = format!("/lessons/{name}");
        let (refused, said) = door.read(&token, &file);
        assert!(
            refused && said.starts_with("exit code 2\n"),
            "{file}: {said}"
        );
        assert!(said.contains("may only list"), "{said}");
        assert!(!said.contains(LESSON_MARKER), "{said}");
        let (refused, said) = door.call(
            &token,
            "patch",
            json!({"file": file, "append": "an invented line\n"}),
        );
        assert!(
            refused && said.starts_with("exit code 2\n"),
            "{file}: {said}"
        );
        assert_eq!(door.root_file(&file), text, "unchanged");
        refusals += 2;
    }
    // Every other path stays closed.
    for folder in ["/", "/books/", "/Lessons/", "/lessons/../books/"] {
        assert!(
            door.call(&token, "list", json!({"folders": [folder]})).0,
            "{folder}"
        );
        refusals += 1;
    }
    assert!(door.read(&token, &format!("{BOOK}/chapter-01.txt")).0);
    refusals += 1;

    let log = door.log();
    assert_eq!(log.matches("\trefused\t").count(), refusals, "{log}");
    assert!(
        log.contains(&format!("\trefused\t{LESSON}\ttablet (lessons)\t")),
        "{log}"
    );
    let (searched, hits) = files_with(&door.dir, LESSON_MARKER);
    assert!(searched >= 3, "searched {searched} files");
    assert!(hits.is_empty(), "the text of a lesson is in {hits:?}");
}

#[test]
fn a_list_costs_no_read_and_a_list_folder_beats_a_read_folder() {
    let change = [
        (
            "ORGTHREAD_DEVICE_READ",
            "/books/*/, /reading/glossary/, /reading/summaries/, /lessons/",
        ),
        ("ORGTHREAD_DEVICE_LIST", "/lessons/"),
    ];
    let door = Door::open_changed("list-cap", "1", &change);
    let token = door.pair();
    for _ in 0..3 {
        let (refused, list) = door.call(
            &token,
            "list",
            json!({"folders": ["/lessons/", BOOK], "fingerprint": true}),
        );
        assert!(!refused, "{list}");
    }
    // The cap of 1 is still free after the lists, and then full.
    let (refused, text) = door.read(&token, &format!("{BOOK}/chapter-01.txt"));
    assert!(!refused, "{text}");
    let (refused, text) = door.read(&token, &format!("{BOOK}/chapter-02.txt"));
    assert!(refused && text.contains("read cap"), "{text}");
    let listed = door.command(&["device", "list"]);
    assert!(
        String::from_utf8_lossy(&listed.stdout).contains("capped reads: 1 in 60 minutes"),
        "{}",
        String::from_utf8_lossy(&listed.stdout)
    );
    // ORGTHREAD_DEVICE_READ names /lessons/ too, and the list rule wins.
    let (refused, said) = door.read(&token, LESSON);
    assert!(refused && said.contains("may only list"), "{said}");
    assert!(!said.contains(LESSON_MARKER));
    // Beside it, a read folder reads as before: the glossary is not capped.
    assert!(
        !door
            .read(&token, "/reading/glossary/an-invented-book/glossary-01.md")
            .0
    );
}

// The tools of a sync: download, upload and trash.

/// A path for a query, with `%XX` for each sign that is not plain, as
/// `URLEncoder` of Java gives it, but with `%20` for a space.
fn encoded(path: &str) -> String {
    path.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'*' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The settings of a door for a sync of `/notes/`.
const SYNC: [(&str, &str); 4] = [
    ("ORGTHREAD_DEVICE_DOWNLOAD", "/notes/"),
    ("ORGTHREAD_DEVICE_UPLOAD", "/notes/"),
    ("ORGTHREAD_DEVICE_TRASH", "/notes/"),
    ("ORGTHREAD_BINARY_ALLOW", "/notes/"),
];

fn sync_door(name: &str, more: &[(&str, &str)]) -> Door {
    let mut change = SYNC.to_vec();
    change.extend_from_slice(more);
    Door::open_changed(name, "8", &change)
}

fn print_of(bytes: &[u8]) -> String {
    orgthread::fingerprint::fingerprint(bytes)
}

/// The exit code and the text of an answer that is no success.
fn not_done(reply: &Bytes) -> (i64, String) {
    assert_eq!(
        reply.status,
        422,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    let answer = reply.json();
    (
        answer["exit"].as_i64().unwrap(),
        answer["text"].as_str().unwrap().to_string(),
    )
}

#[test]
fn a_door_without_the_new_settings_refuses_download_upload_and_trash() {
    let door = Door::open("no-sync");
    let token = door.pair();
    let chapter = format!("{BOOK}/chapter-01.txt");
    let (code, text) = not_done(&door.download(&token, &chapter));
    assert_eq!(code, 2);
    assert!(text.starts_with("exit code 2\n"), "{text}");
    assert!(text.contains("offers no download"), "{text}");
    assert!(!text.contains("Kapitel"), "{text}");
    let (code, text) = not_done(&door.upload(&token, PROGRESS_FILE, None, b"new\n"));
    assert_eq!(code, 2);
    assert!(text.contains("offers no upload"), "{text}");
    assert_eq!(door.root_file(PROGRESS_FILE), PROGRESS, "unchanged");
    let (refused, text) = door.call(
        &token,
        "trash",
        json!({"file": PROGRESS_FILE, "fingerprint": "0".repeat(64)}),
    );
    assert!(refused && text.starts_with("exit code 2\n"), "{text}");
    assert!(text.contains("offers only list, read, patch"), "{text}");
    assert_eq!(door.root_file(PROGRESS_FILE), PROGRESS, "unchanged");
    assert_eq!(door.log().matches("\trefused\t").count(), 3);
    // A download is no read either: the cap of the reads did not count it.
    assert!(!door.log().contains("\tdownloaded\t"));
    // Without a token, the new places are closed as /mcp is.
    assert_eq!(door.download("", &chapter).status, 401);
    assert_eq!(door.upload("", PROGRESS_FILE, None, b"x").status, 401);
}

#[test]
fn binary_bytes_round_trip_exactly() {
    let door = sync_door("round-trip", &[]);
    let token = door.pair();
    let tools = door.mcp(&token, "tools/list", json!({}));
    let tools: Value = serde_json::from_str(&tools.body).unwrap();
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["read", "patch", "list", "trash"]);

    let got = door.download(&token, "/notes/a.note");
    assert_eq!(got.status, 200, "{}", String::from_utf8_lossy(&got.body));
    assert_eq!(got.body, note_bytes(1), "the exact bytes");
    let seen = got.header("X-Orgthread-Fingerprint").unwrap();
    assert_eq!(seen, print_of(&note_bytes(1)));
    assert_eq!(
        got.header("Content-Type").unwrap(),
        "application/octet-stream"
    );

    // The device changed the note and sends it back, with what it saw.
    let mut changed = note_bytes(1);
    changed.extend_from_slice(&[0, 255, 13, 10, 0]);
    let sent = door.upload(&token, "/notes/a.note", Some(&seen), &changed);
    assert_eq!(sent.status, 200, "{}", String::from_utf8_lossy(&sent.body));
    let answer = sent.json();
    assert_eq!(answer["exit"], 0, "{answer}");
    assert_eq!(answer["fingerprint"], print_of(&changed), "{answer}");
    assert_eq!(answer["size"], changed.len(), "{answer}");
    assert_eq!(door.root_bytes("/notes/a.note").unwrap(), changed);
    let again = door.download(&token, "/notes/a.note");
    assert_eq!(again.body, changed);
    assert_eq!(
        again.header("X-Orgthread-Fingerprint").unwrap(),
        print_of(&changed)
    );

    // A new note, in a new folder, with a space and an umlaut in its name.
    let new = "/notes/2026 Woche/Übung 1.note";
    let sent = door.upload(&token, new, None, &note_bytes(9));
    assert_eq!(sent.status, 200, "{}", String::from_utf8_lossy(&sent.body));
    assert_eq!(door.root_bytes(new).unwrap(), note_bytes(9));
    assert!(door.log().contains("\twritten\t"), "{}", door.log());
    assert!(door.log().contains("\tdownloaded\t"), "{}", door.log());
}

#[test]
fn paths_outside_the_named_folders_are_refused() {
    let door = sync_door("sync-paths", &[]);
    let token = door.pair();
    for file in [
        format!("{BOOK}/chapter-01.txt"),
        PROGRESS_FILE.to_string(),
        "/notes/../books/an-invented-book/chapter-01.txt".into(),
        "/notes/./a.note".into(),
        "/Notes/a.note".into(),
        "notes/a.note".into(),
        "/notes//a.note".into(),
        "/notes/".into(),
        "/notes/.trash/2026-11-25/a.note".into(),
        "/notesx/a.note".into(),
    ] {
        let (code, text) = not_done(&door.download(&token, &file));
        assert_eq!(code, 2, "{file}: {text}");
        let (code, text) = not_done(&door.upload(&token, &file, None, b"x"));
        assert_eq!(code, 2, "{file}: {text}");
        let (refused, text) = door.call(
            &token,
            "trash",
            json!({"file": file, "fingerprint": print_of(&note_bytes(1))}),
        );
        assert!(
            refused && text.starts_with("exit code 2\n"),
            "{file}: {text}"
        );
    }
    assert_eq!(door.root_file(PROGRESS_FILE), PROGRESS, "unchanged");
    assert!(!door.dir.join("root/notesx").exists());
    assert!(!door.dir.join("root/notes/.trash").exists());
    assert!(door.log().matches("\trefused\t").count() >= 30);
    // A link inside the folder is never followed.
    std::os::unix::fs::symlink(
        door.dir.join("root/health"),
        door.dir.join("root/notes/sneaky"),
    )
    .unwrap();
    let (code, text) = not_done(&door.download(&token, "/notes/sneaky/invented-health.md"));
    assert_eq!(code, 2, "{text}");
    assert!(!text.contains("private"), "{text}");
}

#[test]
fn a_file_over_the_limit_is_refused() {
    let door = sync_door("sync-limit", &[("ORGTHREAD_DEVICE_FILE_MB", "1")]);
    let token = door.pair();
    let big: Vec<u8> = (0..1_500_000u32).map(|i| (i % 251) as u8).collect();
    fs::write(door.dir.join("root/notes/big.note"), &big).unwrap();
    let (code, text) = not_done(&door.download(&token, "/notes/big.note"));
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("more than the limit of 1 MB"), "{text}");
    let seen = print_of(&note_bytes(1));
    let (code, text) = not_done(&door.upload(&token, "/notes/a.note", Some(&seen), &big));
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("more than the limit of 1 MB"), "{text}");
    assert_eq!(door.root_bytes("/notes/a.note").unwrap(), note_bytes(1));
    // Below the limit, the same door moves the file.
    assert_eq!(door.download(&token, "/notes/a.note").status, 200);
}

#[test]
fn the_day_cap_refuses_logs_and_pushes_once() {
    let door = sync_door("sync-cap", &[("ORGTHREAD_DEVICE_CAP_MB_DAY", "1")]);
    let token = door.pair();
    let half: Vec<u8> = (0..600_000u32).map(|i| (i % 249) as u8).collect();
    fs::write(door.dir.join("root/notes/half.note"), &half).unwrap();
    assert_eq!(door.download(&token, "/notes/half.note").status, 200);
    for _ in 0..2 {
        let (code, text) = not_done(&door.download(&token, "/notes/half.note"));
        assert_eq!(code, 2, "{text}");
        assert!(text.contains("cap of 1 MB in 24 hours"), "{text}");
    }
    // Uploads count in the same cap.
    let seen = print_of(&half);
    let (code, text) = not_done(&door.upload(&token, "/notes/half.note", Some(&seen), &half));
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("cap of 1 MB"), "{text}");
    // A small file still fits.
    assert_eq!(door.download(&token, "/notes/a.note").status, 200);
    assert_eq!(door.log().matches("\tcapped\t").count(), 3);
    wait_for(|| door.notified().contains("downloads and uploads"));
    std::thread::sleep(Duration::from_millis(200));
    let notified = door.notified();
    assert_eq!(
        notified.matches("downloads and uploads until").count(),
        1,
        "one push an hour: {notified}"
    );
    assert!(!notified.contains("half.note"), "no path: {notified}");
    // The cap of the reads is not touched.
    let devices = fs::read_to_string(door.dir.join("state/devices.json")).unwrap();
    let devices: Value = serde_json::from_str(&devices).unwrap();
    assert_eq!(devices["tokens"][0]["reads"], json!([]), "{devices}");
}

#[test]
fn upload_refuses_a_stale_fingerprint_and_a_file_it_did_not_see() {
    let door = sync_door("sync-stale", &[]);
    let token = door.pair();
    // Another device changed the note since this one saw it.
    let seen = print_of(&note_bytes(1));
    fs::write(door.dir.join("root/notes/a.note"), note_bytes(5)).unwrap();
    let (code, text) = not_done(&door.upload(&token, "/notes/a.note", Some(&seen), &note_bytes(7)));
    assert_eq!(code, 4, "{text}");
    assert!(text.contains("changed"), "{text}");
    assert_eq!(door.root_bytes("/notes/a.note").unwrap(), note_bytes(5));
    // With no fingerprint, a file that exists is never replaced.
    let (code, text) = not_done(&door.upload(&token, "/notes/sub/b.note", None, &note_bytes(7)));
    assert_eq!(code, 5, "{text}");
    assert_eq!(door.root_bytes("/notes/sub/b.note").unwrap(), note_bytes(2));
    // A file that is gone since the device saw it is not made again.
    fs::remove_file(door.dir.join("root/notes/sub/b.note")).unwrap();
    let seen = print_of(&note_bytes(2));
    let (code, text) =
        not_done(&door.upload(&token, "/notes/sub/b.note", Some(&seen), &note_bytes(7)));
    assert_eq!(code, 6, "{text}");
    assert!(door.root_bytes("/notes/sub/b.note").is_none());
    // A fingerprint that is no fingerprint is wrong use.
    let (code, _) = not_done(&door.upload(&token, "/notes/a.note", Some("abc"), &note_bytes(7)));
    assert_eq!(code, 2);
    // The right one works.
    let seen = print_of(&note_bytes(5));
    let sent = door.upload(&token, "/notes/a.note", Some(&seen), &note_bytes(7));
    assert_eq!(sent.status, 200, "{}", String::from_utf8_lossy(&sent.body));
}

#[test]
fn trash_moves_into_the_trash_of_the_day_and_never_over_a_file() {
    let door = sync_door("sync-trash", &[]);
    let token = door.pair();
    let day = device::date_text(device::now());
    let trash = door.dir.join(format!("root/notes/.trash/{day}"));
    // A file of the same name went to the trash earlier that day.
    fs::create_dir_all(trash.join("sub")).unwrap();
    fs::write(trash.join("sub/b.note"), b"an earlier invented note").unwrap();
    // A stale fingerprint moves nothing.
    let (refused, text) = door.call(
        &token,
        "trash",
        json!({"file": "/notes/sub/b.note", "fingerprint": print_of(b"other")}),
    );
    assert!(refused && text.starts_with("exit code 4\n"), "{text}");
    assert_eq!(door.root_bytes("/notes/sub/b.note").unwrap(), note_bytes(2));
    let (refused, text) = door.call(
        &token,
        "trash",
        json!({"file": "/notes/sub/b.note", "fingerprint": print_of(&note_bytes(2))}),
    );
    assert!(!refused, "{text}");
    assert!(
        text.contains(&format!("notes/.trash/{day}/sub/b (2).note")),
        "{text}"
    );
    assert!(
        !text.contains(&door.dir.display().to_string()),
        "no disk path: {text}"
    );
    assert_eq!(
        fs::read(trash.join("sub/b.note")).unwrap(),
        b"an earlier invented note",
        "the earlier file stays"
    );
    assert_eq!(
        fs::read(trash.join("sub/b (2).note")).unwrap(),
        note_bytes(2)
    );
    assert!(door.root_bytes("/notes/sub/b.note").is_none());
    // A file directly in the folder goes to the top of the trash of the day.
    let (refused, text) = door.call(
        &token,
        "trash",
        json!({"file": "/notes/a.note", "fingerprint": print_of(&note_bytes(1))}),
    );
    assert!(!refused, "{text}");
    assert_eq!(fs::read(trash.join("a.note")).unwrap(), note_bytes(1));
    // No fingerprint is wrong use, and moves nothing.
    let reply = door.mcp(
        &token,
        "tools/call",
        json!({"name": "trash", "arguments": {"file": "/notes/x.note"}}),
    );
    assert!(reply.body.contains("-32602"), "{}", reply.body);
}
