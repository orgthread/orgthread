//! The door for a chat with no shell: MCP over standard input and output.
//!
//! Each tool call starts the command itself, `orgthread read` or `orgthread write`.
//! So there is no second copy of the rules: the lock, the checks, the log and every
//! refusal are the ones of the command.

use crate::login::{Login, Refused};
use serde_json::{Value, json};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::process::{Command, Output, Stdio};

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
            Ok(message) => answer(&message, exe),
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
    std::thread::scope(|scope| {
        for stream in listener.incoming().flatten() {
            scope.spawn(move || {
                let _ = handle(stream, login, exe);
            });
        }
    });
    Ok(())
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

fn handle(mut stream: TcpStream, login: &Login, exe: &Path) -> io::Result<()> {
    stream.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut first = String::new();
    reader.by_ref().take(16384).read_line(&mut first)?;
    let mut parts = first.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut length = 0usize;
    let mut bearer = String::new();
    loop {
        let mut line = String::new();
        if reader.by_ref().take(16384).read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap_or(0);
            } else if name.eq_ignore_ascii_case("authorization") {
                bearer = value
                    .trim()
                    .strip_prefix("Bearer ")
                    .unwrap_or("")
                    .to_string();
            }
        }
    }
    if length > MAX_BODY {
        return respond(&mut stream, "413 Content Too Large", "", b"");
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let text = String::from_utf8_lossy(&body);

    // The standards also look for these two with the path behind the fixed part.
    let prefix = login.prefix();
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
                Ok(message) => answer(&message, exe),
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
fn answer(message: &Value, exe: &Path) -> Option<Value> {
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
        Some("tools/list") => Ok(tools()),
        Some("tools/call") => call(params, exe),
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

fn tools() -> Value {
    let file = json!({"type": "string", "description": "The full path of the file. On a server, from the root of the Dropbox: /org/a.md"});
    let who = json!({
        "type": "string",
        "description": "The name of this chat, the same one each time"
    });
    json!({"tools": [
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
                the file changed since.",
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
    ]})
}

fn call(params: &Value, exe: &Path) -> Result<Value, (i32, String)> {
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
            return Ok(result(name, start(exe, &args, Some(text))));
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

fn start(exe: &Path, args: &[&str], text: Option<&str>) -> io::Result<Output> {
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
        let _ = stdin.write_all(text.as_bytes());
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
