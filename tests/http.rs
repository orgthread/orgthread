//! The MCP tools over HTTP, on invented files, with the secret address.

use serde_json::{Value, json};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const SECRET: &str = "invented-secret-of-more-than-32-signs";

struct Door {
    port: u16,
    root: PathBuf,
    child: Child,
}

impl Door {
    fn open(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("orgthread-http-{name}-{}", std::process::id()));
        fs::create_dir_all(root.join("org/.locks")).unwrap();
        fs::create_dir_all(root.join("org/notes")).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let port = {
            let free = TcpListener::bind("127.0.0.1:0").unwrap();
            free.local_addr().unwrap().port()
        };
        let child = Command::new(env!("CARGO_BIN_EXE_orgthread"))
            .args(["serve", "--http", &format!("127.0.0.1:{port}")])
            .env("ORGTHREAD_HTTP_SECRET", SECRET)
            .env("ORGTHREAD_ROOT", &root)
            .env("ORGTHREAD_LOG", root.join("log"))
            .env("ORGTHREAD_READS", root.join("reads"))
            .env("ORGTHREAD_VERSIONS", root.join("versions"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        for _ in 0..100 {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Door { port, root, child }
    }
    fn raw(&self, method: &str, path: &str, body: &str) -> (u16, String) {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        let text = format!(
            "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        s.write_all(text.as_bytes()).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        let status = out.split_whitespace().nth(1).unwrap().parse().unwrap();
        let body = out.split_once("\r\n\r\n").unwrap().1.to_string();
        (status, body)
    }
    fn path(&self) -> String {
        format!("/mcp/{SECRET}")
    }
    fn call(&self, tool: &str, arguments: Value) -> Value {
        let message = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": tool, "arguments": arguments},
        });
        let (status, body) = self.raw("POST", &self.path(), &message.to_string());
        assert_eq!(status, 200, "{body}");
        serde_json::from_str(&body).unwrap()
    }
    fn file(&self) -> String {
        self.root
            .join("org/notes/invented.md")
            .to_str()
            .unwrap()
            .to_string()
    }
}

impl Drop for Door {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn text_of(reply: &Value) -> &str {
    reply["result"]["content"][0]["text"].as_str().unwrap()
}

#[test]
fn a_wrong_address_learns_nothing() {
    let d = Door::open("wrong");
    let body = json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}).to_string();
    for path in [
        "/",
        "/mcp",
        "/mcp/",
        "/mcp/wrong-secret",
        "/mcp/invented-secret",
    ] {
        let (status, text) = d.raw("POST", path, &body);
        assert_eq!(status, 404, "{path}");
        assert!(text.is_empty());
    }
}

#[test]
fn only_post_is_answered() {
    let d = Door::open("post");
    assert_eq!(d.raw("GET", &d.path(), "").0, 405);
    assert_eq!(d.raw("DELETE", &d.path(), "").0, 405);
}

#[test]
fn a_chat_can_start_and_list_the_tools() {
    let d = Door::open("list");
    let hello = json!({
        "jsonrpc":"2.0","id":1,"method":"initialize",
        "params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}},
    });
    let (status, text) = d.raw("POST", &d.path(), &hello.to_string());
    assert_eq!(status, 200);
    assert!(text.contains("orgthread"), "{text}");
    let note = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
    assert_eq!(d.raw("POST", &d.path(), &note.to_string()).0, 202);
    let list = json!({"jsonrpc":"2.0","id":2,"method":"tools/list"});
    let (_, text) = d.raw("POST", &d.path(), &list.to_string());
    assert!(
        text.contains("\"read\"") && text.contains("\"write\""),
        "{text}"
    );
}

#[test]
fn write_read_and_a_refusal_work_through_the_door() {
    let d = Door::open("use");
    let (file, who) = (d.file(), "phone chat");
    let r = d.call(
        "write",
        json!({"file": file, "who": who, "text": "Grüße\n"}),
    );
    assert_ne!(r["result"]["isError"], true, "{r}");
    assert_eq!(fs::read(&file).unwrap(), "Grüße\n".as_bytes());
    let r = d.call("read", json!({"file": file, "who": who}));
    assert_eq!(text_of(&r), "Grüße\n");
    fs::write(&file, "edited\n").unwrap();
    let r = d.call("write", json!({"file": file, "who": who, "text": "new\n"}));
    assert_eq!(r["result"]["isError"], true, "{r}");
    assert!(text_of(&r).contains("exit code 4"), "{r}");
    assert_eq!(fs::read(&file).unwrap(), b"edited\n");
}

#[test]
fn broken_json_gets_an_answer_and_the_door_stays_open() {
    let d = Door::open("broken");
    let (status, text) = d.raw("POST", &d.path(), "not json");
    assert_eq!(status, 200);
    assert!(text.contains("-32700"), "{text}");
    let list = json!({"jsonrpc":"2.0","id":2,"method":"tools/list"});
    assert_eq!(d.raw("POST", &d.path(), &list.to_string()).0, 200);
}
