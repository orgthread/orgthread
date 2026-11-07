//! The MCP tools over HTTP behind the login, on invented files.

use orgthread::login::{challenge_of, password_hash};
use serde_json::{Value, json};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const PASSWORD: &str = "an invented long password";
const BASE: &str = "https://door.test";
const REDIRECT: &str = "https://claude.ai/api/mcp/auth_callback";

struct Door {
    port: u16,
    root: PathBuf,
    child: Child,
}

struct Reply {
    status: u16,
    head: String,
    body: String,
}

fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
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
            .env("ORGTHREAD_PUBLIC_URL", BASE)
            .env("ORGTHREAD_LOGIN_HASH", password_hash(PASSWORD))
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
            head: head.to_string(),
            body: body.to_string(),
        }
    }
    /// Goes through the whole login, as Claude's connector does.
    fn log_in(&self) -> String {
        let r = self.raw(
            "POST",
            "/register",
            "",
            &json!({"redirect_uris": [REDIRECT]}).to_string(),
        );
        assert_eq!(r.status, 201, "{}", r.body);
        let client: Value = serde_json::from_str(&r.body).unwrap();
        let client = client["client_id"].as_str().unwrap().to_string();
        let query = format!(
            "client_id={client}&redirect_uri={}&response_type=code&state=abc&code_challenge={}&code_challenge_method=S256&resource={}",
            encode(REDIRECT),
            challenge_of("verifier-1"),
            encode(&format!("{BASE}/mcp"))
        );
        let page = self.raw("GET", &format!("/authorize?{query}"), "", "");
        assert_eq!(page.status, 200, "{}", page.body);
        let sent = self.raw(
            "POST",
            "/authorize",
            "",
            &format!("{query}&password={}", encode(PASSWORD)),
        );
        assert_eq!(sent.status, 302, "{}", sent.body);
        let location = sent
            .head
            .lines()
            .find_map(|l| l.strip_prefix("Location: "))
            .unwrap();
        assert!(location.starts_with(REDIRECT));
        assert!(location.contains("state=abc"));
        let code = location
            .split("code=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap();
        let token = self.raw(
            "POST",
            "/token",
            "",
            &format!(
                "grant_type=authorization_code&code={code}&client_id={client}&redirect_uri={}&code_verifier=verifier-1&resource={}",
                encode(REDIRECT),
                encode(&format!("{BASE}/mcp"))
            ),
        );
        assert_eq!(token.status, 200, "{}", token.body);
        let v: Value = serde_json::from_str(&token.body).unwrap();
        v["access_token"].as_str().unwrap().to_string()
    }
    fn mcp(&self, token: &str, message: &Value) -> Reply {
        let extra = format!("Authorization: Bearer {token}\r\n");
        self.raw("POST", "/mcp", &extra, &message.to_string())
    }
    fn call(&self, token: &str, tool: &str, arguments: Value) -> Value {
        let message = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": tool, "arguments": arguments},
        });
        let r = self.mcp(token, &message);
        assert_eq!(r.status, 200, "{}", r.body);
        serde_json::from_str(&r.body).unwrap()
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
fn with_no_token_the_door_points_to_the_login_and_gives_nothing() {
    let d = Door::open("closed");
    let list = json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}).to_string();
    let r = d.raw("POST", "/mcp", "", &list);
    assert_eq!(r.status, 401);
    assert!(r.body.is_empty());
    assert!(r.head.contains(&format!(
        "WWW-Authenticate: Bearer resource_metadata=\"{BASE}/.well-known/oauth-protected-resource\""
    )));
    let r = d.raw("POST", "/mcp", "Authorization: Bearer invented\r\n", &list);
    assert_eq!(r.status, 401);
    assert_eq!(d.raw("POST", "/anything", "", "").status, 404);
}

#[test]
fn claude_can_find_the_login() {
    let d = Door::open("meta");
    let r = d.raw("GET", "/.well-known/oauth-protected-resource", "", "");
    let v: Value = serde_json::from_str(&r.body).unwrap();
    assert_eq!(v["resource"], format!("{BASE}/mcp"));
    assert_eq!(v["authorization_servers"][0], BASE);
    let r = d.raw("GET", "/.well-known/oauth-authorization-server", "", "");
    let v: Value = serde_json::from_str(&r.body).unwrap();
    assert_eq!(v["issuer"], BASE);
    assert_eq!(v["token_endpoint"], format!("{BASE}/token"));
    assert_eq!(v["code_challenge_methods_supported"][0], "S256");
}

#[test]
fn after_the_login_a_chat_can_start_list_write_and_read() {
    let d = Door::open("use");
    let t = d.log_in();
    let hello = json!({
        "jsonrpc":"2.0","id":1,"method":"initialize",
        "params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}},
    });
    assert!(d.mcp(&t, &hello).body.contains("orgthread"));
    let note = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
    assert_eq!(d.mcp(&t, &note).status, 202);
    let list = d.mcp(&t, &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
    assert!(list.body.contains("\"write\""));

    let (file, who) = (d.file(), "phone chat");
    let r = d.call(
        &t,
        "write",
        json!({"file": file, "who": who, "text": "Grüße\n"}),
    );
    assert_ne!(r["result"]["isError"], true, "{r}");
    assert_eq!(fs::read(&file).unwrap(), "Grüße\n".as_bytes());
    assert_eq!(
        text_of(&d.call(&t, "read", json!({"file": file, "who": who}))),
        "Grüße\n"
    );
    fs::write(&file, "edited\n").unwrap();
    let r = d.call(
        &t,
        "write",
        json!({"file": file, "who": who, "text": "new\n"}),
    );
    assert!(text_of(&r).contains("exit code 4"), "{r}");
    assert_eq!(fs::read(&file).unwrap(), b"edited\n");

    let extra = format!("Authorization: Bearer {t}\r\n");
    assert_eq!(d.raw("GET", "/mcp", &extra, "").status, 405);
    let broken = d.raw("POST", "/mcp", &extra, "not json");
    assert!(broken.body.contains("-32700"));
}

#[test]
fn a_wrong_password_gets_no_code() {
    let d = Door::open("wrong");
    let r = d.raw(
        "POST",
        "/register",
        "",
        &json!({"redirect_uris": [REDIRECT]}).to_string(),
    );
    let v: Value = serde_json::from_str(&r.body).unwrap();
    let client = v["client_id"].as_str().unwrap();
    let query = format!(
        "client_id={client}&redirect_uri={}&response_type=code&code_challenge=x&code_challenge_method=S256",
        encode(REDIRECT)
    );
    let r = d.raw("POST", "/authorize", "", &format!("{query}&password=wrong"));
    assert_eq!(r.status, 401);
    assert!(!r.head.contains("Location"));
    assert!(r.body.contains("wrong"));
}

#[test]
fn the_door_needs_its_settings() {
    let o = Command::new(env!("CARGO_BIN_EXE_orgthread"))
        .args(["serve", "--http", "127.0.0.1:0"])
        .env_remove("ORGTHREAD_PUBLIC_URL")
        .env_remove("ORGTHREAD_LOGIN_HASH")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
}
