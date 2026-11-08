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
use std::time::{Duration, SystemTime};

const GOOD_TOKEN: &str = "invented-token-for-tests";
const FILE: &str = "/org/notes/invented.md";
const LOCK: &str = "org--notes--invented.md.lock";

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
    /// The next update stores this in place of what was sent, as if a person edited it.
    edit_instead: Option<Vec<u8>>,
    /// Entries per page of `list_folder`, so that a test sees the cursor at work.
    page: usize,
    /// How many `list_folder` calls came in, the first page and the next ones.
    list_calls: u32,
}

impl State {
    /// The entries of a folder as `list_folder` gives them, or why not.
    fn children(&self, path: &str) -> Result<Vec<Value>, &'static str> {
        if self.files.contains_key(path) {
            return Err("path/not_folder/..");
        }
        let prefix = format!("{path}/");
        let mut seen = std::collections::BTreeMap::new();
        for (key, file) in &self.files {
            let Some(rest) = key.strip_prefix(&prefix) else {
                continue;
            };
            let entry = match rest.split_once('/') {
                Some((folder, _)) => json!({ ".tag": "folder", "name": folder }),
                None => json!({
                    ".tag": "file",
                    "name": rest,
                    "size": file.bytes.len(),
                    "server_modified": file.modified,
                }),
            };
            let name = entry["name"].as_str().unwrap().to_string();
            seen.insert(name, entry);
        }
        if seen.is_empty() && !path.is_empty() {
            return Err("path/not_found/..");
        }
        Ok(seen.into_values().collect())
    }

    /// One page from `start`, with the cursor for the next one.
    fn page_from(&mut self, path: &str, start: usize) -> Result<Value, &'static str> {
        self.list_calls += 1;
        let all = self.children(path)?;
        let size = if self.page == 0 { 100 } else { self.page };
        let end = (start + size).min(all.len());
        Ok(json!({
            "entries": all[start..end],
            "cursor": format!("{path}|{end}"),
            "has_more": end < all.len(),
        }))
    }

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
    if head.starts_with("POST /oauth2/token") {
        let form = String::from_utf8_lossy(&body).to_string();
        assert!(header("Authorization").is_none());
        let good = form.contains("grant_type=refresh_token")
            && form.contains("refresh_token=invented-refresh")
            && form.contains("client_id=invented-key")
            && form.contains("client_secret=invented-secret");
        return if good {
            let v = json!({ "access_token": GOOD_TOKEN, "expires_in": 14400 });
            reply(&mut stream, 200, v.to_string().as_bytes())
        } else {
            reply(&mut stream, 400, b"bad refresh")
        };
    }
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
        "/2/files/list_folder" => {
            assert_eq!(arg["recursive"], false);
            match s.page_from(&path, 0) {
                Ok(v) => reply(&mut stream, 200, v.to_string().as_bytes()),
                Err(summary) => error(&mut stream, summary),
            }
        }
        "/2/files/list_folder/continue" => {
            let cursor = arg["cursor"].as_str().unwrap_or("").to_string();
            let (folder, start) = cursor.rsplit_once('|').unwrap();
            match s.page_from(folder, start.parse().unwrap()) {
                Ok(v) => reply(&mut stream, 200, v.to_string().as_bytes()),
                Err(summary) => error(&mut stream, summary),
            }
        }
        "/2/files/get_metadata" => match s.files.get(&path) {
            Some(file) => {
                let v = json!({
                    ".tag": "file",
                    "name": path.rsplit('/').next().unwrap_or(""),
                    "size": file.bytes.len(),
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
                let stored = match (&arg["mode"], s.edit_instead.take()) {
                    (Value::Object(_), Some(edit)) => edit,
                    (_, edit) => {
                        s.edit_instead = edit;
                        body
                    }
                };
                s.put(&path, &stored);
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
            .env("ORGTHREAD_LOCK_DIR", self.dir.join("locks"))
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
    fn run_with_env(&self, args: &[&str], stdin: &[u8], env: &[(&str, &str)]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_orgthread"));
        c.args(args)
            .env("ORGTHREAD_STORAGE", "dropbox")
            .env("ORGTHREAD_DROPBOX_API", &self.url)
            .env("ORGTHREAD_DROPBOX_CONTENT", &self.url)
            .env("ORGTHREAD_LOCK_DIR", self.dir.join("locks"))
            .env("ORGTHREAD_LOG", self.dir.join("log"))
            .env("ORGTHREAD_READS", self.dir.join("reads"))
            .env("ORGTHREAD_VERSIONS", self.dir.join("versions"))
            .env("NO_PROXY", "127.0.0.1")
            .env("no_proxy", "127.0.0.1")
            .env("ORGTHREAD_DROPBOX_TOKEN", GOOD_TOKEN)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in env {
            c.env(name, value);
        }
        let mut child = c.spawn().unwrap();
        let _ = child.stdin.take().unwrap().write_all(stdin);
        child.wait_with_output().unwrap()
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
    fs::create_dir_all(f.dir.join("locks")).unwrap();
    let lock = f.dir.join("locks").join(LOCK);
    fs::write(&lock, "other writer\n").unwrap();
    let o = f.write("chat", b"new\n");
    assert_eq!(code(&o), 3, "{o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("other writer"));
    assert_eq!(f.get(FILE).unwrap(), b"old\n");
    assert!(lock.exists());

    // An old time makes the lock stale.
    let old = SystemTime::now() - Duration::from_secs(3_600);
    fs::File::options()
        .write(true)
        .open(&lock)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let o = f.write("chat", b"new\n");
    assert_eq!(code(&o), 0, "{o:?}");
    let said = String::from_utf8_lossy(&o.stderr).to_lowercase();
    assert!(said.contains("other writer"), "{said}");
    assert!(
        !said.contains("lock"),
        "a chat need not know about locks: {said}"
    );
    assert_eq!(f.names(), [FILE]);
    assert!(!lock.exists());
}

#[test]
fn a_lock_in_the_old_dropbox_folder_no_longer_counts() {
    let f = Fake::new();
    let s = Server::new(&f);
    s.dropbox_settings(&f);
    f.put(FILE, b"old\n");
    f.put(
        "/org/.locks/org--notes--invented.md.lock",
        b"a cloud task\n",
    );
    assert_eq!(code(&s.run(&["read", FILE, "--who", "chat"], b"")), 0);
    let o = s.run(&["write", FILE, "--who", "chat"], b"new\n");
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(f.get(FILE).unwrap(), b"new\n");
}

/// The program alone in a folder, as on a server, with an invented home and no settings.
struct Server {
    folder: PathBuf,
    home: PathBuf,
}

impl Server {
    fn new(f: &Fake) -> Self {
        let folder = f.dir.join("server");
        let home = f.dir.join("home");
        fs::create_dir_all(&folder).unwrap();
        fs::create_dir_all(&home).unwrap();
        let folder = fs::canonicalize(folder).unwrap();
        fs::copy(env!("CARGO_BIN_EXE_orgthread"), folder.join("orgthread")).unwrap();
        Server { folder, home }
    }
    fn settings(&self, text: &str) {
        fs::write(self.folder.join(".env"), text).unwrap();
    }
    fn dropbox_settings(&self, f: &Fake) {
        // A key from outside orgthread must not be taken: this would break curl.
        self.settings(&format!(
            "# invented\n\nORGTHREAD_STORAGE=dropbox\nORGTHREAD_DROPBOX_API={0}\nORGTHREAD_DROPBOX_CONTENT={0}\nORGTHREAD_DROPBOX_TOKEN={GOOD_TOKEN}\nPATH=/nowhere\n",
            f.url
        ));
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(self.folder.join("orgthread"));
        c.args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", &self.home)
            .env("NO_PROXY", "127.0.0.1")
            .env("no_proxy", "127.0.0.1")
            .current_dir(&self.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }
    fn run(&self, args: &[&str], text: &[u8]) -> Output {
        let mut child = self.command(args).spawn().unwrap();
        let _ = child.stdin.take().unwrap().write_all(text);
        child.wait_with_output().unwrap()
    }
}

#[test]
fn on_a_server_with_no_settings_the_message_says_what_to_set() {
    let f = Fake::new();
    let s = Server::new(&f);
    for args in [
        &["watch", "--once"][..],
        &["read", FILE, "--who", "c"][..],
        &["write", FILE, "--who", "c"][..],
    ] {
        let o = s.run(args, b"");
        assert_eq!(code(&o), 2, "{args:?} {o:?}");
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(err.contains("ORGTHREAD_ROOT"), "{err}");
        assert!(err.contains("ORGTHREAD_STORAGE=dropbox"), "{err}");
        assert!(
            err.contains(s.folder.join(".env").to_str().unwrap()),
            "{err}"
        );
    }
}

#[test]
fn settings_beside_the_program_let_every_command_work_there() {
    let f = Fake::new();
    let s = Server::new(&f);
    s.dropbox_settings(&f);

    let o = s.run(&["write", FILE, "--who", "phone"], b"secret marker\n");
    assert_eq!(code(&o), 0, "{o:?}");
    let o = s.run(&["read", FILE, "--who", "phone"], b"");
    assert_eq!(o.stdout, b"secret marker\n", "{o:?}");

    // The lock stayed off Dropbox, and all else lives beside the program.
    assert_eq!(f.names(), [FILE]);
    assert!(s.folder.join(".locks").is_dir());
    assert_eq!(fs::read_dir(s.folder.join(".locks")).unwrap().count(), 0);
    let log = fs::read_to_string(s.folder.join("state/writes.log")).unwrap();
    assert!(log.contains("org/notes/invented.md"), "{log}");
    assert!(s.folder.join("state/reads").is_dir());
    assert_eq!(fs::read_dir(&s.home).unwrap().count(), 0, "nothing at home");

    // The screen shows that write and a held lock.
    fs::write(s.folder.join(".locks").join(LOCK), "other writer\n").unwrap();
    let o = s.run(&["watch", "--once"], b"");
    assert_eq!(code(&o), 0, "{o:?}");
    let screen = String::from_utf8_lossy(&o.stdout);
    assert!(screen.contains("written"), "{screen}");
    assert!(screen.contains("org/notes/invented.md"), "{screen}");
    assert!(screen.contains("other writer"), "{screen}");
    assert!(!screen.contains("cannot read"), "{screen}");
    fs::remove_file(s.folder.join(".locks").join(LOCK)).unwrap();

    // No text stays in that folder.
    let mut todo = vec![s.folder.clone()];
    while let Some(dir) = todo.pop() {
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                todo.push(path);
            } else if path.file_name().unwrap() != "orgthread" {
                let bytes = fs::read(&path).unwrap();
                let found = bytes.windows(6).any(|w| w == b"secret");
                assert!(!found, "text found in {}", path.display());
            }
        }
    }
}

#[test]
fn a_rescue_stays_on_the_server_and_the_screen_shows_it() {
    let f = Fake::new();
    let s = Server::new(&f);
    s.dropbox_settings(&f);
    f.put(FILE, b"old\n");
    assert_eq!(code(&s.run(&["read", FILE, "--who", "chat"], b"")), 0);
    f.state.lock().unwrap().edit_instead = Some(b"a person's edit\n".to_vec());
    let o = s.run(&["write", FILE, "--who", "chat"], b"new\n");
    assert_eq!(code(&o), 7, "{o:?}");
    let said = String::from_utf8_lossy(&o.stderr).to_lowercase();
    assert!(
        !said.contains("lock"),
        "a chat need not know about locks: {said}"
    );
    assert!(said.contains("orgthread watch"), "{said}");
    assert_eq!(f.get(FILE).unwrap(), b"old\n");
    assert_eq!(f.names(), [FILE], "nothing extra in Dropbox");
    let kept: Vec<_> = fs::read_dir(s.folder.join(".locks"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(fs::read(&kept[0]).unwrap(), b"a person's edit\n");
    let o = s.run(&["watch", "--once"], b"");
    let screen = String::from_utf8_lossy(&o.stdout);
    assert!(screen.contains("left behind"), "{screen}");
}

#[test]
fn the_screen_on_a_server_needs_no_key_and_no_network() {
    let f = Fake::new();
    let s = Server::new(&f);
    s.settings("ORGTHREAD_STORAGE=dropbox\n");
    let o = s.run(&["watch", "--once"], b"");
    assert_eq!(code(&o), 0, "{o:?}");
    let screen = String::from_utf8_lossy(&o.stdout);
    assert!(screen.contains("No writes yet"), "{screen}");
    assert!(!screen.contains("cannot read"), "{screen}");
}

#[test]
fn the_environment_wins_over_the_settings_file() {
    let f = Fake::new();
    let s = Server::new(&f);
    s.dropbox_settings(&f);
    let elsewhere = f.dir.join("elsewhere.log");
    let mut child = s
        .command(&["write", FILE, "--who", "phone"])
        .env("ORGTHREAD_LOG", &elsewhere)
        .spawn()
        .unwrap();
    let _ = child.stdin.take().unwrap().write_all(b"text\n");
    let o = child.wait_with_output().unwrap();
    assert_eq!(code(&o), 0, "{o:?}");
    assert!(elsewhere.exists());
    assert!(!s.folder.join("state/writes.log").exists());
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

#[test]
fn a_refresh_token_gets_a_fresh_key_for_a_service() {
    let f = Fake::new();
    let run = |refresh: &str| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_orgthread"));
        c.args(["write", FILE, "--who", "service"])
            .env("ORGTHREAD_STORAGE", "dropbox")
            .env("ORGTHREAD_DROPBOX_API", &f.url)
            .env("ORGTHREAD_DROPBOX_CONTENT", &f.url)
            .env("ORGTHREAD_LOCK_DIR", f.dir.join("locks"))
            .env("ORGTHREAD_LOG", f.dir.join("log"))
            .env("ORGTHREAD_READS", f.dir.join("reads"))
            .env("NO_PROXY", "127.0.0.1")
            .env("no_proxy", "127.0.0.1")
            .env_remove("ORGTHREAD_DROPBOX_TOKEN")
            .env("ORGTHREAD_DROPBOX_REFRESH", refresh)
            .env("ORGTHREAD_DROPBOX_APP_KEY", "invented-key")
            .env("ORGTHREAD_DROPBOX_APP_SECRET", "invented-secret")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        let _ = child.stdin.take().unwrap().write_all(b"text\n");
        child.wait_with_output().unwrap()
    };
    let o = run("wrong-refresh");
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(f.get(FILE).is_none());
    let o = run("invented-refresh");
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(f.get(FILE).unwrap(), b"text\n");
}

#[test]
fn the_limits_hold_for_reads_and_writes() {
    let f = Fake::new();
    f.put("/org/notes/a.md", b"old\n");
    f.put("/org/health/log.md", b"private\n");
    f.put("/org/tasks.org", b"* TODO\n");
    f.put("/other/b.md", b"other\n");
    f.put("/organic/c.md", b"near\n");
    let run = |args: &[&str], text: &[u8]| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_orgthread"));
        c.args(args)
            .env("ORGTHREAD_STORAGE", "dropbox")
            .env("ORGTHREAD_DROPBOX_API", &f.url)
            .env("ORGTHREAD_DROPBOX_CONTENT", &f.url)
            .env("ORGTHREAD_DROPBOX_TOKEN", GOOD_TOKEN)
            .env("ORGTHREAD_LOCK_DIR", f.dir.join("locks"))
            .env("ORGTHREAD_LOG", f.dir.join("log"))
            .env("ORGTHREAD_READS", f.dir.join("reads"))
            .env("ORGTHREAD_ALLOW", "/org")
            .env("ORGTHREAD_DENY", "/org/health")
            .env("ORGTHREAD_READ_ONLY", "/org/tasks.org")
            .env("NO_PROXY", "127.0.0.1")
            .env("no_proxy", "127.0.0.1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        let _ = child.stdin.take().unwrap().write_all(text);
        child.wait_with_output().unwrap()
    };
    // Allowed.
    assert_eq!(
        code(&run(&["read", "/org/notes/a.md", "--who", "c"], b"")),
        0
    );
    assert_eq!(
        code(&run(&["write", "/org/notes/a.md", "--who", "c"], b"new\n")),
        0
    );
    assert_eq!(f.get("/org/notes/a.md").unwrap(), b"new\n");
    // Closed, with any capitals, for reading and writing.
    for path in ["/org/health/log.md", "/ORG/Health/log.md", "/org/health"] {
        let o = run(&["read", path, "--who", "c"], b"");
        assert_eq!(code(&o), 2, "{path} {o:?}");
        assert!(o.stdout.is_empty(), "{path}");
        assert_eq!(
            code(&run(&["write", path, "--who", "c"], b"x\n")),
            2,
            "{path}"
        );
    }
    assert_eq!(f.get("/org/health/log.md").unwrap(), b"private\n");
    // Outside, also a folder that only starts with the same letters.
    for path in ["/other/b.md", "/organic/c.md"] {
        assert_eq!(code(&run(&["read", path, "--who", "c"], b"")), 2, "{path}");
    }
    // Read only.
    assert_eq!(
        code(&run(&["read", "/org/tasks.org", "--who", "c"], b"")),
        0
    );
    let o = run(&["write", "/org/tasks.org", "--who", "c"], b"x\n");
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("read only"));
    assert_eq!(f.get("/org/tasks.org").unwrap(), b"* TODO\n");
}

#[test]
fn a_list_follows_the_cursor_over_several_pages() {
    let f = Fake::new();
    f.state.lock().unwrap().page = 2;
    f.put("/org/notes/a.md", b"four");
    f.put("/org/notes/b.md", b"12");
    f.put("/org/notes/.DS_Store", b"x");
    f.put("/org/notes/drafts/c.md", b"c");
    let o = f.run_with(Some(GOOD_TOKEN), &["list", "/org/notes"], b"");
    assert_eq!(code(&o), 0, "{o:?}");
    let text = String::from_utf8(o.stdout).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "/org/notes:");
    assert!(
        lines[1].ends_with("  a.md") && lines[1].contains("        4  2026-11-07"),
        "{text}"
    );
    assert!(lines[2].ends_with("  b.md"), "{text}");
    assert!(
        lines[3].ends_with("  drafts/") && lines[3].contains("  -  -  "),
        "{text}"
    );
    assert_eq!(lines.len(), 4, "{text}");
    assert_eq!(f.state.lock().unwrap().list_calls, 2, "two pages of two");
}

#[test]
fn a_list_of_the_root_a_file_and_a_missing_folder() {
    let f = Fake::new();
    f.put("/org/notes/a.md", b"four");
    let o = f.run(&["list", "/", "/org/notes/a.md", "/org/gone", "/org/"], b"");
    assert_eq!(code(&o), 6, "{o:?}");
    let text = String::from_utf8(o.stdout).unwrap();
    assert!(text.starts_with("/:\n"), "{text}");
    assert!(text.contains("  org/\n"), "{text}");
    assert!(
        text.contains("\n/org/notes:\n") && text.contains("  a.md\n"),
        "{text}"
    );
    assert!(
        text.contains("/org/gone:\n  there is no file or folder /org/gone\n"),
        "{text}"
    );
    assert!(
        text.contains("/org/:\n") && text.contains("  notes/\n"),
        "{text}"
    );
}

#[test]
fn a_list_in_dropbox_needs_a_clean_path() {
    let f = Fake::new();
    for folder in ["org/notes", "/org/../x", "/org//notes"] {
        let o = f.run(&["list", folder], b"");
        assert_eq!(code(&o), 2, "{folder}: {o:?}");
    }
}

/// Bytes that are not text: a NUL sign and no valid UTF-8.
fn picture(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i * 7 % 256) as u8).collect()
}

#[test]
fn a_picture_goes_up_and_comes_back_through_dropbox() {
    let f = Fake::new();
    let bytes = picture(5_000_000);
    let file = "/org/scans/p1.jpg";
    let up = f.run_with_env(
        &["upload", file, "--who", "a"],
        &bytes,
        &[("ORGTHREAD_BINARY_ALLOW", "org/scans")],
    );
    assert_eq!(code(&up), 0, "{}", String::from_utf8_lossy(&up.stderr));
    assert_eq!(f.get(file).unwrap(), bytes);
    let down = f.run(&["download", file, "--who", "b"], b"");
    assert_eq!(code(&down), 0, "{}", String::from_utf8_lossy(&down.stderr));
    assert_eq!(down.stdout, bytes);
    // The download counts as a read, so the same name may replace the file.
    let again = f.run_with_env(
        &["upload", file, "--who", "b"],
        &picture(10),
        &[("ORGTHREAD_BINARY_ALLOW", "org/scans")],
    );
    assert_eq!(
        code(&again),
        0,
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    assert_eq!(f.get(file).unwrap(), picture(10));
}

#[test]
fn a_download_from_dropbox_knows_the_size_first() {
    let f = Fake::new();
    f.put("/org/scans/big.pdf", &picture(2_000_000));
    let check = f.run(
        &["download", "--check", "/org/scans/big.pdf", "--who", "a"],
        b"",
    );
    assert_eq!(code(&check), 0);
    assert_eq!(String::from_utf8_lossy(&check.stdout), "2000000\n");
    let over = f.run_with_env(
        &["download", "/org/scans/big.pdf", "--who", "a"],
        b"",
        &[("ORGTHREAD_DOWNLOAD_MB", "1")],
    );
    assert_eq!(code(&over), 2);
    assert!(over.stdout.is_empty());
    assert_eq!(
        code(&f.run(&["download", "/org/scans/none.pdf", "--who", "a"], b"")),
        6
    );
}
