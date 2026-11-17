//! The door for a device: `orgthread serve --http <address> --device`.
//!
//! It runs as its own process, from its own folder, with its own `.env`, log,
//! locks and file of tokens. A device logs in with a token, not with OAuth. It
//! gets three tools, `list`, `read` and `patch`, and only on the paths of the
//! rules. Each tool call starts the command itself, as on the other doors, so
//! the lock, the checks and the refusals of the command hold as well.
//!
//! No request or response body goes into the log or to the error output: only
//! the time, the word, the path and the device.

use crate::device::{self, Cap, Holder, Rules, Store};
use crate::log;
use crate::serve::{self, INVALID_PARAMS, INVALID_REQUEST, NO_METHOD, NOT_JSON};
use crate::write::exit;
use serde_json::{Value, json};
use std::io::{self, BufReader, Read};
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

/// The settings of a device door, read once at its start.
pub struct DeviceDoor {
    /// The app of this door, such as `reader`.
    pub app: String,
    pub rules: Rules,
    /// The files whose reads count for the cap. `None`: every read counts.
    pub capped: Option<Vec<String>>,
    pub cap: Cap,
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
            store,
            notify,
            log,
            root,
            prefix,
            fails: Mutex::new(Vec::new()),
        }
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
        (method, "/mcp") => {
            let holder = match door.store.holder(&door.app, &head.bearer, device::now()) {
                Ok(Some(holder)) => holder,
                Ok(None) => {
                    let extra = "WWW-Authenticate: Bearer\r\n";
                    return serve::respond(&mut stream, "401 Unauthorized", extra, b"");
                }
                Err(e) => {
                    eprintln!("orgthread: could not read the tokens: {e}");
                    return serve::respond(&mut stream, "500 Internal Server Error", "", b"");
                }
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
        Some("tools/list") => Ok(tools()),
        Some("tools/call") => call(params, exe, door, holder),
        _ => Err((NO_METHOD, format!("there is no method {method}"))),
    };
    Some(match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err((code, text)) => serve::problem(id, code, &text),
    })
}

/// The three tools. There is no `who`: the door names the device itself.
fn tools() -> Value {
    let file = json!({"type": "string", "description": "The full path of the file, from the root: /books/a/chapter-01.txt"});
    json!({"tools": [
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
    ]})
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
            for folder in folders {
                let folder = folder
                    .as_str()
                    .ok_or_else(|| (INVALID_PARAMS, "list needs each folder as text".into()))?;
                if let Err(why) = door.rules.list(folder).and_then(|()| door.no_link(folder)) {
                    return Ok(refuse(door, holder, folder, &why));
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
            Ok(tidied(door, Some("list"), serve::start(exe, &args, None)))
        }
        _ => {
            let path = arguments["file"].as_str().unwrap_or_default();
            let reason = format!(
                "refused. This door offers only {}, and not {}.",
                TOOLS.join(", "),
                device::shown(name)
            );
            Ok(refuse(door, holder, path, &reason))
        }
    }
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
