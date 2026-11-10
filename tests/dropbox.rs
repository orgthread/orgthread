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
    /// How many downloads came in.
    downloads: u32,
    /// How many `get_metadata` calls came in.
    metadata_calls: u32,
    /// How many uploads came in.
    uploads: u32,
    /// The `parent_rev` of each `delete_v2` call, or `None` for a call with none.
    deletes: Vec<Option<String>>,
    /// The argument of each `copy_v2` call.
    copies: Vec<Value>,
    /// The next copy hangs up with no answer: after it made the copy, or before.
    copy_hangs_up: Option<bool>,
    /// The next copy stores these bytes in place of the source, as a broken copy would.
    copy_stores: Option<Vec<u8>>,
    /// The next `delete_v2` fails with a server error.
    delete_fails: bool,
    /// The argument of each `move_v2` call.
    moves: Vec<Value>,
    /// The next move hangs up with no answer: after it moved the file, or before.
    move_hangs_up: Option<bool>,
    /// The next move finds these bytes in the file, as if a write landed in between.
    move_finds: Option<Vec<u8>>,
    /// How many `download_zip` calls came in.
    zips: u32,
    /// The next `download_zip` answers that the folder is too large to zip.
    zip_too_large: bool,
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
                    "content_hash": fingerprint(&file.bytes),
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

    /// Every entry under a folder, at any depth, as a recursive `list_folder`
    /// gives it: the folder itself first, then each folder and file by path.
    fn deep_children(&self, path: &str) -> Result<Vec<Value>, &'static str> {
        self.children(path)?;
        let prefix = format!("{path}/");
        let mut seen = std::collections::BTreeMap::new();
        for (key, file) in &self.files {
            let Some(rest) = key.strip_prefix(&prefix) else {
                continue;
            };
            let parts: Vec<&str> = rest.split('/').collect();
            for i in 1..parts.len() {
                let folder = format!("{prefix}{}", parts[..i].join("/"));
                let v = json!({ ".tag": "folder", "name": parts[i - 1], "path_display": folder });
                seen.insert(folder, v);
            }
            let v = json!({
                ".tag": "file",
                "name": parts[parts.len() - 1],
                "path_display": key,
                "size": file.bytes.len(),
                "content_hash": fingerprint(&file.bytes),
            });
            seen.insert(key.clone(), v);
        }
        let top =
            json!({ ".tag": "folder", "name": path.rsplit('/').next(), "path_display": path });
        Ok(std::iter::once(top).chain(seen.into_values()).collect())
    }

    /// One page of a recursive list from `start`, with its cursor.
    fn deep_page_from(&mut self, path: &str, start: usize) -> Result<Value, &'static str> {
        self.list_calls += 1;
        let all = self.deep_children(path)?;
        let size = if self.page == 0 { 100 } else { self.page };
        let end = (start + size).min(all.len());
        Ok(json!({
            "entries": all[start..end],
            "cursor": format!("deep:{path}|{end}"),
            "has_more": end < all.len(),
        }))
    }

    /// The zip of `download_zip`: the folder itself at the top, then its files,
    /// stored with no packing, as a test needs only the names and the bytes.
    fn zip_of(&self, path: &str) -> Result<Vec<u8>, &'static str> {
        use zip::write::SimpleFileOptions;
        self.children(path)?;
        let top = path.rsplit('/').next().unwrap_or("");
        let prefix = format!("{path}/");
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        let mut out = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        out.add_directory(format!("{top}/"), options).unwrap();
        let mut names: Vec<&String> = self.files.keys().collect();
        names.sort();
        for key in names {
            if let Some(rest) = key.strip_prefix(&prefix) {
                out.start_file(format!("{top}/{rest}"), options).unwrap();
                out.write_all(&self.files[key].bytes).unwrap();
            }
        }
        Ok(out.finish().unwrap().into_inner())
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

fn reply_with_result(stream: &mut TcpStream, body: &[u8], result: &Value) {
    let head = format!(
        "HTTP/1.1 200 X\r\nContent-Length: {}\r\nDropbox-API-Result: {result}\r\nConnection: close\r\n\r\n",
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
    if endpoint == "/2/files/download" {
        s.downloads += 1;
    }
    if endpoint == "/2/files/get_metadata" {
        s.metadata_calls += 1;
    }
    if endpoint == "/2/files/upload" {
        s.uploads += 1;
    }
    match endpoint.as_str() {
        "/2/files/download" => match s.files.get(&path) {
            Some(file) => {
                let result = json!({
                    "rev": file.rev.to_string(),
                    "content_hash": fingerprint(&file.bytes),
                });
                reply_with_result(&mut stream, &file.bytes, &result)
            }
            None => error(&mut stream, "path/not_found/.."),
        },
        "/2/files/list_folder" => {
            let page = if arg["recursive"] == true {
                s.deep_page_from(&path, 0)
            } else {
                assert_eq!(arg["recursive"], false);
                s.page_from(&path, 0)
            };
            match page {
                Ok(v) => reply(&mut stream, 200, v.to_string().as_bytes()),
                Err(summary) => error(&mut stream, summary),
            }
        }
        "/2/files/download_zip" => {
            s.zips += 1;
            if std::mem::take(&mut s.zip_too_large) {
                return error(&mut stream, "too_large/..");
            }
            match s.zip_of(&path) {
                Ok(zip) => reply(&mut stream, 200, &zip),
                Err(summary) => error(&mut stream, summary),
            }
        }
        "/2/files/list_folder/continue" => {
            let cursor = arg["cursor"].as_str().unwrap_or("").to_string();
            let (folder, start) = cursor.rsplit_once('|').unwrap();
            let start = start.parse().unwrap();
            if let Some(folder) = folder.strip_prefix("deep:") {
                return match s.deep_page_from(folder, start) {
                    Ok(v) => reply(&mut stream, 200, v.to_string().as_bytes()),
                    Err(summary) => error(&mut stream, summary),
                };
            }
            match s.page_from(folder, start) {
                Ok(v) => reply(&mut stream, 200, v.to_string().as_bytes()),
                Err(summary) => error(&mut stream, summary),
            }
        }
        "/2/files/get_metadata" if !s.files.contains_key(&path) && s.children(&path).is_ok() => {
            let v = json!({ ".tag": "folder", "name": path.rsplit('/').next().unwrap_or("") });
            reply(&mut stream, 200, v.to_string().as_bytes());
        }
        "/2/files/copy_v2" => {
            s.copies.push(arg.clone());
            assert_eq!(arg["autorename"], false);
            let from = arg["from_path"].as_str().unwrap_or("").to_string();
            let to = arg["to_path"].as_str().unwrap_or("").to_string();
            let folder_at = |p: &str| s.children(p).is_ok() && !s.files.contains_key(p);
            if s.files.contains_key(&to) || folder_at(&to) {
                return error(&mut stream, "to/conflict/file/..");
            }
            let Some(bytes) = s.files.get(&from).map(|f| f.bytes.clone()) else {
                return error(&mut stream, "from_lookup/not_found/..");
            };
            let hang_up = s.copy_hangs_up.take();
            if hang_up == Some(false) {
                return;
            }
            let bytes = s.copy_stores.take().unwrap_or(bytes);
            s.put(&to, &bytes);
            if hang_up == Some(true) {
                return;
            }
            let file = &s.files[&to];
            let v = json!({ "metadata": {
                ".tag": "file",
                "name": to.rsplit('/').next().unwrap_or(""),
                "size": file.bytes.len(),
                "rev": file.rev.to_string(),
                "content_hash": fingerprint(&file.bytes),
            }});
            reply(&mut stream, 200, v.to_string().as_bytes());
        }
        "/2/files/move_v2" => {
            s.moves.push(arg.clone());
            assert_eq!(arg["autorename"], false);
            assert_eq!(arg["allow_ownership_transfer"], false);
            let from = arg["from_path"].as_str().unwrap_or("").to_string();
            let to = arg["to_path"].as_str().unwrap_or("").to_string();
            let folder_at = |p: &str| s.children(p).is_ok() && !s.files.contains_key(p);
            if s.files.contains_key(&to) || folder_at(&to) {
                return error(&mut stream, "to/conflict/file/..");
            }
            if folder_at(&from) {
                return error(&mut stream, "from_lookup/not_file/..");
            }
            let Some(mut bytes) = s.files.get(&from).map(|f| f.bytes.clone()) else {
                return error(&mut stream, "from_lookup/not_found/..");
            };
            let hang_up = s.move_hangs_up.take();
            if hang_up == Some(false) {
                return;
            }
            if let Some(found) = s.move_finds.take() {
                bytes = found;
            }
            s.files.remove(&from);
            s.put(&to, &bytes);
            if hang_up == Some(true) {
                return;
            }
            let file = &s.files[&to];
            let v = json!({ "metadata": {
                ".tag": "file",
                "name": to.rsplit('/').next().unwrap_or(""),
                "size": file.bytes.len(),
                "rev": file.rev.to_string(),
                "content_hash": fingerprint(&file.bytes),
            }});
            reply(&mut stream, 200, v.to_string().as_bytes());
        }
        "/2/files/delete_v2" if std::mem::take(&mut s.delete_fails) => {
            reply(&mut stream, 500, b"server error");
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
        "/2/files/delete_v2" => {
            s.deletes
                .push(arg["parent_rev"].as_str().map(str::to_string));
            match s.files.get(&path).map(|f| f.rev.to_string()) {
                None => error(&mut stream, "path_lookup/not_found/.."),
                Some(rev) if arg["parent_rev"].as_str().is_some_and(|want| want != rev) => {
                    error(&mut stream, "path_write/conflict/file/..");
                }
                Some(rev) => {
                    let file = s.files.remove(&path).unwrap();
                    let v = json!({ "metadata": {
                        ".tag": "file",
                        "name": path.rsplit('/').next().unwrap_or(""),
                        "size": file.bytes.len(),
                        "rev": rev,
                        "content_hash": fingerprint(&file.bytes),
                    }});
                    reply(&mut stream, 200, v.to_string().as_bytes());
                }
            }
        }
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
    let first = screen.lines().next().unwrap();
    assert!(first.starts_with("orgthread "), "{first}");
    assert!(first.contains(" · Dropbox"), "{first}");
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
    for path in [
        "/org/../a.md",
        "org/../a.md",
        "/",
        "",
        "/org/",
        "//org/a.md",
    ] {
        let o = f.run(&["write", path, "--who", "c"], b"x");
        assert_eq!(code(&o), 2, "{path} {o:?}");
    }
    assert!(f.names().is_empty());
}

/// The log and the screen show a path with no `/` at its start, so a path
/// copied from there works for every command, as the one with the `/` does.
#[test]
fn a_path_from_the_log_works_with_or_without_the_first_slash() {
    let f = Fake::new();
    let bare = "org/notes/invented.md";
    let o = f.run(&["write", bare, "--who", "a"], b"one\n");
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        f.get(FILE).unwrap(),
        b"one\n",
        "the same file as with the /"
    );
    let o = f.run(&["read", bare, "--who", "a"], b"");
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(o.stdout, b"one\n");
    // A read without the / counts for a write with it, and the other way round.
    let o = f.run(&["write", FILE, "--who", "a"], b"two\n");
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    let patch = br#"{"edits": [{"old": "two\n", "new": "three\n"}]}"#;
    let o = f.run(&["patch", bare, "--who", "a"], patch);
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(f.get(FILE).unwrap(), b"three\n");
    let o = f.run(&["download", bare, "--who", "b"], b"");
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(o.stdout, b"three\n");
    let o = f.run(&["upload", bare, "--who", "b"], b"four\n");
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(f.get(FILE).unwrap(), b"four\n");
    let bare_list = f.run(&["list", "org/notes"], b"");
    let list = f.run(&["list", "/org/notes"], b"");
    assert_eq!(
        code(&bare_list),
        0,
        "{}",
        String::from_utf8_lossy(&bare_list.stderr)
    );
    // The same entries; only the heading repeats the name as it was given.
    let body = |o: &std::process::Output| {
        let text = String::from_utf8_lossy(&o.stdout).to_string();
        text.split_once('\n').map(|(_, rest)| rest.to_string())
    };
    assert!(String::from_utf8_lossy(&list.stdout).contains("invented.md"));
    assert_eq!(body(&bare_list), body(&list));
    assert_eq!(f.names(), [FILE], "one file, no copy beside it");
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
fn a_list_gives_the_fingerprints_of_dropbox_with_no_download() {
    let f = Fake::new();
    f.put("/org/notes/a.md", b"four");
    f.put("/org/notes/drafts/c.md", b"c");
    let o = f.run(
        &[
            "list",
            "--fingerprint",
            "/org/notes",
            "/org/notes/drafts/c.md",
        ],
        b"",
    );
    assert_eq!(code(&o), 0, "{o:?}");
    let text = String::from_utf8(o.stdout).unwrap();
    let a = format!("  {}  a.md\n", fingerprint(b"four"));
    let c = format!("  {}  c.md\n", fingerprint(b"c"));
    assert!(text.contains(&a), "{text}");
    assert!(text.contains(&c), "{text}");
    assert!(
        text.contains(&format!("  {:<64}  drafts/\n", "-")),
        "{text}"
    );
    assert_eq!(f.state.lock().unwrap().downloads, 0, "{text}");
}

#[test]
fn a_list_in_dropbox_needs_a_clean_path() {
    let f = Fake::new();
    for folder in ["", "/org/../x", "org/../x", "/org//notes"] {
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

const ALLOW: (&str, &str) = ("ORGTHREAD_DELETE_ALLOW", "/org/notes");

fn deleted_fields(f: &Fake) -> Vec<String> {
    let log = fs::read_to_string(f.dir.join("log")).unwrap();
    let line = log.lines().last().unwrap();
    line.split('\t').skip(1).map(str::to_string).collect()
}

#[test]
fn read_then_delete_is_one_trip_with_the_rev_of_the_read() {
    let f = Fake::new();
    f.put(FILE, b"old\n");
    f.read("chat");
    let rev = f.state.lock().unwrap().files[FILE].rev.to_string();
    let before = f.state.lock().unwrap().metadata_calls;
    let o = f.run_with_env(&["delete", FILE, "--who", "chat"], b"", &[ALLOW]);
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        format!(
            "deleted: org/notes/invented.md, 4 bytes. Dropbox keeps it in Deleted files, rev {rev}.\n"
        )
    );
    assert_eq!(f.get(FILE), None);
    let s = f.state.lock().unwrap();
    assert_eq!(s.deletes, [Some(rev.clone())], "one call, with parent_rev");
    assert_eq!(s.metadata_calls, before, "no get_metadata first");
    drop(s);
    let print = fingerprint(b"old\n");
    assert_eq!(
        deleted_fields(&f),
        [
            "deleted",
            "org/notes/invented.md",
            "chat",
            &print,
            "-",
            "4",
            &rev
        ]
    );
    assert!(
        !f.dir.join("versions").exists(),
        "no text on the disk of a server"
    );
}

#[test]
fn a_newer_version_in_dropbox_is_a_409_conflict_and_stays() {
    let f = Fake::new();
    f.put(FILE, b"old\n");
    f.read("chat");
    // The same bytes again: a new rev. Only Dropbox's own check can see this.
    f.put(FILE, b"old\n");
    let o = f.run_with_env(&["delete", FILE, "--who", "chat"], b"", &[ALLOW]);
    assert_eq!(code(&o), 4, "{o:?}");
    assert_eq!(f.get(FILE).unwrap(), b"old\n");
    assert_eq!(f.state.lock().unwrap().deletes.len(), 1);
}

#[test]
fn a_file_gone_in_dropbox_is_not_found_and_exit_6() {
    let f = Fake::new();
    f.put(FILE, b"old\n");
    f.read("chat");
    f.state.lock().unwrap().files.clear();
    let o = f.run_with_env(&["delete", FILE, "--who", "chat"], b"", &[ALLOW]);
    assert_eq!(code(&o), 6, "{o:?}");
}

#[test]
fn expect_with_no_noted_rev_looks_up_the_rev_first() {
    let f = Fake::new();
    f.put(FILE, b"old\n");
    let print = fingerprint(b"old\n");
    let args = ["delete", FILE, "--who", "x", "--expect", &print];
    let wrong = fingerprint(b"other\n");
    let o = f.run_with_env(
        &["delete", FILE, "--who", "x", "--expect", &wrong],
        b"",
        &[ALLOW],
    );
    assert_eq!(code(&o), 4, "{o:?}");
    assert!(f.state.lock().unwrap().deletes.is_empty(), "no delete call");
    let o = f.run_with_env(&args, b"", &[ALLOW]);
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(f.get(FILE), None);
    let rev = f.state.lock().unwrap().deletes.clone();
    assert_eq!(rev.len(), 1);
    assert!(rev[0].is_some(), "the rev from get_metadata went with it");
}

#[test]
fn a_delete_in_dropbox_needs_the_setting_and_a_fingerprint() {
    let f = Fake::new();
    f.put(FILE, b"old\n");
    f.read("chat");
    let o = f.run(&["delete", FILE, "--who", "chat"], b"");
    assert_eq!(code(&o), 2, "{o:?}");
    let o = f.run_with_env(&["delete", FILE, "--who", "other"], b"", &[ALLOW]);
    assert_eq!(code(&o), 4, "{o:?}");
    let o = f.run_with_env(&["delete", FILE, "--who", "chat", "--check"], b"", &[ALLOW]);
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(f.get(FILE).unwrap(), b"old\n");
    assert!(f.state.lock().unwrap().deletes.is_empty());
}

#[test]
fn a_delete_works_with_or_without_the_first_slash() {
    let f = Fake::new();
    let bare = FILE.trim_start_matches('/');
    // Read with the slash, delete in the form of the log: the same note, one trip.
    f.put(FILE, b"old\n");
    assert_eq!(code(&f.read("chat")), 0);
    let rev = f.state.lock().unwrap().files[FILE].rev.to_string();
    let o = f.run_with_env(&["delete", bare, "--who", "chat"], b"", &[ALLOW]);
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(f.get(FILE), None);
    assert_eq!(f.state.lock().unwrap().deletes, [Some(rev)]);
    // The other way round, with an allow folder written with no slash.
    f.put(FILE, b"new\n");
    assert_eq!(code(&f.run(&["read", bare, "--who", "chat"], b"")), 0);
    let allow = ("ORGTHREAD_DELETE_ALLOW", "org/notes");
    let o = f.run_with_env(&["delete", FILE, "--who", "chat", "--check"], b"", &[allow]);
    assert_eq!(code(&o), 0, "{o:?}");
    let o = f.run_with_env(&["delete", FILE, "--who", "chat"], b"", &[allow]);
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(f.get(FILE), None);
    assert_eq!(f.state.lock().unwrap().deletes.len(), 2);
    let log = fs::read_to_string(f.dir.join("log")).unwrap();
    let paths: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("\tdeleted\t"))
        .map(|l| l.split('\t').nth(2).unwrap())
        .collect();
    assert_eq!(paths, [bare, bare], "the log names the file one way");
}

fn copy(f: &Fake, from: &str, to: &str) -> Output {
    f.run(&["copy", from, to, "--who", "chat"], b"")
}

fn last_log(f: &Fake) -> Vec<String> {
    let log = fs::read_to_string(f.dir.join("log")).unwrap_or_default();
    let line = log.lines().last().expect("no log line").to_string();
    line.split('\t').map(str::to_string).collect()
}

#[test]
fn a_dropbox_copy_is_two_calls_and_no_byte_passes_the_server() {
    let f = Fake::new();
    f.put(FILE, "Grüße\n".as_bytes());
    let o = copy(&f, FILE, "/org/new/deep/b.md");
    assert_eq!(code(&o), 0, "{o:?}");
    let print = fingerprint("Grüße\n".as_bytes());
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        format!(
            "copied: org/notes/invented.md -> org/new/deep/b.md, 8 bytes, {print}, new folders: org/new, org/new/deep\n"
        )
    );
    assert_eq!(f.get("/org/new/deep/b.md").unwrap(), "Grüße\n".as_bytes());
    let s = f.state.lock().unwrap();
    assert_eq!(
        s.metadata_calls, 4,
        "one look at the source, and one at each folder up to the first that exists"
    );
    assert_eq!(s.copies.len(), 1);
    assert_eq!(s.downloads, 0, "no byte passes this server");
    assert_eq!(
        s.copies[0],
        json!({"from_path": FILE, "to_path": "/org/new/deep/b.md", "autorename": false})
    );
    drop(s);
    assert_eq!(
        last_log(&f)[1..],
        [
            "copied",
            "org/new/deep/b.md",
            "chat",
            "-",
            print.as_str(),
            "8",
            "/org/notes/invented.md"
        ]
    );
    assert!(fs::read_dir(f.dir.join("locks")).unwrap().next().is_none());
}

#[test]
fn a_dropbox_target_that_exists_stays_and_gets_exit_5() {
    let f = Fake::new();
    f.put(FILE, b"new\n");
    f.put("/org/notes/b.md", b"old\n");
    let o = copy(&f, FILE, "org/notes/b.md");
    assert_eq!(code(&o), 5, "{o:?}");
    assert_eq!(f.get("/org/notes/b.md").unwrap(), b"old\n");
    assert_eq!(last_log(&f)[1], "exists");
    // A folder at the new name, too.
    let o = copy(&f, FILE, "/org/notes");
    assert_eq!(code(&o), 5, "{o:?}");
    assert!(
        f.state.lock().unwrap().deletes.is_empty(),
        "nothing removed"
    );
}

#[test]
fn in_dropbox_no_source_is_6_a_folder_2_and_another_version_4() {
    let f = Fake::new();
    let o = copy(&f, "/org/notes/gone.md", "/org/b.md");
    assert_eq!(code(&o), 6, "{o:?}");
    f.put(FILE, b"edited\n");
    let o = copy(&f, "/org/notes", "/org/b.md");
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("is a folder"));
    let seen = fingerprint(b"seen\n");
    let o = f.run(
        &[
            "copy",
            FILE,
            "/org/b.md",
            "--who",
            "chat",
            "--fingerprint",
            &seen,
        ],
        b"",
    );
    assert_eq!(code(&o), 4, "{o:?}");
    assert_eq!(
        f.state.lock().unwrap().copies.len(),
        0,
        "no copy_v2 for any of them"
    );
    assert_eq!(f.get("/org/b.md"), None);
    let right = fingerprint(b"edited\n");
    let o = f.run(
        &[
            "copy",
            FILE,
            "/org/b.md",
            "--who",
            "chat",
            "--fingerprint",
            &right,
        ],
        b"",
    );
    assert_eq!(code(&o), 0, "{o:?}");
}

#[test]
fn a_dropbox_copy_with_other_bytes_is_removed_by_its_rev_and_gives_7() {
    let f = Fake::new();
    f.put(FILE, b"source\n");
    f.state.lock().unwrap().copy_stores = Some(b"broken".to_vec());
    let o = copy(&f, FILE, "/org/b.md");
    assert_eq!(code(&o), 7, "{o:?}");
    assert_eq!(f.get("/org/b.md"), None, "the broken copy is gone");
    assert_eq!(f.get(FILE).unwrap(), b"source\n");
    let s = f.state.lock().unwrap();
    assert_eq!(s.deletes.len(), 1);
    assert!(
        s.deletes[0].is_some(),
        "the delete names the rev of the copy"
    );
    drop(s);
    let line = last_log(&f);
    assert_eq!(line[1], "removed");
    assert_eq!(line[4], fingerprint(b"source\n"));
    assert_eq!(line[5], fingerprint(b"broken"));
}

#[test]
fn a_dropbox_copy_that_cannot_be_removed_gives_9() {
    let f = Fake::new();
    f.put(FILE, b"source\n");
    {
        let mut s = f.state.lock().unwrap();
        s.copy_stores = Some(b"broken".to_vec());
        s.delete_fails = true;
    }
    let o = copy(&f, FILE, "/org/b.md");
    assert_eq!(code(&o), 9, "{o:?}");
    let said = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(said.contains("DAMAGE POSSIBLE"), "{said}");
    assert_eq!(f.get("/org/b.md").unwrap(), b"broken");
    assert_eq!(last_log(&f)[1], "not-removed");
}

#[test]
fn with_no_answer_one_look_at_the_target_tells() {
    // The copy was made: done, exit 0, with one more get_metadata.
    let f = Fake::new();
    f.put(FILE, b"source\n");
    f.state.lock().unwrap().copy_hangs_up = Some(true);
    let o = copy(&f, FILE, "/org/b.md");
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(
        f.state.lock().unwrap().metadata_calls,
        3,
        "the source, the folder, and the target after no answer"
    );
    assert_eq!(last_log(&f)[1], "copied");
    // The copy was not made: exit 1, and the task can try again.
    f.state.lock().unwrap().copy_hangs_up = Some(false);
    let o = copy(&f, FILE, "/org/c.md");
    assert_eq!(code(&o), 1, "{o:?}");
    let said = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(
        said.contains("is not there, so nothing was copied. Try again"),
        "{said}"
    );
    assert_eq!(f.get("/org/c.md"), None);
    assert_eq!(
        code(&copy(&f, FILE, "/org/c.md")),
        0,
        "the second try works"
    );
    // Other bytes at the target after no answer are never removed.
    {
        let mut s = f.state.lock().unwrap();
        s.copy_hangs_up = Some(true);
        s.copy_stores = Some(b"other".to_vec());
    }
    let o = copy(&f, FILE, "/org/d.md");
    assert_eq!(code(&o), 1, "{o:?}");
    assert_eq!(f.get("/org/d.md").unwrap(), b"other");
    assert!(f.state.lock().unwrap().deletes.is_empty());
}

#[test]
fn on_a_server_the_name_decides_if_a_file_is_not_text() {
    let f = Fake::new();
    f.put("/org/notes/photo.jpg", &[0x89, b'P', b'N', b'G', 0]);
    let o = copy(&f, "/org/notes/photo.jpg", "/org/notes/b.jpg");
    assert_eq!(code(&o), 2, "{o:?}");
    assert_eq!(last_log(&f)[1], "not-text");
    let open = [("ORGTHREAD_BINARY_ALLOW", "org/pictures")];
    let o = f.run_with_env(
        &[
            "copy",
            "/org/notes/photo.jpg",
            "/org/pictures/b.jpg",
            "--who",
            "chat",
        ],
        b"",
        &open,
    );
    assert_eq!(code(&o), 0, "{o:?}");
    // The limit that the help names: with no extension, the name tells nothing.
    f.put("/org/notes/photo", &[0x89, b'P', b'N', b'G', 0]);
    assert_eq!(code(&copy(&f, "/org/notes/photo", "/org/notes/copy")), 0);
    assert_eq!(f.state.lock().unwrap().downloads, 0);
}

#[test]
fn the_limits_hold_for_both_ends_of_a_dropbox_copy() {
    let f = Fake::new();
    f.put(FILE, b"text\n");
    f.put("/org/closed/c.md", b"text\n");
    let deny = [("ORGTHREAD_DENY", "org/closed")];
    let o = f.run_with_env(
        &["copy", "/org/closed/c.md", "/org/b.md", "--who", "x"],
        b"",
        &deny,
    );
    assert_eq!(code(&o), 2, "{o:?}");
    let ro = [("ORGTHREAD_READ_ONLY", "org/closed")];
    let o = f.run_with_env(&["copy", FILE, "/org/closed/d.md", "--who", "x"], b"", &ro);
    assert_eq!(code(&o), 2, "{o:?}");
    assert_eq!(f.state.lock().unwrap().copies.len(), 0);
    assert_eq!(last_log(&f)[1], "not-allowed");
}

#[test]
fn a_copy_and_a_write_of_one_target_share_one_lock() {
    let f = Fake::new();
    f.put(FILE, b"text\n");
    let locks = f.dir.join("locks");
    fs::create_dir_all(&locks).unwrap();
    fs::write(locks.join("org--b.md.lock"), "a writer\n").unwrap();
    let o = copy(&f, FILE, "/org/b.md");
    assert_eq!(code(&o), 3, "{o:?}");
    assert_eq!(f.state.lock().unwrap().copies.len(), 0);
}

fn mv(f: &Fake, from: &str, to: &str) -> Output {
    f.run(&["move", from, to, "--who", "chat"], b"")
}

fn mv_print(f: &Fake, from: &str, to: &str, print: &str) -> Output {
    f.run(
        &["move", from, to, "--who", "chat", "--fingerprint", print],
        b"",
    )
}

#[test]
fn a_dropbox_move_is_one_move_call_that_never_renames_and_names_the_new_folders() {
    let f = Fake::new();
    f.put(FILE, "Grüße\n".as_bytes());
    let o = mv(&f, FILE, "/org/new/deep/b.md");
    assert_eq!(code(&o), 0, "{o:?}");
    let print = fingerprint("Grüße\n".as_bytes());
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        format!(
            "moved: org/notes/invented.md -> org/new/deep/b.md, 8 bytes, {print}, new folders: org/new, org/new/deep\nnote: the file name changed, so wikilinks to [[invented]] break. Fix them to the new name.\n"
        )
    );
    assert_eq!(f.get("/org/new/deep/b.md").unwrap(), "Grüße\n".as_bytes());
    assert_eq!(f.get(FILE), None, "the old name is gone");
    let s = f.state.lock().unwrap();
    assert_eq!(
        s.metadata_calls, 4,
        "one look at the file, and one at each folder up to the first that exists"
    );
    assert_eq!(s.downloads, 0, "no byte passes this server");
    assert_eq!(
        s.moves,
        [json!({
            "from_path": FILE,
            "to_path": "/org/new/deep/b.md",
            "autorename": false,
            "allow_ownership_transfer": false,
        })]
    );
    drop(s);
    assert_eq!(
        last_log(&f)[1..],
        [
            "moved",
            "org/new/deep/b.md",
            "chat",
            "-",
            print.as_str(),
            "8",
            "/org/notes/invented.md"
        ]
    );
    assert!(fs::read_dir(f.dir.join("locks")).unwrap().next().is_none());
    // The way back is a move back, with or without the first slash.
    assert_eq!(code(&mv(&f, "org/new/deep/b.md", FILE)), 0);
    assert_eq!(f.get(FILE).unwrap(), "Grüße\n".as_bytes());
}

#[test]
fn a_dropbox_new_name_that_exists_stays_and_gets_exit_5() {
    let f = Fake::new();
    f.put(FILE, b"new\n");
    f.put("/org/notes/b.md", b"old\n");
    let o = mv(&f, FILE, "org/notes/b.md");
    assert_eq!(code(&o), 5, "{o:?}");
    assert_eq!(f.get("/org/notes/b.md").unwrap(), b"old\n");
    assert_eq!(f.get(FILE).unwrap(), b"new\n");
    assert_eq!(last_log(&f)[1], "move-exists");
    let o = mv(&f, FILE, "/org/notes");
    assert_eq!(code(&o), 5, "{o:?}");
    assert_eq!(f.get(FILE).unwrap(), b"new\n");
}

#[test]
fn in_dropbox_no_file_is_6_a_folder_2_and_another_version_4_with_no_move_call() {
    let f = Fake::new();
    let o = mv(&f, "/org/notes/gone.md", "/org/b.md");
    assert_eq!(code(&o), 6, "{o:?}");
    assert_eq!(last_log(&f)[1], "move-missing");
    f.put(FILE, b"edited\n");
    let o = mv(&f, "/org/notes", "/org/b.md");
    assert_eq!(code(&o), 2, "{o:?}");
    assert_eq!(last_log(&f)[1], "move-folder");
    let o = mv_print(&f, FILE, "/org/b.md", &fingerprint(b"seen\n"));
    assert_eq!(code(&o), 4, "{o:?}");
    assert_eq!(last_log(&f)[1], "move-changed");
    assert!(
        f.state.lock().unwrap().moves.is_empty(),
        "no move_v2 for any of them"
    );
    assert_eq!(f.get(FILE).unwrap(), b"edited\n");
    let o = mv_print(&f, FILE, "/org/b.md", &fingerprint(b"edited\n"));
    assert_eq!(code(&o), 0, "{o:?}");
}

#[test]
fn a_file_that_changed_during_a_dropbox_move_goes_back_with_exit_7() {
    let f = Fake::new();
    f.put(FILE, b"seen\n");
    f.state.lock().unwrap().move_finds = Some(b"a later edit\n".to_vec());
    let o = mv_print(&f, FILE, "/org/b.md", &fingerprint(b"seen\n"));
    assert_eq!(code(&o), 7, "{o:?}");
    assert_eq!(f.get(FILE).unwrap(), b"a later edit\n", "moved back");
    assert_eq!(f.get("/org/b.md"), None);
    let line = last_log(&f);
    assert_eq!(line[1], "moved-back");
    assert_eq!(line[5], fingerprint(b"a later edit\n"));
    let moves = f.state.lock().unwrap().moves.clone();
    assert_eq!(moves.len(), 2);
    assert_eq!(moves[1]["from_path"], "/org/b.md");
    assert_eq!(moves[1]["to_path"], FILE);
}

#[test]
fn with_no_answer_to_a_move_one_look_at_each_name_tells() {
    let f = Fake::new();
    f.put(FILE, b"text\n");
    // The file moved: done, exit 0.
    f.state.lock().unwrap().move_hangs_up = Some(true);
    let o = mv(&f, FILE, "/org/b.md");
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(last_log(&f)[1], "moved");
    // The file did not move: exit 1, and the task can try again.
    f.state.lock().unwrap().move_hangs_up = Some(false);
    let o = mv(&f, "/org/b.md", "/org/c.md");
    assert_eq!(code(&o), 1, "{o:?}");
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("nothing moved. Try again"),
        "{o:?}"
    );
    assert_eq!(code(&mv(&f, "/org/b.md", "/org/c.md")), 0);
}

#[test]
fn the_setting_of_moves_holds_for_both_dropbox_names() {
    let f = Fake::new();
    f.put("/org/health/diet/d.md", b"diet\n");
    f.put("/org/notes/n.md", b"note\n");
    let allow = [(
        "ORGTHREAD_MOVE_ALLOW",
        "/org/health/diet,/org/health/log,/org/proposals",
    )];
    let run = |from: &str, to: &str| f.run_with_env(&["move", from, to, "--who", "x"], b"", &allow);
    assert_eq!(
        code(&run("/org/health/diet/d.md", "org/health/log/2026-11/d.md")),
        0
    );
    let o = run("/org/notes/n.md", "/org/proposals/n.md");
    assert_eq!(code(&o), 2, "{o:?}");
    let o = run("org/health/log/2026-11/d.md", "/org/notes/d.md");
    assert_eq!(code(&o), 2, "{o:?}");
    assert_eq!(last_log(&f)[1], "move-not-allowed");
    assert_eq!(
        f.state.lock().unwrap().moves.len(),
        1,
        "no call for a refusal"
    );
    // The limits of the door hold for both names too.
    let ro = [("ORGTHREAD_READ_ONLY", "/org/notes")];
    let o = f.run_with_env(
        &["move", "/org/notes/n.md", "/org/n.md", "--who", "x"],
        b"",
        &ro,
    );
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(f.get("/org/notes/n.md").is_some());
}

#[test]
fn on_a_server_the_name_decides_if_a_moved_file_is_not_text() {
    let f = Fake::new();
    f.put("/org/notes/photo.jpg", b"\x89PNG\x00");
    let o = mv(&f, "/org/notes/photo.jpg", "/org/notes/b.jpg");
    assert_eq!(code(&o), 2, "{o:?}");
    assert_eq!(last_log(&f)[1], "move-not-text");
    let open = [("ORGTHREAD_BINARY_ALLOW", "/org/pictures")];
    let o = f.run_with_env(
        &[
            "move",
            "/org/notes/photo.jpg",
            "/org/pictures/b.jpg",
            "--who",
            "x",
        ],
        b"",
        &open,
    );
    assert_eq!(code(&o), 0, "{o:?}");
    assert!(f.get("/org/pictures/b.jpg").is_some());
}

const FIXED: (&str, &str) = ("ORGTHREAD_FIXED_FOLDERS", "/org");

/// The word and the path of each log line.
fn log_words(f: &Fake) -> Vec<(String, String)> {
    fs::read_to_string(f.dir.join("log"))
        .unwrap_or_default()
        .lines()
        .map(|l| {
            let fields: Vec<&str> = l.split('\t').collect();
            (fields[1].to_string(), fields[2].to_string())
        })
        .collect()
}

#[test]
fn an_upload_into_a_missing_dropbox_folder_names_and_logs_the_folders() {
    let f = Fake::new();
    f.put(FILE, b"old\n");
    let o = f.run(&["upload", "/org/new/deep/a.md", "--who", "chat"], b"abc");
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        "written: org/new/deep/a.md, 3 bytes, new folders: org/new, org/new/deep\n"
    );
    assert_eq!(f.get("/org/new/deep/a.md").unwrap(), b"abc");
    let words: Vec<(&str, &str)> = vec![
        ("made-folder", "org/new"),
        ("made-folder", "org/new/deep"),
        ("written", "org/new/deep/a.md"),
    ];
    let want: Vec<(String, String)> = words
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
    assert_eq!(log_words(&f), want);
    let first = fs::read_to_string(f.dir.join("log")).unwrap();
    let first: Vec<&str> = first.lines().next().unwrap().split('\t').collect();
    assert_eq!(first[3..], ["chat", "-", "-", "-"]);
}

#[test]
fn the_guard_refuses_before_any_dropbox_call_that_writes() {
    let f = Fake::new();
    f.put(FILE, b"old\n");
    for command in [&["write"][..], &["upload"]] {
        let o = f.run_with_env(
            &[command[0], "/org/garden/a.md", "--who", "chat"],
            b"abc",
            &[FIXED],
        );
        assert_eq!(code(&o), 2, "{o:?}");
        let said = String::from_utf8_lossy(&o.stderr);
        assert!(
            said.contains("a folder of ORGTHREAD_FIXED_FOLDERS"),
            "{said}"
        );
    }
    for command in ["copy", "move"] {
        let o = f.run_with_env(
            &[command, FILE, "/org/garden/a.md", "--who", "chat"],
            b"",
            &[FIXED],
        );
        assert_eq!(code(&o), 2, "{command}: {o:?}");
    }
    let s = f.state.lock().unwrap();
    assert_eq!(s.uploads, 0, "no upload call");
    assert!(
        s.copies.is_empty() && s.moves.is_empty(),
        "no copy or move call"
    );
    drop(s);
    assert_eq!(f.names(), [FILE]);
    assert!(fs::read_dir(f.dir.join("locks")).unwrap().next().is_none());
    // Deeper is fine.
    let o = f.run_with_env(
        &["write", "/org/notes/2026/a.md", "--who", "chat"],
        b"abc",
        &[FIXED],
    );
    assert_eq!(code(&o), 0, "{o:?}");
    assert!(
        String::from_utf8_lossy(&o.stdout).ends_with(", new folders: org/notes/2026\n"),
        "{o:?}"
    );
}

#[test]
fn a_change_to_a_dropbox_file_that_exists_makes_no_extra_look() {
    let f = Fake::new();
    f.put(FILE, b"old\n");
    assert_eq!(code(&f.read("chat")), 0);
    let before = f.state.lock().unwrap().metadata_calls;
    let o = f.run_with_env(&["write", FILE, "--who", "chat"], b"new\n", &[FIXED]);
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        "written: org/notes/invented.md, 4 bytes\n"
    );
    assert_eq!(
        f.state.lock().unwrap().metadata_calls - before,
        1,
        "only the look for the rev of the file, no look at its folders"
    );
    assert_eq!(log_words(&f).len(), 1, "no made-folder line");
    // A new file in a folder that exists costs one look at that folder.
    let before = f.state.lock().unwrap().metadata_calls;
    let o = f.run(&["write", "/org/notes/b.md", "--who", "chat"], b"b\n");
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(f.state.lock().unwrap().metadata_calls - before, 1);
}

#[test]
fn a_dropbox_copy_that_fails_after_its_folders_logs_them_before_its_refusal() {
    let f = Fake::new();
    f.put(FILE, b"source\n");
    // Dropbox makes the folders and stores other bytes, then gives no answer.
    {
        let mut s = f.state.lock().unwrap();
        s.copy_hangs_up = Some(true);
        s.copy_stores = Some(b"other".to_vec());
    }
    let o = copy(&f, FILE, "/org/new/deep/d.md");
    assert_eq!(code(&o), 1, "{o:?}");
    assert_eq!(
        f.get("/org/new/deep/d.md").unwrap(),
        b"other",
        "never removed"
    );
    let want: Vec<(String, String)> = [
        ("made-folder", "org/new"),
        ("made-folder", "org/new/deep"),
        ("failed", "org/new/deep/d.md"),
    ]
    .iter()
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect();
    assert_eq!(log_words(&f), want);
    let said = String::from_utf8_lossy(&o.stderr);
    assert!(
        said.contains("these new folders were made, and they stay: org/new, org/new/deep"),
        "{said}"
    );
}

/// Invented notes in the stand-in for a search, with one closed folder.
fn search_fake() -> Fake {
    let f = Fake::new();
    f.put(
        "/org/notes/health/log.md",
        b"Blood pressure morning: 120/80\nslept well\n",
    );
    f.put("/org/notes/plan.org", b"* TODO measure blood pressure\n");
    f.put("/org/notes/.obsidian/app.json", b"blood pressure\n");
    f.put("/org/notes/scan.bin", b"blood\0pressure");
    f.put("/org/notes/photo.png", b"blood pressure");
    f.put("/org/private/a.md", b"blood pressure\n");
    f
}

fn search(f: &Fake, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut all = vec!["search"];
    all.extend(args);
    all.extend(["--who", "chat"]);
    f.run_with_env(&all, b"", env)
}

#[test]
fn a_dropbox_search_is_one_zip_in_memory_and_skips_what_it_should() {
    let f = search_fake();
    let o = search(&f, &["BLOOD pressure", "/org/notes"], &[]);
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(
        String::from_utf8(o.stdout).unwrap(),
        "/org/notes/health/log.md:1: Blood pressure morning: 120/80\n\
         /org/notes/plan.org:1: * TODO measure blood pressure\n"
    );
    {
        let s = f.state.lock().unwrap();
        assert_eq!(s.zips, 1, "one zip for the whole search");
        assert_eq!((s.downloads, s.list_calls), (0, 0), "no other call");
    }
    let o = search(&f, &["blood", "/org/notes", "--all"], &[]);
    assert!(
        String::from_utf8(o.stdout)
            .unwrap()
            .contains("/org/notes/.obsidian/app.json:1:")
    );
    // The log has the folder and the number of files, never the text or a hit.
    let log = fs::read_to_string(f.dir.join("log")).unwrap();
    let first: Vec<&str> = log.lines().next().unwrap().split('\t').collect();
    assert_eq!(first[1..], ["searched", "org/notes", "chat", "-", "-", "2"]);
    assert!(!log.to_lowercase().contains("blood"), "{log}");
    assert!(!log.contains("120/80"), "{log}");
    // No zip and no text stays on the disk of the server.
    let mut todo = vec![f.dir.clone()];
    while let Some(dir) = todo.pop() {
        for item in fs::read_dir(dir).unwrap() {
            let path = item.unwrap().path();
            if path.is_dir() {
                todo.push(path);
            } else {
                let bytes = fs::read(&path).unwrap();
                let text = String::from_utf8_lossy(&bytes).to_lowercase();
                assert!(!text.contains("blood"), "{}", path.display());
                assert!(!bytes.starts_with(b"PK"), "a zip at {}", path.display());
            }
        }
    }
}

#[test]
fn a_dropbox_search_with_names_lists_every_page_and_fetches_no_file() {
    let f = search_fake();
    f.state.lock().unwrap().page = 2;
    let o = search(&f, &["LOG", "/org/notes", "--names"], &[]);
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(
        String::from_utf8(o.stdout).unwrap(),
        "/org/notes/health/log.md\n"
    );
    let o = search(&f, &["o", "/org/notes", "--names"], &[]);
    let said = String::from_utf8(o.stdout).unwrap();
    assert!(
        said.contains("/org/notes/photo.png\n"),
        "a picture is found by its name: {said}"
    );
    assert!(!said.contains(".obsidian"), "{said}");
    let s = f.state.lock().unwrap();
    assert_eq!(s.zips, 0);
    assert_eq!(s.downloads, 0);
    assert!(s.list_calls >= 4, "several pages: {}", s.list_calls);
}

#[test]
fn a_dropbox_content_search_of_a_folder_with_a_closed_folder_is_refused() {
    let f = search_fake();
    let deny = [("ORGTHREAD_DENY", "/org/private")];
    let o = search(&f, &["blood", "/org"], &deny);
    assert_eq!(code(&o), 2, "{o:?}");
    let said = String::from_utf8_lossy(&o.stderr);
    assert!(said.contains("/org/private"), "{said}");
    assert!(!said.to_lowercase().contains("blood"), "{said}");
    assert_eq!(f.state.lock().unwrap().zips, 0, "no zip was fetched");
    // A folder with no closed path works, and names work for /org too.
    assert_eq!(code(&search(&f, &["blood", "/org/notes"], &deny)), 0);
    let o = search(&f, &["a.md", "/org", "--names"], &deny);
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(String::from_utf8(o.stdout).unwrap(), "no hits in /org\n");
    // The closed folder itself is closed for names too.
    assert_eq!(
        code(&search(&f, &["a", "/org/private", "--names"], &deny)),
        2
    );
}

#[test]
fn a_dropbox_zip_above_the_limit_is_refused_with_exit_2() {
    let f = search_fake();
    f.put("/org/notes/big.md", &vec![b'x'; 1_200_000]);
    let o = search(
        &f,
        &["blood", "/org/notes"],
        &[("ORGTHREAD_SEARCH_MB", "1")],
    );
    assert_eq!(code(&o), 2, "{o:?}");
    let said = String::from_utf8_lossy(&o.stderr);
    assert!(
        said.contains("too large") && said.contains("ORGTHREAD_SEARCH_MB"),
        "{said}"
    );
    assert!(o.stdout.is_empty());
    assert_eq!(
        code(&search(&f, &["blood", "/org/notes"], &[])),
        0,
        "25 MB by default"
    );
    f.state.lock().unwrap().zip_too_large = true;
    let o = search(&f, &["blood", "/org/notes"], &[]);
    assert_eq!(code(&o), 2, "{o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("too_large"));
    let log = fs::read_to_string(f.dir.join("log")).unwrap();
    assert_eq!(log.lines().count(), 1, "only the search that ran: {log}");
}

#[test]
fn a_dropbox_search_of_nothing_is_6_and_of_a_file_is_2() {
    let f = search_fake();
    assert_eq!(code(&search(&f, &["blood", "/org/gone"], &[])), 6);
    assert_eq!(
        code(&search(&f, &["blood", "/org/gone", "--names"], &[])),
        6
    );
    assert_eq!(code(&search(&f, &["blood", "/org/notes/plan.org"], &[])), 2);
    assert_eq!(
        code(&search(
            &f,
            &["blood", "/org/notes/plan.org", "--names"],
            &[]
        )),
        2
    );
    assert_eq!(code(&search(&f, &["blood", "/org/../x"], &[])), 2);
}
