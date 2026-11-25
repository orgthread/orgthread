//! The door for a device: `orgthread serve --http <address> --device`.
//!
//! It runs as its own process, from its own folder, with its own `.env`, log,
//! locks and file of tokens. A device logs in with a token, not with OAuth. It
//! gets three tools, `list`, `read` and `patch`, and only on the paths of the
//! rules. Only where the rules name their folders, it also gets `trash` as a
//! tool, and the exact bytes of a file at `GET /download` and `PUT /upload`,
//! outside MCP, as a file may be larger than a message. Each tool call starts the command itself, as on the other doors, so
//! the lock, the checks and the refusals of the command hold as well.
//!
//! No request or response body goes into the log or to the error output: only
//! the time, the word, the path and the device.

use crate::device::{self, Cap, Holder, Listing, Rules, Store};
use crate::fingerprint::fingerprint;
use crate::log;
use crate::login::parse_form;
use crate::serve::{self, INVALID_PARAMS, INVALID_REQUEST, NO_METHOD, NOT_JSON};
use crate::write::exit;
use serde_json::{Value, json};
use std::io::{self, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A device sends a path or a small patch, so a request is small.
const MAX_BODY: usize = 1024 * 1024;
/// Five wrong pairing codes within 15 minutes close `/pair` for 15 minutes.
const MAX_FAILS: usize = 5;
const FAIL_WINDOW: Duration = Duration::from_secs(15 * 60);
/// The tools of this door. Every other tool is refused.
pub const TOOLS: [&str; 3] = ["list", "read", "patch"];
/// The status of an answer at `/download` or `/upload` that is no success: a
/// refusal or a failure of the command, with its exit code in the body.
const NOT_DONE: &str = "422 Unprocessable Content";
/// A trash move tries `a.note`, then `a (2).note` and so on, up to this many.
const TRASH_TRIES: usize = 20;

/// The limits of download and upload of a device, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Moved {
    /// The largest file.
    pub file: u64,
    /// The most bytes of downloads and uploads together in 24 hours.
    pub day: u64,
}

impl Default for Moved {
    fn default() -> Self {
        Moved {
            file: 20_000_000,
            day: 200_000_000,
        }
    }
}

/// The settings of a device door, read once at its start.
pub struct DeviceDoor {
    /// The app of this door, such as `reader`.
    pub app: String,
    pub rules: Rules,
    /// The files whose reads count for the cap. `None`: every read counts.
    pub capped: Option<Vec<String>>,
    pub cap: Cap,
    /// The limits of download and upload.
    pub moved: Moved,
    pub store: Store,
    /// The program that sends the owner a push.
    pub notify: Option<PathBuf>,
    pub log: PathBuf,
    /// The root of a local folder, `None` with Dropbox.
    pub root: Option<PathBuf>,
    /// The path part of the public address, such as `/a-long-secret`, or empty.
    pub prefix: String,
    /// The times of the wrong pairing codes.
    fails: Mutex<Vec<Instant>>,
}

impl DeviceDoor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        app: String,
        rules: Rules,
        capped: Option<Vec<String>>,
        cap: Cap,
        store: Store,
        notify: Option<PathBuf>,
        log: PathBuf,
        root: Option<PathBuf>,
        prefix: String,
    ) -> DeviceDoor {
        DeviceDoor {
            app,
            rules,
            capped,
            cap,
            moved: Moved::default(),
            store,
            notify,
            log,
            root,
            prefix,
            fails: Mutex::new(Vec::new()),
        }
    }

    /// The same door, with other limits for download and upload.
    pub fn with_moved(mut self, moved: Moved) -> DeviceDoor {
        self.moved = moved;
        self
    }

    /// The tools that `tools/list` shows: `trash` only where its rules are set.
    fn offered(&self) -> Vec<&'static str> {
        let mut tools = TOOLS.to_vec();
        if !self.rules.trash.is_empty() {
            tools.push("trash");
        }
        tools
    }

    /// One line in the log: the time, the word, the path and the device.
    fn log(&self, word: &str, path: &str, who: &str) {
        let now = log::utc_now();
        let shown = device::shown(path);
        let line = log::line(&[&now, word, &shown, who, "", "", ""]);
        if let Err(e) = log::append(&self.log, &line) {
            eprintln!(
                "orgthread: the log line {word} failed, {}: {e}",
                self.log.display()
            );
        }
    }

    /// Runs the program of `ORGTHREAD_DEVICE_NOTIFY` with one line of text, and
    /// does not wait for it. Its output goes nowhere.
    fn notify(&self, text: &str, who: &str) {
        let Some(program) = &self.notify else {
            return;
        };
        let started = Command::new(program)
            .arg(text)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match started {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(_) => self.log("not-notified", "", who),
        }
    }

    /// `Ok` when no part of the path is a link. Only a local folder has links.
    fn no_link(&self, path: &str) -> Result<(), String> {
        match &self.root {
            Some(root) => device::no_link(root, path),
            None => Ok(()),
        }
    }

    /// True when a read of `path` counts for the cap.
    fn counts(&self, path: &str) -> bool {
        match &self.capped {
            None => true,
            Some(rules) => rules.iter().any(|rule| device::fits(rule, path)),
        }
    }

    /// The path that the command gets: with Dropbox the same, in a local folder
    /// the full path on the disk.
    fn for_command(&self, path: &str) -> String {
        match &self.root {
            Some(root) => format!("{}{path}", root.display()),
            None => path.to_string(),
        }
    }

    /// The text of a command for the device: in a local folder the root on the
    /// disk becomes `/`, so the device sees its own paths and no more.
    fn tidy(&self, text: &str) -> String {
        match &self.root {
            Some(root) => text.replace(&format!("{}/", root.display()), "/"),
            None => text.to_string(),
        }
    }

    /// True while too many wrong codes came in: then `/pair` is closed.
    fn pair_closed(&self) -> bool {
        let mut fails = self.fails.lock().unwrap_or_else(|e| e.into_inner());
        fails.retain(|t| t.elapsed() < FAIL_WINDOW);
        fails.len() >= MAX_FAILS
    }

    fn pair_failed(&self) {
        let mut fails = self.fails.lock().unwrap_or_else(|e| e.into_inner());
        fails.push(Instant::now());
    }
}

/// Serves until the process ends, one thread per connection.
pub fn run(addresses: &[SocketAddr], door: &DeviceDoor, exe: &Path) -> io::Result<()> {
    let listener = TcpListener::bind(addresses)?;
    std::thread::scope(|scope| {
        for stream in listener.incoming().flatten() {
            scope.spawn(move || {
                let _ = handle(stream, door, exe);
            });
        }
    });
    Ok(())
}

fn handle(mut stream: TcpStream, door: &DeviceDoor, exe: &Path) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(60)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let head = serve::read_head(&mut reader)?;
    // An upload may be larger than any other request: its body is read only
    // after the token, the rules and the limits allow it.
    if head.method == "PUT" && head.path.strip_prefix(door.prefix.as_str()) == Some("/upload") {
        let Some(holder) = holder_of(&mut stream, door, &head.bearer)? else {
            // So the device can read the 401, not a reset connection.
            drop_body(&mut reader, &head, door.moved.file);
            return Ok(());
        };
        return upload(&mut stream, &mut reader, &head, door, exe, &holder);
    }
    if head.length > MAX_BODY {
        return serve::respond(&mut stream, "413 Content Too Large", "", b"");
    }
    let mut body = vec![0; head.length];
    reader.read_exact(&mut body)?;
    // A wrong address learns nothing, not even that this is the right server.
    let Some(path) = head.path.strip_prefix(door.prefix.as_str()) else {
        return serve::respond(&mut stream, "404 Not Found", "", b"");
    };
    match (head.method.as_str(), path) {
        ("POST", "/pair") => pair(&mut stream, door, &body),
        (method, "/download" | "/upload") => {
            let Some(holder) = holder_of(&mut stream, door, &head.bearer)? else {
                return Ok(());
            };
            match (method, path) {
                ("GET", "/download") => download(&mut stream, &head, door, exe, &holder),
                _ => serve::respond(&mut stream, "405 Method Not Allowed", "", b""),
            }
        }
        (method, "/mcp") => {
            let Some(holder) = holder_of(&mut stream, door, &head.bearer)? else {
                return Ok(());
            };
            if method != "POST" {
                return serve::respond(&mut stream, "405 Method Not Allowed", "", b"");
            }
            let answered = match serde_json::from_slice::<Value>(&body) {
                Ok(message) => answer(&message, exe, door, &holder),
                Err(e) => Some(serve::problem(Value::Null, NOT_JSON, &e.to_string())),
            };
            match answered {
                Some(reply) => serve::respond_json(&mut stream, "200 OK", &reply),
                None => serve::respond(&mut stream, "202 Accepted", "", b""),
            }
        }
        _ => serve::respond(&mut stream, "404 Not Found", "", b""),
    }
}

/// The holder of the token of a request. Without one that works, the answer
/// is an empty 401, or a 500 when the file of the tokens cannot be read, and
/// the result is `None`.
fn holder_of(
    stream: &mut TcpStream,
    door: &DeviceDoor,
    bearer: &str,
) -> io::Result<Option<Holder>> {
    match door.store.holder(&door.app, bearer, device::now()) {
        Ok(Some(holder)) => Ok(Some(holder)),
        Ok(None) => {
            let extra = "WWW-Authenticate: Bearer\r\n";
            serve::respond(stream, "401 Unauthorized", extra, b"")?;
            Ok(None)
        }
        Err(e) => {
            eprintln!("orgthread: could not read the tokens: {e}");
            serve::respond(stream, "500 Internal Server Error", "", b"")?;
            Ok(None)
        }
    }
}

/// `POST /pair` with `{"code": "..."}`: a token for a code, once.
fn pair(stream: &mut TcpStream, door: &DeviceDoor, body: &[u8]) -> io::Result<()> {
    if door.pair_closed() {
        let error = json!({"error": "too_many_tries"});
        return serve::respond_json(stream, "429 Too Many Requests", &error);
    }
    let given: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let Some(code) = given["code"].as_str() else {
        return serve::respond_json(stream, "400 Bad Request", &json!({"error": "bad_request"}));
    };
    match door.store.pair(&door.app, code, device::now()) {
        Ok(Some(paired)) => {
            let who = device::who(&paired.device, &door.app);
            door.log("paired", "", &who);
            door.notify(&device::pair_notice(&paired.device, &door.app), &who);
            let answer = json!({
                "token": paired.token,
                "expires": device::date_text(paired.until),
            });
            serve::respond_json(stream, "200 OK", &answer)
        }
        Ok(None) => {
            door.pair_failed();
            door.log("pair-refused", "", "");
            // Only a token problem gets 401, and nothing gets 403: a wrong code
            // is a bad request, as the apps expect.
            serve::respond_json(stream, "400 Bad Request", &json!({"error": "wrong_code"}))
        }
        Err(e) => {
            eprintln!("orgthread: a pairing failed: {e}");
            let error = json!({"error": "server_error"});
            serve::respond_json(stream, "500 Internal Server Error", &error)
        }
    }
}

/// The reply to one message, as on the other doors, with the tools of a device.
fn answer(message: &Value, exe: &Path, door: &DeviceDoor, holder: &Holder) -> Option<Value> {
    if !message.is_object() {
        let text = "a message must be one JSON object";
        return Some(serve::problem(Value::Null, INVALID_REQUEST, text));
    }
    let id = message.get("id")?.clone();
    let method = message.get("method")?;
    let params = message.get("params").unwrap_or(&Value::Null);
    let result = match method.as_str() {
        Some("initialize") => Ok(serve::greeting(params)),
        Some("ping") => Ok(json!({})),
        Some("tools/list") => Ok(tools(door)),
        Some("tools/call") => call(params, exe, door, holder),
        _ => Err((NO_METHOD, format!("there is no method {method}"))),
    };
    Some(match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err((code, text)) => serve::problem(id, code, &text),
    })
}

/// The three tools, and `trash` where its rules are set. There is no `who`:
/// the door names the device itself.
fn tools(door: &DeviceDoor) -> Value {
    let file = json!({"type": "string", "description": "The full path of the file, from the root: /books/a/chapter-01.txt"});
    let mut value = json!({"tools": [
        {
            "name": "read",
            "description": "Gives the exact text of a file, and notes that this device read it.",
            "inputSchema": {
                "type": "object",
                "properties": {"file": file},
                "required": ["file"],
            },
        },
        {
            "name": "patch",
            "description": "Changes part of a file. Each edit replaces its exact old text, whole \
                lines with their newlines, by its new text. The old text must occur once, or \
                give the line where it starts. append adds lines at the end. All edits are \
                applied, or none. Read the file first. A patch never makes a file.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": file,
                    "edits": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "old": {"type": "string"},
                                "new": {"type": "string"},
                                "line": {"type": "integer", "minimum": 1},
                            },
                            "required": ["old", "new"],
                        },
                    },
                    "append": {"type": "string"},
                },
                "required": ["file"],
            },
        },
        {
            "name": "list",
            "description": "Lists the files and folders in one or more folders, one level \
                down: one group per folder, and per entry its size in bytes, its time and its \
                name. A folder name ends with /. To read a file, join the header of its group \
                and its name.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "folders": {
                        "type": "array",
                        "items": {"type": "string"},
                        "minItems": 1,
                        "description": "The full paths of the folders, from the root: /books/",
                    },
                    "fingerprint": {
                        "type": "boolean",
                        "description": "true also shows the fingerprint of each file",
                    },
                },
                "required": ["folders"],
            },
        },
    ]});
    if !door.rules.trash.is_empty() {
        let tool = json!({
            "name": "trash",
            "description": "Moves a file into .trash/YYYY-MM-DD/ of its folder, with the rest \
                of its path, never over a file: a second file of the same name and day gets \
                (2), and so on. Only the version of fingerprint moves.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": file,
                    "fingerprint": {"type": "string", "description": "The fingerprint this device saw, from list"},
                },
                "required": ["file", "fingerprint"],
            },
        });
        if let Some(list) = value["tools"].as_array_mut() {
            list.push(tool);
        }
    }
    value
}

/// True when `text` is a fingerprint: 64 small hex signs.
fn is_fingerprint(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A refusal of the door itself, with exit code 2 as for a closed path, and
/// one log line.
fn refuse(door: &DeviceDoor, holder: &Holder, path: &str, reason: &str) -> Value {
    let who = device::who(&holder.device, &holder.app);
    door.log("refused", path, &who);
    let text = format!("exit code {}\norgthread: {reason}\n", exit::USAGE);
    json!({"content": [{"type": "text", "text": text}], "isError": true})
}

fn call(
    params: &Value,
    exe: &Path,
    door: &DeviceDoor,
    holder: &Holder,
) -> Result<Value, (i32, String)> {
    let name = params["name"].as_str().unwrap_or_default();
    let arguments = &params["arguments"];
    let text_of = |key: &str| {
        arguments[key]
            .as_str()
            .ok_or_else(|| (INVALID_PARAMS, format!("{name} needs {key}, as text")))
    };
    let who = device::who(&holder.device, &holder.app);
    match name {
        "read" => {
            let file = text_of("file")?;
            if let Err(why) = door.rules.read(file).and_then(|()| door.no_link(file)) {
                return Ok(refuse(door, holder, file, &why));
            }
            if door.counts(file) {
                match door.store.count(holder, door.cap, device::now()) {
                    Ok(None) => {}
                    Ok(Some(capped)) => {
                        door.log("capped", file, &who);
                        if capped.warn {
                            door.notify(&device::cap_notice(holder, &capped), &who);
                        }
                        let text = format!(
                            "exit code {}\norgthread: refused. This device reached its read cap of {}. It may read again after {}.\n",
                            exit::USAGE,
                            capped.reached,
                            device::time_text(capped.until)
                        );
                        return Ok(
                            json!({"content": [{"type": "text", "text": text}], "isError": true}),
                        );
                    }
                    Err(e) => {
                        let text = format!(
                            "exit code {}\norgthread: could not count the read: {e}\n",
                            exit::FAILED
                        );
                        return Ok(
                            json!({"content": [{"type": "text", "text": text}], "isError": true}),
                        );
                    }
                }
            }
            let target = door.for_command(file);
            let run = serve::start(exe, &["read", "--who", &who, &target], None);
            Ok(tidied(door, Some("read"), run))
        }
        "patch" => {
            let file = text_of("file")?;
            if let Err(why) = door.rules.patch(file).and_then(|()| door.no_link(file)) {
                return Ok(refuse(door, holder, file, &why));
            }
            let mut patch = serde_json::Map::new();
            for key in ["edits", "append"] {
                if !arguments[key].is_null() {
                    patch.insert(key.to_string(), arguments[key].clone());
                }
            }
            // The command checks the patch itself, so the refusals are the same.
            let text = Value::Object(patch).to_string();
            let size = text.len().to_string();
            let target = door.for_command(file);
            let args = ["patch", "--who", &who, "--size", &size, &target];
            let run = serve::start(exe, &args, Some(text.as_bytes()));
            Ok(tidied(door, Some("patch"), run))
        }
        "list" => {
            let folders = arguments["folders"]
                .as_array()
                .filter(|f| !f.is_empty())
                .ok_or_else(|| {
                    (
                        INVALID_PARAMS,
                        "list needs folders, as a list of paths".into(),
                    )
                })?;
            let mut targets = Vec::new();
            // Each header of the answer, and whether its group shows folder names only.
            let mut headers = Vec::new();
            for folder in folders {
                let folder = folder
                    .as_str()
                    .ok_or_else(|| (INVALID_PARAMS, "list needs each folder as text".into()))?;
                let listing = door.rules.list(folder).and_then(|listing| {
                    door.no_link(folder)?;
                    Ok(listing)
                });
                match listing {
                    Err(why) => return Ok(refuse(door, holder, folder, &why)),
                    Ok(listing) => headers.push((format!("{folder}:"), listing)),
                }
                targets.push(door.for_command(folder));
            }
            // One level down only: a deeper list could follow a link that the
            // check of each folder never saw.
            let mut args = vec!["list".to_string()];
            match &arguments["fingerprint"] {
                Value::Null | Value::Bool(false) => {}
                Value::Bool(true) => args.push("--fingerprint".into()),
                _ => {
                    return Err((
                        INVALID_PARAMS,
                        "list needs fingerprint as true or false".into(),
                    ));
                }
            }
            args.push("--".into());
            args.extend(targets);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let mut value = tidied(door, Some("list"), serve::start(exe, &args, None));
            if let Some(text) = value["content"][0]["text"].as_str() {
                value["content"][0]["text"] = Value::String(folders_only(text, &headers));
            }
            Ok(value)
        }
        "trash" if !door.rules.trash.is_empty() => {
            let file = text_of("file")?;
            let print = text_of("fingerprint")?;
            if !is_fingerprint(print) {
                return Err((
                    INVALID_PARAMS,
                    "trash needs fingerprint, the 64 signs that list gives".into(),
                ));
            }
            let date = device::vienna_date(device::now());
            let place = door.rules.trash(file, &date).and_then(|to| {
                door.no_link(file)?;
                door.no_link(&to)?;
                Ok(to)
            });
            let to = match place {
                Ok(to) => to,
                Err(why) => return Ok(refuse(door, holder, file, &why)),
            };
            let from = door.for_command(file);
            let mut run = None;
            for n in 1..=TRASH_TRIES {
                let name = if n == 1 {
                    to.clone()
                } else {
                    device::numbered(&to, n)
                };
                let target = door.for_command(&name);
                let args = [
                    "move",
                    "--who",
                    &who,
                    "--fingerprint",
                    print,
                    "--",
                    &from,
                    &target,
                ];
                let done = serve::start(exe, &args, None);
                let taken = matches!(&done, Ok(out) if out.status.code() == Some(exit::EXISTS));
                run = Some(done);
                if !taken {
                    break;
                }
            }
            let run = run.unwrap_or_else(|| Err(io::Error::other("no move was tried")));
            Ok(tidied(door, Some("trash"), run))
        }
        _ => {
            let path = arguments["file"].as_str().unwrap_or_default();
            let mut reason = format!(
                "refused. This door offers only {}, and not {}.",
                door.offered().join(", "),
                device::shown(name)
            );
            let open = match name {
                "download" => !door.rules.download.is_empty(),
                "upload" => !door.rules.upload.is_empty(),
                _ => false,
            };
            if open {
                reason.push_str(" A device moves the bytes of a file with GET /download or PUT /upload, outside /mcp.");
            }
            Ok(refuse(door, holder, path, &reason))
        }
    }
}

/// The answer at `/download` or `/upload` that is no success: 422, with the
/// exit code and the text, in the form of a refused tool.
fn not_done(stream: &mut TcpStream, code: i32, text: &str) -> io::Result<()> {
    let answer = json!({"exit": code, "text": text});
    serve::respond_json(stream, NOT_DONE, &answer)
}

/// A refusal of the door at `/download` or `/upload`: exit code 2 and one log
/// line, as `refuse` gives for a tool.
fn not_allowed(
    stream: &mut TcpStream,
    door: &DeviceDoor,
    holder: &Holder,
    path: &str,
    reason: &str,
) -> io::Result<()> {
    let who = device::who(&holder.device, &holder.app);
    door.log("refused", path, &who);
    let text = format!("exit code {}\norgthread: {reason}\n", exit::USAGE);
    not_done(stream, exit::USAGE, &text)
}

/// The text of a command that did not end with 0, tidied for the device.
fn said(door: &DeviceDoor, out: &std::process::Output) -> String {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    match out.status.code() {
        Some(code) => door.tidy(&format!("exit code {code}\n{text}")),
        None => door.tidy(&format!("the command was stopped\n{text}")),
    }
}

/// The one file of a request at `/download` or `/upload`: `?file=<path>`,
/// with `%XX` and `+` decoded, as a form gives it.
fn file_of(head: &serve::Head) -> Result<String, String> {
    parse_form(&head.query)
        .remove("file")
        .filter(|file| !file.is_empty())
        .ok_or_else(|| "needs ?file=<path>, the full path of the file from the root".into())
}

/// Counts `bytes` against the cap in MB of the day. `Ok(true)` means go on;
/// `Ok(false)` means the answer is sent: capped, or the count failed.
fn count_moved(
    stream: &mut TcpStream,
    door: &DeviceDoor,
    holder: &Holder,
    path: &str,
    bytes: u64,
) -> io::Result<bool> {
    let who = device::who(&holder.device, &holder.app);
    match door
        .store
        .count_bytes(holder, door.moved.day, bytes, device::now())
    {
        Ok(None) => Ok(true),
        Ok(Some(capped)) => {
            door.log("capped", path, &who);
            if capped.warn {
                door.notify(&device::moved_notice(holder, &capped), &who);
            }
            let text = format!(
                "exit code {}\norgthread: refused. This device reached its cap of {} for downloads and uploads. It may move files again after {}.\n",
                exit::USAGE,
                capped.reached,
                device::time_text(capped.until)
            );
            not_done(stream, exit::USAGE, &text)?;
            Ok(false)
        }
        Err(e) => {
            let text = format!(
                "exit code {}\norgthread: could not count the transfer: {e}\n",
                exit::FAILED
            );
            not_done(stream, exit::FAILED, &text)?;
            Ok(false)
        }
    }
}

/// `GET /download?file=<path>`: the exact bytes of one file, with its
/// fingerprint in the header `X-Orgthread-Fingerprint`.
fn download(
    stream: &mut TcpStream,
    head: &serve::Head,
    door: &DeviceDoor,
    exe: &Path,
    holder: &Holder,
) -> io::Result<()> {
    let file = match file_of(head) {
        Ok(file) => file,
        Err(why) => return not_allowed(stream, door, holder, "", &format!("download {why}")),
    };
    if door.rules.download.is_empty() {
        let why = format!(
            "refused. This door offers no download: {} is not set.",
            crate::settings::DEVICE_DOWNLOAD
        );
        return not_allowed(stream, door, holder, &file, &why);
    }
    if let Err(why) = door
        .rules
        .download(&file)
        .and_then(|()| door.no_link(&file))
    {
        return not_allowed(stream, door, holder, &file, &why);
    }
    let who = device::who(&holder.device, &holder.app);
    let target = door.for_command(&file);
    // First the size, so a file over a limit moves no byte.
    let checked = serve::start(exe, &["download", "--check", "--who", &who, &target], None)?;
    if !checked.status.success() {
        let code = checked.status.code().unwrap_or(exit::FAILED);
        return not_done(stream, code, &said(door, &checked));
    }
    let size: u64 = String::from_utf8_lossy(&checked.stdout)
        .trim()
        .parse()
        .unwrap_or(u64::MAX);
    if size > door.moved.file {
        let why = format!(
            "refused. {} has {size} bytes, more than the limit of {} MB of a device file",
            device::shown(&file),
            door.moved.file / 1_000_000
        );
        return not_allowed(stream, door, holder, &file, &why);
    }
    if !count_moved(stream, door, holder, &file, size)? {
        return Ok(());
    }
    let out = serve::start(exe, &["download", "--who", &who, &target], None)?;
    let code = out.status.code().unwrap_or(exit::FAILED);
    // 8: the bytes went out whole, only the log line failed.
    if code != 0 && code != exit::NOT_LOGGED {
        return not_done(stream, code, &said(door, &out));
    }
    if out.stdout.len() as u64 > door.moved.file {
        let why = format!(
            "refused. {} grew past the limit of {} MB of a device file during the download",
            device::shown(&file),
            door.moved.file / 1_000_000
        );
        return not_allowed(stream, door, holder, &file, &why);
    }
    let extra = format!(
        "Content-Type: application/octet-stream\r\nX-Orgthread-Fingerprint: {}\r\nX-Orgthread-Exit: {code}\r\n",
        fingerprint(&out.stdout)
    );
    serve::respond(stream, "200 OK", &extra, &out.stdout)
}

/// Reads and drops the body of a refused upload, so the device can read the
/// answer, up to `max` bytes and for at most a few seconds.
fn drop_body(reader: &mut BufReader<TcpStream>, head: &serve::Head, max: u64) {
    if head.length == 0 || head.wants_continue {
        return;
    }
    let _ = reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(5)));
    let rest = (head.length as u64).min(max);
    let _ = io::copy(&mut reader.by_ref().take(rest), &mut io::sink());
}

/// `PUT /upload?file=<path>&fingerprint=<the version it replaces>`: the exact
/// bytes of one file, written by the `upload` command. With no fingerprint,
/// the file must be new. The answer gives the new fingerprint.
fn upload(
    stream: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    head: &serve::Head,
    door: &DeviceDoor,
    exe: &Path,
    holder: &Holder,
) -> io::Result<()> {
    let limit = door.moved.file;
    // The answer of a refusal comes before the body, which is then dropped.
    let refused = |stream: &mut TcpStream,
                   reader: &mut BufReader<TcpStream>,
                   path: &str,
                   why: &str|
     -> io::Result<()> {
        not_allowed(stream, door, holder, path, why)?;
        drop_body(reader, head, limit);
        Ok(())
    };
    let query = parse_form(&head.query);
    let file = match file_of(head) {
        Ok(file) => file,
        Err(why) => return refused(stream, reader, "", &format!("upload {why}")),
    };
    if door.rules.upload.is_empty() {
        let why = format!(
            "refused. This door offers no upload: {} is not set.",
            crate::settings::DEVICE_UPLOAD
        );
        return refused(stream, reader, &file, &why);
    }
    if let Err(why) = door.rules.upload(&file).and_then(|()| door.no_link(&file)) {
        return refused(stream, reader, &file, &why);
    }
    let expect = match query.get("fingerprint").map(String::as_str) {
        None | Some("") => None,
        Some(print) if is_fingerprint(print) => Some(print.to_string()),
        Some(_) => {
            let why =
                "upload needs fingerprint as the 64 signs that list gives, or none for a new file";
            return refused(stream, reader, &file, why);
        }
    };
    if !head.has_length {
        let why = "upload needs a Content-Length, and takes no chunked body";
        return refused(stream, reader, &file, why);
    }
    let length = head.length as u64;
    if length > limit {
        let why = format!(
            "refused. The new file has {length} bytes, more than the limit of {} MB of a device file",
            limit / 1_000_000
        );
        // Too large to drop in good time: the answer comes, and the rest stays unread.
        not_allowed(stream, door, holder, &file, &why)?;
        drop_body(reader, head, MAX_BODY as u64);
        return Ok(());
    }
    if !count_moved(stream, door, holder, &file, length)? {
        drop_body(reader, head, limit);
        return Ok(());
    }
    if head.wants_continue {
        stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
    }
    let _ = reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(60)));
    let mut body = vec![0; head.length];
    reader.read_exact(&mut body)?;
    let print = fingerprint(&body);
    let who = device::who(&holder.device, &holder.app);
    let size = head.length.to_string();
    let target = door.for_command(&file);
    let mut args = vec!["upload", "--who", &who, "--size", &size];
    match &expect {
        Some(seen) => args.extend(["--expect", seen.as_str()]),
        None => args.push("--new"),
    }
    args.push(&target);
    let out = serve::start(exe, &args, Some(&body))?;
    let code = out.status.code().unwrap_or(exit::FAILED);
    // 8 and 10: written, with a log line or the clean-up left.
    if code == 0 || code == exit::NOT_LOGGED || code == exit::LOCK_PROBLEM {
        let text = door.tidy(&format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ));
        let answer = json!({"exit": code, "fingerprint": print, "size": length, "text": text});
        return serve::respond_json(stream, "200 OK", &answer);
    }
    not_done(stream, code, &said(door, &out))
}

/// The result of a command, as the other doors give it. The text of a file
/// that was read stays exactly as it is; any other text is tidied.
fn tidied(door: &DeviceDoor, tool: Option<&str>, run: io::Result<std::process::Output>) -> Value {
    let mut value = serve::result(tool, run);
    let refused = value["isError"].as_bool().unwrap_or(true);
    if tool == Some("read") && !refused {
        return value;
    }
    if let Some(text) = value["content"][0]["text"].as_str() {
        value["content"][0]["text"] = Value::String(door.tidy(text));
    }
    value
}

/// The text of a list, with only the folder names left in the groups that
/// `headers` marks, such as `/books/:` for the rule `/books/*/`. A file directly
/// in such a folder never shows, not even its name. A header is a line that is
/// exactly one of the folders asked for, then `:`.
fn folders_only(text: &str, headers: &[(String, Listing)]) -> String {
    let mut listing = Listing::All;
    let mut out = String::new();
    for line in text.split_inclusive('\n') {
        let bare = line.trim_end_matches('\n');
        if let Some((_, next)) = headers.iter().find(|(h, _)| h == bare) {
            listing = *next;
            out.push_str(line);
            continue;
        }
        let keep = listing == Listing::All
            || bare.is_empty()
            || bare.ends_with('/')
            || bare.starts_with("more entries not shown");
        if keep {
            out.push_str(line);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_above_a_star_keeps_only_the_folder_names() {
        let text = "/books/:\n      -  -  an-invented-book/\n  120  Tue 2026-11-17 10:00  an-invented-book.epub\n    9  Tue 2026-11-17 10:00  odd-name:\n\n/books/an-invented-book/:\n   40  Tue 2026-11-17 10:00  chapter-01.txt\n";
        let headers = [
            ("/books/:".to_string(), Listing::FoldersOnly),
            ("/books/an-invented-book/:".to_string(), Listing::All),
        ];
        let kept = folders_only(text, &headers);
        assert!(kept.contains("an-invented-book/\n"));
        assert!(!kept.contains(".epub"), "{kept}");
        assert!(!kept.contains("odd-name"), "{kept}");
        assert!(
            kept.contains("chapter-01.txt"),
            "another group keeps all: {kept}"
        );
        let all = [("/books/:".to_string(), Listing::All)];
        assert_eq!(folders_only(text, &all), text);
    }
}
