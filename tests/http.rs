//! The MCP tools over HTTP behind the login, on invented files.

use orgthread::login::{challenge_of, password_hash};
use serde_json::{Value, json};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

const PASSWORD: &str = "an invented long password";
const BASE: &str = "https://door.test";
const REDIRECT: &str = "https://claude.ai/api/mcp/auth_callback";

/// One door at a time. Side by side, about 2 runs in 100 failed here with a reset
/// connection, while 5,400 requests to one running door showed none. The cause is
/// not proven, and it seems to lie in running these tests in parallel on the Mac.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

struct Door {
    _turn: MutexGuard<'static, ()>,
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

/// Starts the program, and returns once it answers.
fn start(port: u16, base: &str, root: &Path) -> Child {
    let child = Command::new(env!("CARGO_BIN_EXE_orgthread"))
        .args(["serve", "--http", &format!("127.0.0.1:{port}")])
        .env("ORGTHREAD_PUBLIC_URL", base)
        .env("ORGTHREAD_LOGIN_HASH", password_hash(PASSWORD))
        .env("ORGTHREAD_ROOT", root)
        .env("ORGTHREAD_LOCK_DIR", root.join("org/.locks"))
        .env("ORGTHREAD_LOG", root.join("log"))
        .env("ORGTHREAD_READS", root.join("reads"))
        .env("ORGTHREAD_VERSIONS", root.join("versions"))
        .env("ORGTHREAD_LOGINS", root.join("logins.json"))
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
    child
}

impl Door {
    fn open(name: &str) -> Self {
        Self::open_at(name, BASE)
    }
    fn open_at(name: &str, base: &str) -> Self {
        let turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let root =
            std::env::temp_dir().join(format!("orgthread-http-{name}-{}", std::process::id()));
        fs::create_dir_all(root.join("org/.locks")).unwrap();
        fs::create_dir_all(root.join("org/notes")).unwrap();
        let root = fs::canonicalize(root).unwrap();
        // A port for this door alone: tests run side by side, and a port asked from
        // the system and given back could go to two doors at once.
        static NEXT: AtomicU16 = AtomicU16::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let port = 20_000 + (std::process::id() % 20_000) as u16 + n;
        assert!(
            TcpListener::bind(("127.0.0.1", port)).is_ok(),
            "port {port} is busy"
        );
        let child = start(port, base, &root);
        Door {
            _turn: turn,
            port,
            root,
            child,
        }
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
        self.log_in_fully().1
    }
    /// The same, and gives the client, the access token and the refresh token.
    fn log_in_fully(&self) -> (String, String, String) {
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
        (
            client,
            v["access_token"].as_str().unwrap().to_string(),
            v["refresh_token"].as_str().unwrap().to_string(),
        )
    }
    /// Stops the program and starts it again on the same port and files.
    fn restart(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.child = start(self.port, BASE, &self.root);
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

const SECRET_BASE: &str = "https://door.test/invented-long-secret";
const SECRET: &str = "/invented-long-secret";

#[test]
fn without_the_secret_there_is_no_door_at_all() {
    let d = Door::open_at("hidden", SECRET_BASE);
    let list = json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}).to_string();
    for (method, path) in [
        ("POST", "/mcp"),
        ("GET", "/authorize"),
        ("POST", "/authorize"),
        ("POST", "/token"),
        ("POST", "/register"),
        ("GET", "/.well-known/oauth-protected-resource"),
        ("GET", "/.well-known/oauth-authorization-server"),
        ("POST", "/wrong-secret/mcp"),
        (
            "GET",
            "/.well-known/oauth-authorization-server/wrong-secret",
        ),
    ] {
        let r = d.raw(method, path, "", &list);
        assert_eq!(r.status, 404, "{method} {path}");
        assert!(r.body.is_empty());
    }
}

#[test]
fn claude_finds_the_login_behind_the_secret_every_standard_way() {
    let d = Door::open_at("found", SECRET_BASE);
    let r = d.raw("POST", &format!("{SECRET}/mcp"), "", "{}");
    assert_eq!(r.status, 401);
    assert!(r.head.contains(&format!(
        "resource_metadata=\"{SECRET_BASE}/.well-known/oauth-protected-resource\""
    )));
    for path in [
        format!("{SECRET}/.well-known/oauth-protected-resource"),
        format!("/.well-known/oauth-protected-resource{SECRET}/mcp"),
    ] {
        let r = d.raw("GET", &path, "", "");
        assert_eq!(r.status, 200, "{path}");
        let v: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(v["resource"], format!("{SECRET_BASE}/mcp"));
        assert_eq!(v["authorization_servers"][0], SECRET_BASE);
    }
    for path in [
        format!("{SECRET}/.well-known/oauth-authorization-server"),
        format!("/.well-known/oauth-authorization-server{SECRET}"),
    ] {
        let r = d.raw("GET", &path, "", "");
        assert_eq!(r.status, 200, "{path}");
        let v: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(v["issuer"], SECRET_BASE);
        assert_eq!(v["token_endpoint"], format!("{SECRET_BASE}/token"));
    }
}

#[test]
fn the_whole_login_and_a_write_work_behind_the_secret() {
    let d = Door::open_at("whole", SECRET_BASE);
    let reg = d.raw(
        "POST",
        &format!("{SECRET}/register"),
        "",
        &json!({"redirect_uris": [REDIRECT]}).to_string(),
    );
    assert_eq!(reg.status, 201, "{}", reg.body);
    let v: Value = serde_json::from_str(&reg.body).unwrap();
    let client = v["client_id"].as_str().unwrap().to_string();
    let resource = encode(&format!("{SECRET_BASE}/mcp"));
    let query = format!(
        "client_id={client}&redirect_uri={}&response_type=code&code_challenge={}&code_challenge_method=S256&resource={resource}",
        encode(REDIRECT),
        challenge_of("v2"),
    );
    let page = d.raw("GET", &format!("{SECRET}/authorize?{query}"), "", "");
    assert!(
        page.body
            .contains(&format!("action=\"{SECRET}/authorize\""))
    );
    let sent = d.raw(
        "POST",
        &format!("{SECRET}/authorize"),
        "",
        &format!("{query}&password={}", encode(PASSWORD)),
    );
    assert_eq!(sent.status, 302, "{}", sent.body);
    let location = sent
        .head
        .lines()
        .find_map(|l| l.strip_prefix("Location: "))
        .unwrap();
    let code = location
        .split("code=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();
    let token = d.raw(
        "POST",
        &format!("{SECRET}/token"),
        "",
        &format!(
            "grant_type=authorization_code&code={code}&client_id={client}&redirect_uri={}&code_verifier=v2&resource={resource}",
            encode(REDIRECT)
        ),
    );
    assert_eq!(token.status, 200, "{}", token.body);
    let v: Value = serde_json::from_str(&token.body).unwrap();
    let t = v["access_token"].as_str().unwrap();
    let call = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "write", "arguments": {"file": d.file(), "who": "c", "text": "ok\n"}},
    });
    let extra = format!("Authorization: Bearer {t}\r\n");
    let r = d.raw("POST", &format!("{SECRET}/mcp"), &extra, &call.to_string());
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(fs::read(d.file()).unwrap(), b"ok\n");
    // The right token at the address with no secret still finds nothing.
    assert_eq!(d.raw("POST", "/mcp", &extra, &call.to_string()).status, 404);
}

#[test]
fn a_login_survives_a_restart_of_the_program() {
    let mut d = Door::open("restart");
    let (client, access, refresh) = d.log_in_fully();
    let start_call = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
    assert_eq!(d.mcp(&access, &start_call).status, 200);

    d.restart();

    // Claude needs do nothing: the token it holds still opens the door.
    assert_eq!(d.mcp(&access, &start_call).status, 200);
    assert_eq!(d.mcp("invented", &start_call).status, 401);

    // And when that token runs out, the refresh token works, once.
    let form = format!("grant_type=refresh_token&refresh_token={refresh}&client_id={client}");
    let renewed = d.raw("POST", "/token", "", &form);
    assert_eq!(renewed.status, 200, "{}", renewed.body);
    let v: Value = serde_json::from_str(&renewed.body).unwrap();
    assert_eq!(
        d.mcp(v["access_token"].as_str().unwrap(), &start_call)
            .status,
        200
    );
    assert_eq!(d.raw("POST", "/token", "", &form).status, 400);

    // It stays dead after one more restart, and no raw token is on the disk.
    d.restart();
    assert_eq!(d.raw("POST", "/token", "", &form).status, 400);
    let kept = fs::read_to_string(d.root.join("logins.json")).unwrap();
    assert!(!kept.contains(&access));
    assert!(!kept.contains(&refresh));
    assert!(!kept.contains(v["refresh_token"].as_str().unwrap()));
}
