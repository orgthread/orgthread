//! The door for a chat with no shell: MCP over standard input and output.
//!
//! Each tool call starts the command itself, `orgthread read` or `orgthread write`.
//! So there is no second copy of the rules: the lock, the checks, the log and every
//! refusal are the ones of the command.

use crate::login::{Login, Refused};
use crate::search;
use crate::transfer::{self, Direction, Links, Taken};
use crate::write::exit;
use serde_json::{Value, json};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

/// Used only when a client names no version of the protocol.
const VERSION: &str = "2025-06-18";

const INVALID_REQUEST: i32 = -32600;
const NO_METHOD: i32 = -32601;
const INVALID_PARAMS: i32 = -32602;
const NOT_JSON: i32 = -32700;

/// Answers one message per line until the input ends.
pub fn run(mut input: impl BufRead, mut output: impl Write, exe: &Path) -> io::Result<()> {
    let mut line = Vec::new();
    loop {
        line.clear();
        if input.read_until(b'\n', &mut line)? == 0 {
            return Ok(());
        }
        if line.trim_ascii().is_empty() {
            continue;
        }
        let answer = match serde_json::from_slice(&line) {
            Ok(message) => answer(&message, exe, None),
            Err(e) => Some(problem(Value::Null, NOT_JSON, &format!("not JSON: {e}"))),
        };
        if let Some(answer) = answer {
            writeln!(output, "{answer}")?;
            output.flush()?;
        }
    }
}

/// The door for a chat in the cloud: the same answers over HTTP, behind a login.
/// It speaks plain HTTP, so a program that handles TLS must stand in front. There is
/// no session and no stream: each POST gets one JSON answer, which the Streamable
/// HTTP form of MCP allows.
pub fn run_http(addresses: &[SocketAddr], login: &Login, exe: &Path) -> io::Result<()> {
    let listener = TcpListener::bind(addresses)?;
    // The links live on the host alone: a chat shows each command it runs, and
    // a link with the secret path in it would give that path away.
    let resource = login.resource();
    let after = resource.strip_prefix("https://").unwrap_or(&resource);
    let host = after.split('/').next().unwrap_or(after);
    let remote = Remote {
        links: Links::default(),
        base: format!("https://{host}"),
    };
    let remote = &remote;
    std::thread::scope(|scope| {
        for stream in listener.incoming().flatten() {
            scope.spawn(move || {
                let _ = handle(stream, login, exe, remote);
            });
        }
    });
    Ok(())
}

/// What only the remote door has: the one-time links of download and upload.
/// A chat on the same machine has the disk, and no address for a link.
pub struct Remote {
    links: Links,
    /// The scheme and host of the public address, with no path.
    base: String,
}

/// What `address` means, if it reaches this machine alone: an IP address of the
/// machine itself, or the exact name `localhost`. Any other name is `None` with no
/// lookup, as what it points at can change. The caller binds what this returns, so
/// the address checked is the address used.
pub fn local_addresses(address: &str) -> Option<Vec<SocketAddr>> {
    if address.parse::<SocketAddr>().is_err() && !address.starts_with("localhost:") {
        return None;
    }
    let found: Vec<SocketAddr> = address.to_socket_addrs().ok()?.collect();
    let all_local = !found.is_empty() && found.iter().all(|a| a.ip().is_loopback());
    all_local.then_some(found)
}

const MAX_BODY: usize = 64 * 1024 * 1024;
const JSON_TYPE: &str = "application/json";

fn respond(stream: &mut TcpStream, status: &str, extra: &str, body: &[u8]) -> io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)
}

fn respond_json(stream: &mut TcpStream, status: &str, value: &Value) -> io::Result<()> {
    let extra = format!("Content-Type: {JSON_TYPE}\r\n");
    respond(stream, status, &extra, value.to_string().as_bytes())
}

fn respond_page(stream: &mut TcpStream, status: &str, page: &str) -> io::Result<()> {
    let extra = "Content-Type: text/html; charset=utf-8\r\nX-Frame-Options: DENY\r\n";
    respond(stream, status, extra, page.as_bytes())
}

fn handle(mut stream: TcpStream, login: &Login, exe: &Path, remote: &Remote) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    // A client that stops reading must not hold a thread for ever.
    stream.set_write_timeout(Some(Duration::from_secs(60)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut first = String::new();
    reader.by_ref().take(16384).read_line(&mut first)?;
    let mut parts = first.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut length = 0usize;
    let mut has_length = false;
    let mut wants_continue = false;
    let mut bearer = String::new();
    loop {
        let mut line = String::new();
        if reader.by_ref().take(16384).read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap_or(0);
                has_length = true;
            } else if name.eq_ignore_ascii_case("expect") {
                wants_continue = value.trim().eq_ignore_ascii_case("100-continue");
            } else if name.eq_ignore_ascii_case("authorization") {
                bearer = value
                    .trim()
                    .strip_prefix("Bearer ")
                    .unwrap_or("")
                    .to_string();
            }
        }
    }
    // A link is the only login of a transfer, and its body can be large, so it
    // is read only once the link is known. Links live outside the secret path.
    let prefix = login.prefix();
    if let Some(secret) = path.strip_prefix("/t/") {
        let request = Transfer {
            method,
            secret,
            length: has_length.then_some(length),
            wants_continue,
        };
        return transfer(&mut stream, &mut reader, &request, remote, exe);
    }
    if length > MAX_BODY {
        return respond(&mut stream, "413 Content Too Large", "", b"");
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let text = String::from_utf8_lossy(&body);

    // The standards also look for these two with the path behind the fixed part.
    if method == "GET" && path == format!("/.well-known/oauth-authorization-server{prefix}") {
        return respond_json(&mut stream, "200 OK", &login.server_metadata());
    }
    if method == "GET" && path == format!("/.well-known/oauth-protected-resource{prefix}/mcp") {
        return respond_json(&mut stream, "200 OK", &login.resource_metadata());
    }
    // A wrong address learns nothing, not even that this is the right server.
    let Some(path) = path.strip_prefix(prefix) else {
        return respond(&mut stream, "404 Not Found", "", b"");
    };

    match (method, path) {
        (
            "GET",
            "/.well-known/oauth-protected-resource" | "/.well-known/oauth-protected-resource/mcp",
        ) => respond_json(&mut stream, "200 OK", &login.resource_metadata()),
        ("GET", "/.well-known/oauth-authorization-server") => {
            respond_json(&mut stream, "200 OK", &login.server_metadata())
        }
        ("POST", "/register") => {
            let given = serde_json::from_slice(&body).unwrap_or(Value::Null);
            match login.register(&given) {
                Ok(client) => respond_json(&mut stream, "201 Created", &client),
                Err(e) => respond_json(
                    &mut stream,
                    "400 Bad Request",
                    &json!({"error": "invalid_redirect_uri", "error_description": e}),
                ),
            }
        }
        ("GET", "/authorize") => match login.page(query, "Enter your orgthread password.") {
            Ok(page) => respond_page(&mut stream, "200 OK", &page),
            Err(e) => respond_page(&mut stream, "400 Bad Request", &e),
        },
        ("POST", "/authorize") => match login.approve(&text) {
            Ok(back) => {
                let extra = format!("Location: {back}\r\n");
                respond(&mut stream, "302 Found", &extra, b"")
            }
            Err(Refused::WrongPassword) => {
                let page = login
                    .page(&text, "That password is wrong. Try again.")
                    .unwrap_or_default();
                respond_page(&mut stream, "401 Unauthorized", &page)
            }
            Err(Refused::Locked) => respond_page(
                &mut stream,
                "429 Too Many Requests",
                "Too many wrong passwords. Try again in 15 minutes.",
            ),
            Err(Refused::BadRequest(e)) => respond_page(&mut stream, "400 Bad Request", &e),
        },
        ("POST", "/token") => match login.token(&text) {
            Ok(tokens) => respond_json(&mut stream, "200 OK", &tokens),
            Err(word) => respond_json(&mut stream, "400 Bad Request", &json!({"error": word})),
        },
        (_, "/mcp") => {
            if !login.allows(&bearer) {
                let extra = format!("WWW-Authenticate: {}\r\n", login.challenge_header());
                return respond(&mut stream, "401 Unauthorized", &extra, b"");
            }
            if method != "POST" {
                return respond(&mut stream, "405 Method Not Allowed", "", b"");
            }
            let answered = match serde_json::from_slice::<Value>(&body) {
                Ok(message) => answer(&message, exe, Some(remote)),
                Err(e) => Some(problem(Value::Null, NOT_JSON, &e.to_string())),
            };
            match answered {
                Some(reply) => respond_json(&mut stream, "200 OK", &reply),
                // A notification wants no answer.
                None => respond(&mut stream, "202 Accepted", "", b""),
            }
        }
        _ => respond(&mut stream, "404 Not Found", "", b""),
    }
}

fn problem(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// The reply to one message. A message with no id, or with no method, wants none.
fn answer(message: &Value, exe: &Path, remote: Option<&Remote>) -> Option<Value> {
    if !message.is_object() {
        let text = "a message must be one JSON object";
        return Some(problem(Value::Null, INVALID_REQUEST, text));
    }
    let id = message.get("id")?.clone();
    let method = message.get("method")?;
    let params = message.get("params").unwrap_or(&Value::Null);
    let result = match method.as_str() {
        Some("initialize") => Ok(greeting(params)),
        Some("ping") => Ok(json!({})),
        Some("tools/list") => Ok(tools(remote.is_some())),
        Some("tools/call") => call(params, exe, remote),
        _ => Err((NO_METHOD, format!("there is no method {method}"))),
    };
    Some(match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err((code, text)) => problem(id, code, &text),
    })
}

fn greeting(params: &Value) -> Value {
    // Only tools are offered, and they read the same in every version so far.
    let version = params["protocolVersion"].as_str().unwrap_or(VERSION);
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "orgthread", "version": env!("CARGO_PKG_VERSION")},
    })
}

fn tools(remote: bool) -> Value {
    let file = json!({"type": "string", "description": "The full path of the file. On a server, from the root of the Dropbox: /org/a.md"});
    let who = json!({
        "type": "string",
        "description": "The name of this chat, the same one each time"
    });
    let mut tools = json!({"tools": [
        {
            "name": "read",
            "description": "Gives the exact text of a file, and notes what this name read.",
            "inputSchema": {
                "type": "object",
                "properties": {"file": file, "who": who},
                "required": ["file", "who"],
            },
        },
        {
            "name": "write",
            "description": "Replaces a whole file with the text. To change a file, read it \
                first with the same name. A new file needs no read. A write is refused when \
                the file changed since. Missing folders of the file are made, and the answer \
                names them. Where the owner fixed the first level, a new folder there is \
                refused.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": file,
                    "who": who,
                    "text": {"type": "string", "description": "The whole new text"},
                },
                "required": ["file", "who", "text"],
            },
        },
        {
            "name": "patch",
            "description": "Changes part of a file, so that a large file need not be sent whole. \
                Each edit replaces its exact old text, whole lines with their newlines, by its \
                new text. The old text must occur once. If it occurs more than once, add a line \
                before or after it that differs, such as the heading of the section, or give the \
                line where it starts. append adds lines at the end. All edits are applied, or \
                none. Read the file first with the same name; a change elsewhere in the file \
                needs no new read. A task that may send the same patch twice puts new lines \
                into an edit, with the lines before them as old text, not into append.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": file,
                    "who": who,
                    "edits": {
                        "type": "array",
                        "description": "The changes. An empty new text deletes the old text",
                        "items": {
                            "type": "object",
                            "properties": {
                                "old": {"type": "string", "description": "The exact old text, whole lines with their newlines"},
                                "new": {"type": "string", "description": "The new text"},
                                "line": {"type": "integer", "minimum": 1, "description": "Only when the old text occurs more than once: the line where it starts"},
                            },
                            "required": ["old", "new"],
                        },
                    },
                    "append": {"type": "string", "description": "Lines to add at the end of the file"},
                },
                "required": ["file", "who"],
            },
        },
        {
            "name": "delete",
            "description": "Deletes one file, never a folder, and only the version this name \
                saw: read it first with the same name, or give its fingerprint as expect, \
                from list with fingerprint. A delete is refused when the file changed since, \
                and outside the folders the owner opened for deletes. There is a way back: \
                Dropbox keeps the file in Deleted files, and a local door keeps the last \
                version. With check, it only checks the rules and the version, and deletes \
                nothing.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": file,
                    "who": who,
                    "expect": {"type": "string", "description": "The fingerprint of the version to delete, when this name has not read it"},
                    "check": {"type": "boolean", "description": "true only checks, and deletes nothing"},
                },
                "required": ["file", "who"],
            },
        },
        {
            "name": "copy",
            "description": "Copies files to new names, with their exact bytes, in the same \
                storage. Each pair names a source, from, and a new name, to. Up to 25 pairs in \
                one call. For more, call again with the rest. A copy never replaces a file: if \
                anything is at the new name, that pair is refused as existing. Missing \
                folders of the new name are made, and the answer names them. The rules of \
                read hold for the source and \
                the rules of write for the new name. A file that is not text may go only to \
                the folders that the owner allowed; on a server only the names tell this. With \
                fingerprint, from list with fingerprint, a source of another version is \
                refused as changed. Each pair gives one line: copied, with the size, \
                the fingerprint and any new folders, or refused with its exit code and the reason. The pairs that \
                passed stay copied.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pairs": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": MAX_FILES,
                        "items": {
                            "type": "object",
                            "properties": {
                                "from": {"type": "string", "description": "The full path of the source. On a server, from the root of the Dropbox: /org/a.md"},
                                "to": {"type": "string", "description": "The full path of the new file, which must not exist yet"},
                                "fingerprint": {"type": "string", "description": "Only when the source must be one version: its fingerprint"},
                            },
                            "required": ["from", "to"],
                        },
                    },
                    "who": who,
                },
                "required": ["pairs", "who"],
            },
        },
        {
            "name": "move",
            "description": "Moves files to new names in the same storage. A rename is a move \
                in the same folder. Each pair names a file, from, and its new name, to. Up to \
                100 pairs in one call. For more, call again with the rest. The pairs run one by \
                one, in order. A move never replaces a file: if anything is at the new name, \
                that pair is refused as existing. Missing folders of the new name are made, \
                and the answer names them. Only files move, never a folder. The rules of write \
                hold for both names, and where the owner named the folders open for moves, \
                both names must lie in them. A file that is not text may go only to the \
                folders that the owner allowed. With fingerprint, from list with fingerprint, \
                a file of another version is refused as changed. Each pair gives one line: \
                moved, with the size, the fingerprint and any new folders, or refused with its \
                exit code and the reason. A renamed Markdown file adds a line that wikilinks to \
                its old name break. The pairs that passed stay moved. The way back is a move \
                from the new name to the old one.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pairs": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": MAX_MOVES,
                        "items": {
                            "type": "object",
                            "properties": {
                                "from": {"type": "string", "description": "The full path of the file. On a server, from the root of the Dropbox: /org/a.md"},
                                "to": {"type": "string", "description": "The full path of its new name, which must not exist yet"},
                                "fingerprint": {"type": "string", "description": "Only when the file must be one version: its fingerprint"},
                            },
                            "required": ["from", "to"],
                        },
                    },
                    "who": who,
                },
                "required": ["pairs", "who"],
            },
        },
        {
            "name": "list",
            "description": "Lists the files and folders in one or more folders: one group per \
                folder, and per entry its size in bytes, its time and its name. A folder name \
                ends with /. To read a file, join the header of its group and its name. One \
                level by default. Names that start with a dot are hidden. With fingerprint, also \
                the fingerprint of each file, the content hash that Dropbox reports, before \
                its name.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "folders": {
                        "type": "array",
                        "items": {"type": "string"},
                        "minItems": 1,
                        "description": "The full paths of the folders. On a server, from the root of the Dropbox: /org/notes",
                    },
                    "depth": {
                        "oneOf": [
                            {"type": "integer", "minimum": 1},
                            {"type": "string", "enum": ["all"]},
                        ],
                        "description": "How many levels down: 1 by default, or all",
                    },
                    "all": {
                        "type": "boolean",
                        "description": "true also shows the names that start with a dot",
                    },
                    "fingerprint": {
                        "type": "boolean",
                        "description": "true also shows the fingerprint of each file",
                    },
                },
                "required": ["folders"],
            },
        },
        {
            "name": "search",
            "description": "Finds a text in the files of one folder and all its folders. Plain \
                text, case ignored. A hit is path:line: text, with the line cut at 300 \
                characters. With names, it matches only the names of files and folders, and a \
                hit is the path. 100 hits by default, max up to 1000, and the last line says \
                how many more were found. Closed paths stay out, read-only hits are marked \
                (read only), names that start with a dot are skipped, and files that are not \
                text are skipped for content. No hits gives the answer \"no hits in\" and the \
                folder.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": {"type": "string", "description": "The text to find, on one line"},
                    "folder": {"type": "string", "description": "The full path of the folder. On a server, from the root of the Dropbox: /org/notes"},
                    "names": {
                        "type": "boolean",
                        "description": "true matches only the names of files and folders",
                    },
                    "max": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": search::MOST,
                        "description": "The most hits to show, from 1 to 1000: 100 by default",
                    },
                    "all": {
                        "type": "boolean",
                        "description": "true also searches the names that start with a dot",
                    },
                    "who": who,
                },
                "required": ["text", "folder", "who"],
            },
        },
    ]});
    if let Some(list) = tools["tools"].as_array_mut() {
        if remote {
            list.extend(transfer_tools());
        }
        list.push(json!({
            "name": "version",
            "description": "Gives the version and the commit of this server, which door \
                answers, local or remote, the kind of storage, and the names of the tools it \
                gives now. If a name is missing from your own list of tools, ask the owner \
                to refresh it. Needs nothing.",
            "inputSchema": {"type": "object", "properties": {}},
        }));
    }
    tools
}

/// The answer of the tool `version`. It is built in this process, so it names the
/// build that answers, also when a newer program waits on the disk for a restart.
/// Each line is fixed at build time or one word from a short list, so no setting,
/// path, address or limit can reach a chat through it.
fn version(remote: bool) -> Value {
    let door = if remote { "remote" } else { "local" };
    // The same test as the commands make, which read the same environment.
    let dropbox = std::env::var("ORGTHREAD_STORAGE").is_ok_and(|v| v == "dropbox");
    let storage = crate::storage::kind(dropbox);
    let offered = tools(remote);
    let names: Vec<&str> = offered["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    let text = format!(
        "orgthread {}\ndoor: {door}\nstorage: {storage}\ntools: {}\n",
        crate::VERSION,
        names.join(", ")
    );
    json!({"content": [{"type": "text", "text": text}], "isError": false})
}

fn transfer_tools() -> [Value; 2] {
    let mb = |limit: Result<u64, String>| limit.map(transfer::megabytes).unwrap_or_default();
    let files = json!({
        "type": "array",
        "items": {"type": "string"},
        "minItems": 1,
        "maxItems": MAX_FILES,
        "description": "The full paths of the files. On a server, from the root of the Dropbox: /org/a.pdf",
    });
    let who = json!({
        "type": "string",
        "description": "The name of this chat, the same one each time"
    });
    [
        json!({
            "name": "download",
            "description": format!("Gives a one-time link for each file, to fetch its exact bytes \
                into your code sandbox: curl -sS -o <name> '<link>'. For any file, also not \
                text, up to {}. Each line gives the path, the size and the link, or why the file \
                is refused. Up to 25 files in one call. For more, call again with the rest. A \
                link works once and must start within 5 minutes, so use it soon. Never show, \
                store or log a link. Notes what this name read, as read does. {}",
                mb(transfer::download_limit()), BLOCKED),
            "inputSchema": {
                "type": "object",
                "properties": {"files": files, "who": who},
                "required": ["files", "who"],
            },
        }),
        json!({
            "name": "upload",
            "description": format!("Gives a one-time link for each file, to send it from your \
                code sandbox: curl -sS -T <file> '<link>'. curl then prints the answer: written, \
                or exit code N and the refusal. The rules of write hold: to replace a file, \
                download or read it first with the same name. A file that is not text may go \
                only to the folders that the owner allowed. Up to {}. Up to 25 files in one \
                call. For more, call again with the rest. A link works once and must start \
                within 5 minutes, so use it soon. Missing folders of the file are made, as for \
                write. Never show, store or log a link. {}",
                mb(transfer::upload_limit()), BLOCKED),
            "inputSchema": {
                "type": "object",
                "properties": {"files": files, "who": who},
                "required": ["files", "who"],
            },
        }),
    ]
}

/// The answer of the claude.ai proxy when the host of this door is not on its
/// list. It never reaches the door, so only the tool description can explain it.
const BLOCKED: &str = "If curl says CONNECT tunnel failed, response 403, the request \
    never reached this server: the sandbox may reach only the hosts its owner allowed. \
    Ask the owner to add the host of this server on claude.ai, under Settings, \
    Capabilities, Allowed websites. An empty answer with HTTP 404 means that the link \
    was used already: ask the tool for a new one.";

/// At most this many files in one call of download or upload, and pairs of copy.
const MAX_FILES: usize = 25;

/// At most this many pairs in one call of move. A move carries two names and
/// no bytes, so it needs no link that must start in time, as a download does.
const MAX_MOVES: usize = 100;

/// One link for each file, after the same checks as the command makes. A file
/// that is refused gets its reason, and the others still get their links.
fn links_for(
    params: &Value,
    exe: &Path,
    remote: &Remote,
    direction: Direction,
) -> Result<Value, (i32, String)> {
    let tool = match direction {
        Direction::Download => "download",
        Direction::Upload => "upload",
    };
    let wrong = |what: &str| (INVALID_PARAMS, format!("{tool} needs {what}"));
    let arguments = &params["arguments"];
    let who = arguments["who"]
        .as_str()
        .ok_or_else(|| wrong("who, as text"))?;
    let files = arguments["files"]
        .as_array()
        .filter(|f| (1..=MAX_FILES).contains(&f.len()))
        .ok_or_else(|| wrong("files, as a list of 1 to 25 paths"))?;
    let mut lines = Vec::new();
    let mut first_failed = None;
    for file in files {
        let file = file.as_str().ok_or_else(|| wrong("each file as text"))?;
        let checked = start(exe, &[tool, "--check", "--who", who, file], None);
        let line = match checked {
            Ok(out) if out.status.success() => match remote.links.make(direction, file, who) {
                Ok(secret) => {
                    let link = format!("{}/t/{secret}", remote.base);
                    match direction {
                        Direction::Download => {
                            let size = String::from_utf8_lossy(&out.stdout);
                            format!("{file}  {} bytes  {link}", size.trim())
                        }
                        Direction::Upload => format!("{file}  {link}"),
                    }
                }
                Err(e) => {
                    first_failed.get_or_insert(exit::FAILED);
                    format!("{file}  refused, exit code {}: {e}", exit::FAILED)
                }
            },
            Ok(out) => {
                let code = out.status.code().unwrap_or(exit::FAILED);
                first_failed.get_or_insert(code);
                let said = String::from_utf8_lossy(&out.stderr);
                let reason = said.lines().next().unwrap_or("");
                let reason = reason.strip_prefix("orgthread: ").unwrap_or(reason);
                format!("{file}  refused, exit code {code}: {reason}")
            }
            Err(e) => {
                first_failed.get_or_insert(exit::FAILED);
                format!(
                    "{file}  refused, exit code {}: could not start the command: {e}",
                    exit::FAILED
                )
            }
        };
        lines.push(line);
    }
    let mut text = lines.join("\n") + "\n";
    if let Some(code) = first_failed {
        text = format!("exit code {code}\n{text}");
    }
    Ok(json!({"content": [{"type": "text", "text": text}], "isError": first_failed.is_some()}))
}

fn call(params: &Value, exe: &Path, remote: Option<&Remote>) -> Result<Value, (i32, String)> {
    let name = params["name"].as_str();
    let text_of = |key: &str| {
        params["arguments"][key].as_str().ok_or_else(|| {
            let tool = name.unwrap_or_default();
            (INVALID_PARAMS, format!("{tool} needs {key}, as text"))
        })
    };
    // The file comes last. Named like an option, it fails as the command's wrong use.
    let (args, text) = match name {
        Some("read") => (
            vec!["read", "--who", text_of("who")?, text_of("file")?],
            None,
        ),
        Some("write") => {
            let text = text_of("text")?;
            // With the size, a server stopped halfway cannot leave a cut-off text.
            let size = text.len().to_string();
            let (who, file) = (text_of("who")?, text_of("file")?);
            let args = vec!["write", "--who", who, "--size", size.as_str(), file];
            return Ok(result(name, start(exe, &args, Some(text.as_bytes()))));
        }
        Some("patch") => {
            let arguments = &params["arguments"];
            let mut patch = serde_json::Map::new();
            for key in ["edits", "append"] {
                if !arguments[key].is_null() {
                    patch.insert(key.to_string(), arguments[key].clone());
                }
            }
            // The command checks the patch itself, so the refusals are the same.
            let text = Value::Object(patch).to_string();
            let size = text.len().to_string();
            let (who, file) = (text_of("who")?, text_of("file")?);
            let args = vec!["patch", "--who", who, "--size", size.as_str(), file];
            return Ok(result(name, start(exe, &args, Some(text.as_bytes()))));
        }
        Some(tool @ ("download" | "upload")) => {
            let Some(remote) = remote else {
                return Err((INVALID_PARAMS, format!("there is no tool {tool}")));
            };
            let direction = if tool == "download" {
                Direction::Download
            } else {
                Direction::Upload
            };
            return links_for(params, exe, remote, direction);
        }
        Some("version") => return Ok(version(remote.is_some())),
        Some("delete") => {
            let arguments = &params["arguments"];
            let mut args = vec!["delete", "--who", text_of("who")?];
            match &arguments["expect"] {
                Value::Null => {}
                Value::String(print) => args.extend(["--expect", print.as_str()]),
                _ => return Err((INVALID_PARAMS, "delete needs expect, as text".into())),
            }
            match &arguments["check"] {
                Value::Null | Value::Bool(false) => {}
                Value::Bool(true) => args.push("--check"),
                _ => return Err((INVALID_PARAMS, "delete needs check as true or false".into())),
            }
            args.push(text_of("file")?);
            return Ok(result(name, start(exe, &args, None)));
        }
        Some("copy") => return run_pairs(params, exe, "copy", MAX_FILES),
        Some("move") => return run_pairs(params, exe, "move", MAX_MOVES),
        Some("search") => {
            let owned = search_args(params)?;
            let args: Vec<&str> = owned.iter().map(String::as_str).collect();
            return Ok(result(name, start(exe, &args, None)));
        }
        Some("list") => {
            let owned = list_args(params)?;
            let args: Vec<&str> = owned.iter().map(String::as_str).collect();
            return Ok(result(name, start(exe, &args, None)));
        }
        _ => {
            return Err((
                INVALID_PARAMS,
                format!("there is no tool {}", params["name"]),
            ));
        }
    };
    Ok(result(name, start(exe, &args, text)))
}

/// One `orgthread copy` or `orgthread move` for each pair, in order. A pair that
/// is refused gets its line with the exit code, and the next pairs still run. The
/// answer starts with the code of the first pair that failed.
fn run_pairs(params: &Value, exe: &Path, tool: &str, most: usize) -> Result<Value, (i32, String)> {
    let wrong = |what: &str| (INVALID_PARAMS, format!("{tool} needs {what}"));
    let arguments = &params["arguments"];
    let who = arguments["who"]
        .as_str()
        .ok_or_else(|| wrong("who, as text"))?;
    let pairs = arguments["pairs"]
        .as_array()
        .filter(|p| (1..=most).contains(&p.len()))
        .ok_or_else(|| {
            wrong(&format!(
                "pairs, as a list of 1 to {most} pairs of from and to"
            ))
        })?;
    // Every pair is checked before the first one runs, so wrong use changes nothing.
    let mut runs = Vec::new();
    for pair in pairs {
        let from = pair["from"]
            .as_str()
            .ok_or_else(|| wrong("from in each pair, as text"))?;
        let to = pair["to"]
            .as_str()
            .ok_or_else(|| wrong("to in each pair, as text"))?;
        let print = match &pair["fingerprint"] {
            Value::Null => None,
            Value::String(print) => Some(print.as_str()),
            _ => return Err(wrong("fingerprint, as text")),
        };
        runs.push((from, to, print));
    }
    let mut lines = Vec::new();
    let mut first_failed = None;
    for (from, to, print) in runs {
        let mut args = vec![tool, "--who", who];
        if let Some(print) = print {
            args.extend(["--fingerprint", print]);
        }
        // The paths come after --, so a path named like an option stays a path.
        args.extend(["--", from, to]);
        let line = match start(exe, &args, None) {
            Ok(out) if out.status.success() => {
                String::from_utf8_lossy(&out.stdout).trim_end().to_string()
            }
            Ok(out) => {
                let code = out.status.code().unwrap_or(exit::FAILED);
                first_failed.get_or_insert(code);
                let said = String::from_utf8_lossy(&out.stderr);
                let reason = said.lines().next().unwrap_or("");
                let reason = reason.strip_prefix("orgthread: ").unwrap_or(reason);
                format!("{from} -> {to}  refused, exit code {code}: {reason}")
            }
            Err(e) => {
                first_failed.get_or_insert(exit::FAILED);
                format!(
                    "{from} -> {to}  refused, exit code {}: could not start the command: {e}",
                    exit::FAILED
                )
            }
        };
        lines.push(line);
    }
    let mut text = lines.join("\n") + "\n";
    if let Some(code) = first_failed {
        text = format!("exit code {code}\n{text}");
    }
    Ok(json!({"content": [{"type": "text", "text": text}], "isError": first_failed.is_some()}))
}

/// The command line of a list. The folders come after `--`, so a folder named
/// like an option stays a folder.
fn list_args(params: &Value) -> Result<Vec<String>, (i32, String)> {
    let wrong = |what: &str| (INVALID_PARAMS, format!("list needs {what}"));
    let arguments = &params["arguments"];
    let folders = arguments["folders"]
        .as_array()
        .filter(|f| !f.is_empty())
        .ok_or_else(|| wrong("folders, as a list of paths"))?;
    let mut args = vec!["list".to_string()];
    if !arguments["depth"].is_null() {
        match loose(&arguments["depth"]).as_deref() {
            Some("all") => args.extend(["--depth".into(), "all".into()]),
            Some(n) if n.parse::<u64>().is_ok_and(|n| n >= 1) => {
                args.extend(["--depth".into(), n.to_string()]);
            }
            _ => return Err(wrong("depth as a number from 1, or \"all\"")),
        }
    }
    match &arguments["all"] {
        Value::Null | Value::Bool(false) => {}
        Value::Bool(true) => args.push("--all".into()),
        _ => return Err(wrong("all as true or false")),
    }
    match &arguments["fingerprint"] {
        Value::Null | Value::Bool(false) => {}
        Value::Bool(true) => args.push("--fingerprint".into()),
        _ => return Err(wrong("fingerprint as true or false")),
    }
    args.push("--".into());
    for folder in folders {
        let folder = folder
            .as_str()
            .ok_or_else(|| wrong("each folder as text"))?;
        args.push(folder.to_string());
    }
    Ok(args)
}

/// The plain value of a field that holds a number or a word, such as `depth`
/// or `max`. Some chats send every number as text, `"2"`, or put the JSON of
/// the value in the text, with its quotes: `"\"all\""`. Both count as the plain
/// value. A whole number may also come as `2.0`. `None` for any other kind.
fn loose(value: &Value) -> Option<String> {
    match value {
        Value::Number(n) => n.as_u64().map(|n| n.to_string()).or_else(|| {
            n.as_f64()
                .filter(|f| f.fract() == 0.0 && (0.0..=u32::MAX as f64).contains(f))
                .map(|f| (f as u64).to_string())
        }),
        Value::String(text) => Some(text.trim().trim_matches('"').trim().to_string()),
        _ => None,
    }
}

/// The command line of a search. The text and the folder come after `--`, so a
/// text named like an option stays the text. `max` may come as a number or as a
/// number in text, through `loose`. The messages never repeat the text.
fn search_args(params: &Value) -> Result<Vec<String>, (i32, String)> {
    let wrong = |what: &str| (INVALID_PARAMS, format!("search needs {what}"));
    let arguments = &params["arguments"];
    let text_of = |key: &str| {
        arguments[key]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| wrong(&format!("{key}, as text")))
    };
    let (text, folder, who) = (text_of("text")?, text_of("folder")?, text_of("who")?);
    let mut args = vec!["search".to_string(), "--who".to_string(), who];
    let most = search::MOST;
    if !arguments["max"].is_null() {
        match loose(&arguments["max"]).and_then(|n| n.parse::<u64>().ok()) {
            Some(n) if (1..=most as u64).contains(&n) => {
                args.extend(["--max".into(), n.to_string()]);
            }
            _ => return Err(wrong(&format!("max as a number from 1 to {most}"))),
        }
    }
    for flag in ["names", "all"] {
        match &arguments[flag] {
            Value::Null | Value::Bool(false) => {}
            Value::Bool(true) => args.push(format!("--{flag}")),
            _ => return Err(wrong(&format!("{flag} as true or false"))),
        }
    }
    args.extend(["--".into(), text, folder]);
    Ok(args)
}

fn start(exe: &Path, args: &[&str], text: Option<&[u8]>) -> io::Result<Output> {
    let mut child = Command::new(exe)
        .args(args)
        .stdin(if text.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let (Some(text), Some(mut stdin)) = (text, child.stdin.take()) {
        // A command that refuses early closes the pipe. Its exit code says why.
        let _ = stdin.write_all(text);
    }
    child.wait_with_output()
}

fn result(tool: Option<&str>, run: io::Result<Output>) -> Value {
    let (text, refused) = match run {
        Err(e) => (format!("could not start the command: {e}"), true),
        Ok(out) if out.status.success() && tool == Some("read") => {
            match String::from_utf8(out.stdout) {
                Ok(text) => (text, false),
                Err(e) => (format!("the text is not valid UTF-8: {e}"), true),
            }
        }
        Ok(out) => {
            let said = String::from_utf8_lossy(&out.stdout);
            let warned = String::from_utf8_lossy(&out.stderr);
            match out.status.code() {
                Some(0) => (format!("{said}{warned}"), false),
                Some(code) => (format!("exit code {code}\n{said}{warned}"), true),
                None => (format!("the command was stopped\n{said}{warned}"), true),
            }
        }
    };
    json!({"content": [{"type": "text", "text": text}], "isError": refused})
}

/// One request at a link: `GET` fetches a file, `PUT` sends one.
struct Transfer<'a> {
    method: &'a str,
    secret: &'a str,
    length: Option<usize>,
    wants_continue: bool,
}

fn respond_text(stream: &mut TcpStream, status: &str, text: &str) -> io::Result<()> {
    let extra = "Content-Type: text/plain; charset=utf-8\r\n";
    respond(stream, status, extra, text.as_bytes())
}

/// The answer of a command, as the tools give it: its text, and `exit code N`
/// first when it refused.
fn said_by(run: io::Result<Output>) -> (&'static str, String) {
    match run {
        Err(e) => (
            "500 Internal Server Error",
            format!("could not start the command: {e}\n"),
        ),
        Ok(out) => {
            let said = String::from_utf8_lossy(&out.stdout);
            let warned = String::from_utf8_lossy(&out.stderr);
            match out.status.code() {
                Some(0) => ("200 OK", format!("{said}{warned}")),
                Some(code) => ("409 Conflict", format!("exit code {code}\n{said}{warned}")),
                None => (
                    "500 Internal Server Error",
                    format!("the command was stopped\n{said}{warned}"),
                ),
            }
        }
    }
}

fn transfer(
    stream: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    request: &Transfer,
    remote: &Remote,
    exe: &Path,
) -> io::Result<()> {
    let direction = match request.method {
        "GET" => Direction::Download,
        "PUT" => Direction::Upload,
        _ => {
            let text = "A link takes GET to fetch a file, or PUT to send one.\n";
            return refuse(stream, reader, request, "405 Method Not Allowed", text);
        }
    };
    let (status, text) = match remote.links.take(request.secret, direction) {
        Taken::Ready(link) => {
            return match direction {
                Direction::Download => send_file(stream, &link, exe),
                Direction::Upload => receive_file(stream, reader, request, &link, exe),
            };
        }
        Taken::Expired => (
            "410 Gone",
            "This link waited more than 5 minutes, and it is gone now. Ask the tool for a new one.\n",
        ),
        Taken::WrongDirection => (
            "405 Method Not Allowed",
            "This link is for the other direction: GET fetches with download, PUT sends with upload.\n",
        ),
        // As for any wrong address: the answer says nothing, not even that this
        // is the right server.
        Taken::Unknown => ("404 Not Found", ""),
    };
    refuse(stream, reader, request, status, text)
}

/// The answer to a request that moves nothing. A client that sends at once must
/// not lose it to a reset connection, so what it sends is read and dropped, up to
/// a bound. A client that waits for 100 Continue sends nothing, so the wait is short.
fn refuse(
    stream: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    request: &Transfer,
    status: &str,
    text: &str,
) -> io::Result<()> {
    if text.is_empty() {
        respond(stream, status, "", b"")?;
    } else {
        respond_text(stream, status, text)?;
    }
    if let Some(length) = request.length.filter(|n| *n > 0) {
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        let rest = (length as u64).min(MAX_BODY as u64);
        let _ = io::copy(&mut reader.take(rest), &mut io::sink());
    }
    Ok(())
}

/// The whole file is held in memory: the write procedure needs it, to put the
/// old state back when the check after the write fails. The limit keeps it small.
fn receive_file(
    stream: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    request: &Transfer,
    link: &transfer::Link,
    exe: &Path,
) -> io::Result<()> {
    let Some(length) = request.length else {
        return respond_text(
            stream,
            "411 Length Required",
            "An upload needs a Content-Length. curl -T gives one.\n",
        );
    };
    let limit = match transfer::upload_limit() {
        Ok(limit) => limit,
        Err(e) => {
            let text = format!("exit code {}\n{e}\n", exit::USAGE);
            return refuse(stream, reader, request, "409 Conflict", &text);
        }
    };
    if length as u64 > limit {
        let text = format!(
            "exit code {}\norgthread: refused, nothing written. The new file has more than the limit of {}.\n",
            exit::USAGE,
            transfer::megabytes(limit)
        );
        return refuse(stream, reader, request, "409 Conflict", &text);
    }
    if request.wants_continue {
        stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let size = length.to_string();
    let args = ["upload", "--who", &link.who, "--size", &size, &link.file];
    let (status, text) = said_by(start(exe, &args, Some(&body)));
    respond_text(stream, status, &text)
}

/// Stops a command that is still at work, also when the client went away.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        if let Ok(None) = self.0.try_wait() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

/// The bytes pass through in parts, so a large file never sits whole in memory.
/// They go out in chunks: if the command fails halfway, the last chunk never
/// comes, and curl reports a cut-off transfer instead of a short file.
fn send_file(stream: &mut TcpStream, link: &transfer::Link, exe: &Path) -> io::Result<()> {
    let child = Command::new(exe)
        .args(["download", "--who", &link.who, &link.file])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut running = match child {
        Ok(child) => Running(child),
        Err(e) => {
            let (status, text) = said_by(Err(e));
            return respond_text(stream, status, &text);
        }
    };
    let Some(mut out) = running.0.stdout.take() else {
        return respond_text(
            stream,
            "500 Internal Server Error",
            "the command gave no output\n",
        );
    };
    let mut buffer = vec![0; 64 * 1024];
    let mut n = out.read(&mut buffer)?;
    if n == 0 {
        // Nothing came: an empty file, or a refusal. The exit code says which.
        drop(out);
        let mut warned = String::new();
        if let Some(mut err) = running.0.stderr.take() {
            let _ = err.read_to_string(&mut warned);
        }
        let status = running.0.wait()?;
        return match status.code() {
            Some(0) => respond(
                stream,
                "200 OK",
                "Content-Type: application/octet-stream\r\n",
                b"",
            ),
            Some(code) => respond_text(
                stream,
                "409 Conflict",
                &format!("exit code {code}\n{warned}"),
            ),
            None => respond_text(
                stream,
                "500 Internal Server Error",
                &format!("the command was stopped\n{warned}"),
            ),
        };
    }
    stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nTransfer-Encoding: chunked\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
    )?;
    while n > 0 {
        stream.write_all(format!("{n:x}\r\n").as_bytes())?;
        stream.write_all(&buffer[..n])?;
        stream.write_all(b"\r\n")?;
        n = out.read(&mut buffer)?;
    }
    drop(out);
    if running.0.wait()?.success() {
        stream.write_all(b"0\r\n\r\n")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_tool_with_a_list_names_its_limit_of_25() {
        let offered = tools(true);
        let says = |name: &str| -> String {
            offered["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["name"] == name)
                .unwrap()["description"]
                .as_str()
                .unwrap()
                .to_string()
        };
        for name in ["download", "upload"] {
            assert!(
                says(name)
                    .contains("Up to 25 files in one call. For more, call again with the rest."),
                "{name}: {}",
                says(name)
            );
        }
        assert!(
            says("copy")
                .contains("Up to 25 pairs in one call. For more, call again with the rest.")
        );
        assert_eq!(MAX_FILES, 25);
        assert!(
            says("move")
                .contains("Up to 100 pairs in one call. For more, call again with the rest.")
        );
        assert_eq!(MAX_MOVES, 100);
    }
}
