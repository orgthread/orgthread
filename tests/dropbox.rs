//! The commands against a small stand-in for the Dropbox API, on invented files.
//! The stand-in follows the Dropbox docs. The real proof runs against Dropbox.

use orgthread::fingerprint::fingerprint;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

const GOOD_TOKEN: &str = "invented-token-for-tests";
const FILE: &str = "/org/notes/invented.md";
const LOCK: &str = "/org/.locks/org--notes--invented.md.lock";

struct Stored {
    bytes: Vec<u8>,
    rev: u64,
    modified: String,
}

#[derive(Default)]
struct State {
    files: HashMap<String, Stored>,
    next_rev: u64,
    /// The next update finds a newer version, as if a second writer came first.
    second_writer: bool,
}

impl State {
    fn put(&mut self, path: &str, bytes: &[u8]) {
        self.next_rev += 1;
        let stored = Stored {
            bytes: bytes.to_vec(),
            rev: self.next_rev,
            modified: "2026-11-07T10:00:00Z".to_string(),
        };
        self.files.insert(path.to_string(), stored);
    }
}

fn reply(stream: &mut TcpStream, status: u16, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
}

fn error(stream: &mut TcpStream, summary: &str) {
    let body = json!({ "error_summary": summary }).to_string();
    reply(stream, 409, body.as_bytes());
}

fn serve_one(mut stream: TcpStream, state: &Mutex<State>) {
    let mut data = Vec::new();
    let mut buf = [0u8; 8192];
    let end = loop {
        let n = stream.read(&mut buf).unwrap_or(0);
        if n == 0 {
            return;
        }
        data.extend_from_slice(&buf[..n]);
        if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&data[..end]).to_string();
    let header = |name: &str| {
        head.lines()
            .find_map(|l| {
                l.split_once(": ")
                    .filter(|(k, _)| k.eq_ignore_ascii_case(name))
            })
            .map(|(_, v)| v.to_string())
    };
    let length: usize = header("Content-Length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    while data.len() < end + length {
        let n = stream.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
    }
    let body = data[end..].to_vec();
    if header("Authorization").as_deref() != Some(&format!("Bearer {GOOD_TOKEN}")) {
        return reply(&mut stream, 401, b"bad token");
    }
    let endpoint = head.split_whitespace().nth(1).unwrap_or("").to_string();
    let arg: Value = match header("Dropbox-API-Arg") {
        Some(text) => serde_json::from_str(&text).unwrap(),
        None => serde_json::from_slice(&body).unwrap_or(Value::Null),
    };
    let path = arg["path"].as_str().unwrap_or("").to_string();
    let mut s = state.lock().unwrap();
    match endpoint.as_str() {
        "/2/files/download" => match s.files.get(&path) {
            Some(file) => reply(&mut stream, 200, &file.bytes),
            None => error(&mut stream, "path/not_found/.."),
        },
        "/2/files/get_metadata" => match s.files.get(&path) {
            Some(file) => {
                let v = json!({
                    ".tag": "file",
                    "rev": file.rev.to_string(),
                    "content_hash": fingerprint(&file.bytes),
                    "server_modified": file.modified,
                });
                reply(&mut stream, 200, v.to_string().as_bytes());
            }
            None => error(&mut stream, "path/not_found/.."),
        },
        "/2/files/upload" => {
            assert_eq!(arg["autorename"], false);
            assert_eq!(arg["strict_conflict"], true);
            let current = s.files.get(&path).map(|f| f.rev.to_string());
            let ok = match &arg["mode"] {
                Value::String(mode) if mode == "add" => current.is_none(),
                Value::String(mode) if mode == "overwrite" => true,
                mode => {
                    let second = std::mem::take(&mut s.second_writer);
                    !second && current.as_deref() == mode["update"].as_str()
                }
            };
            if ok {
                s.put(&path, &body);
                reply(&mut stream, 200, b"{}");
            } else {
                error(&mut stream, "path/conflict/file/..");
            }
        }
        "/2/files/delete_v2" => match s.files.get(&path).map(|f| f.rev.to_string()) {
            None => error(&mut stream, "path_lookup/not_found/.."),
            Some(rev) if arg["parent_rev"].as_str().is_some_and(|want| want != rev) => {
                error(&mut stream, "path_write/conflict/file/..");
            }
            Some(_) => {
                s.files.remove(&path);
                reply(&mut stream, 200, b"{}");
            }
        },
        _ => reply(&mut stream, 404, b"no such endpoint"),
    }
}

struct Fake {
    url: String,
    state: Arc<Mutex<State>>,
    dir: PathBuf,
}

impl Fake {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("orgthread-dbx-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let state = Arc::new(Mutex::new(State::default()));
        let shared = Arc::clone(&state);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                serve_one(stream, &shared);
            }
        });
        Fake { url, state, dir }
    }
    fn run_with(&self, token: Option<&str>, args: &[&str], stdin: &[u8]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_orgthread"));
        c.args(args)
            .env("ORGTHREAD_STORAGE", "dropbox")
            .env("ORGTHREAD_DROPBOX_API", &self.url)
            .env("ORGTHREAD_DROPBOX_CONTENT", &self.url)
            .env("ORGTHREAD_LOG", self.dir.join("log"))
            .env("ORGTHREAD_READS", self.dir.join("reads"))
            .env("ORGTHREAD_VERSIONS", self.dir.join("versions"))
            .env("NO_PROXY", "127.0.0.1")
            .env("no_proxy", "127.0.0.1")
            .env_remove("ORGTHREAD_DROPBOX_TOKEN")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(token) = token {
            c.env("ORGTHREAD_DROPBOX_TOKEN", token);
        }
        let mut child = c.spawn().unwrap();
        let _ = child.stdin.take().unwrap().write_all(stdin);
        child.wait_with_output().unwrap()
    }
    fn run(&self, args: &[&str], stdin: &[u8]) -> Output {
        self.run_with(Some(GOOD_TOKEN), args, stdin)
    }
    fn read(&self, who: &str) -> Output {
        self.run(&["read", FILE, "--who", who], b"")
    }
    fn write(&self, who: &str, text: &[u8]) -> Output {
        self.run(&["write", FILE, "--who", who], text)
    }
    fn put(&self, path: &str, bytes: &[u8]) {
        self.state.lock().unwrap().put(path, bytes);
    }
    fn get(&self, path: &str) -> Option<Vec<u8>> {
        self.state
            .lock()
            .unwrap()
            .files
            .get(path)
            .map(|f| f.bytes.clone())
    }
    fn names(&self) -> Vec<String> {
        let mut names: Vec<_> = self.state.lock().unwrap().files.keys().cloned().collect();
        names.sort();
        names
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap()
}

#[test]
fn new_read_change_all_work_and_every_byte_survives() {
    let f = Fake::new();
    let text = "Grüße,  odd spacing\r\nno last newline".as_bytes();
    let o = f.write("chat", text);
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(f.get(FILE).unwrap(), text);
    let o = f.read("chat");
    assert_eq!(o.stdout, text);
    assert_eq!(code(&f.write("chat", b"next\n")), 0);
    assert_eq!(f.get(FILE).unwrap(), b"next\n");
    assert_eq!(f.names(), [FILE], "no lock and no other copy stays");
}

#[test]
fn a_path_with_german_letters_works() {
    let f = Fake::new();
    let path = "/org/notes/Grüße.md";
    let o = f.run(&["write", path, "--who", "chat"], b"text\n");
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(f.get(path).unwrap(), b"text\n");
}

#[test]
fn an_edit_after_the_read_is_refused_and_survives() {
    let f = Fake::new();
    f.put(FILE, b"old\n");
    f.read("chat");
    f.put(FILE, b"edited\n");
    assert_eq!(code(&f.write("chat", b"new\n")), 4);
    assert_eq!(f.get(FILE).unwrap(), b"edited\n");
    assert_eq!(f.names(), [FILE]);
}

#[test]
fn a_second_writer_loses_with_no_conflicted_copy() {
    let f = Fake::new();
    f.put(FILE, b"old\n");
    f.read("chat");
    f.state.lock().unwrap().second_writer = true;
    let o = f.write("chat", b"new\n");
    assert_eq!(code(&o), 4, "{o:?}");
    assert_eq!(f.get(FILE).unwrap(), b"old\n");
    assert_eq!(f.names(), [FILE], "no conflicted copy and no lock");
}

#[test]
fn an_unread_file_is_refused() {
    let f = Fake::new();
    f.put(FILE, b"here\n");
    assert_eq!(code(&f.write("chat", b"new\n")), 5);
    assert_eq!(f.get(FILE).unwrap(), b"here\n");
}

#[test]
fn a_file_gone_since_the_read_gives_exit_6() {
    let f = Fake::new();
    f.put(FILE, b"old\n");
    f.read("chat");
    f.state.lock().unwrap().files.remove(FILE);
    assert_eq!(code(&f.write("chat", b"new\n")), 6);
}

#[test]
fn a_fresh_lock_is_respected_and_a_stale_one_taken_over() {
    let f = Fake::new();
    f.put(FILE, b"old\n");
    f.read("chat");
    f.put(LOCK, b"other writer\n");
    let now = std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .unwrap();
    let now = String::from_utf8(now.stdout).unwrap().trim().to_string();
    f.state
        .lock()
        .unwrap()
        .files
        .get_mut(LOCK)
        .unwrap()
        .modified = now;
    let o = f.write("chat", b"new\n");
    assert_eq!(code(&o), 3, "{o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("other writer"));
    assert_eq!(f.get(FILE).unwrap(), b"old\n");
    assert!(f.get(LOCK).is_some());

    // The stand-in's old time makes the lock stale.
    f.state
        .lock()
        .unwrap()
        .files
        .get_mut(LOCK)
        .unwrap()
        .modified = "2020-01-01T00:00:00Z".to_string();
    let o = f.write("chat", b"new\n");
    assert_eq!(code(&o), 0, "{o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("other writer"));
    assert_eq!(f.names(), [FILE]);
}

#[test]
fn a_wrong_token_fails_clearly_and_is_never_shown() {
    let f = Fake::new();
    f.put(FILE, b"old\n");
    let o = f.run_with(
        Some("wrong-invented-token"),
        &["write", FILE, "--who", "c"],
        b"new\n",
    );
    assert_eq!(code(&o), 1, "{o:?}");
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("token"), "{err}");
    assert!(!err.contains("wrong-invented-token"), "{err}");
    assert_eq!(f.get(FILE).unwrap(), b"old\n");
}

#[test]
fn a_missing_token_and_a_bad_path_are_wrong_use() {
    let f = Fake::new();
    assert_eq!(
        code(&f.run_with(None, &["read", FILE, "--who", "c"], b"")),
        2
    );
    for path in ["org/notes/a.md", "/org/../a.md", "/", "/org/"] {
        let o = f.run(&["write", path, "--who", "c"], b"x");
        assert_eq!(code(&o), 2, "{path} {o:?}");
    }
    assert!(f.names().is_empty());
}

#[test]
fn no_text_stays_on_the_disk() {
    let f = Fake::new();
    assert_eq!(code(&f.write("chat", b"secret marker word\n")), 0);
    assert_eq!(code(&f.write("chat", b"secret marker word 2\n")), 0);
    assert!(
        !f.dir.join("versions").exists(),
        "no copies in Dropbox mode"
    );
    for entry in fs::read_dir(&f.dir).unwrap().flatten() {
        let walk = if entry.path().is_dir() {
            fs::read_dir(entry.path())
                .unwrap()
                .flatten()
                .map(|e| e.path())
                .collect()
        } else {
            vec![entry.path()]
        };
        for file in walk {
            let text = fs::read(&file).unwrap();
            let found = text.windows(6).any(|w| w == b"secret");
            assert!(!found, "text found in {}", file.display());
        }
    }
}
