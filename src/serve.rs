//! The door for a chat with no shell: MCP over standard input and output.
//!
//! Each tool call starts the command itself, `orgthread read` or `orgthread write`.
//! So there is no second copy of the rules: the lock, the checks, the log and every
//! refusal are the ones of the command.

use serde_json::{Value, json};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
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

/// The door for a chat in the cloud: the same answers over HTTP, on one address
/// with a long secret in it. It listens on this machine only, so a program that
/// handles TLS must stand in front. There is no session and no stream: each POST
/// gets one JSON answer, which the Streamable HTTP form of MCP allows.
pub fn run_http(address: &str, secret: &str, exe: &Path) -> io::Result<()> {
    let listener = TcpListener::bind(address)?;
    let path = format!("/mcp/{secret}");
    std::thread::scope(|scope| {
        for stream in listener.incoming().flatten() {
            let path = &path;
            scope.spawn(move || {
                let _ = handle(stream, path, exe);
            });
        }
    });
    Ok(())
}

const MAX_BODY: usize = 64 * 1024 * 1024;

fn respond(stream: &mut TcpStream, status: &str, body: &[u8]) -> io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)
}

fn handle(mut stream: TcpStream, path: &str, exe: &Path) -> io::Result<()> {
    stream.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut first = String::new();
    reader.by_ref().take(8192).read_line(&mut first)?;
    let mut parts = first.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if reader.by_ref().take(8192).read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    // A wrong address learns nothing, not even that this is the right server.
    if target != path {
        return respond(&mut stream, "404 Not Found", b"");
    }
    if method != "POST" {
        return respond(&mut stream, "405 Method Not Allowed", b"");
    }
    if length > MAX_BODY {
        return respond(&mut stream, "413 Content Too Large", b"");
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let answered = match serde_json::from_slice::<Value>(&body) {
        Ok(message) => answer(&message, exe),
        Err(e) => Some(problem(Value::Null, NOT_JSON, &e.to_string())),
    };
    match answered {
        Some(reply) => respond(&mut stream, "200 OK", reply.to_string().as_bytes()),
        // A notification wants no answer.
        None => respond(&mut stream, "202 Accepted", b""),
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
