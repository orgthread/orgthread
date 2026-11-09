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
        .env("ORGTHREAD_BINARY_ALLOW", "org/scans")
        .env("ORGTHREAD_UPLOAD_MB", "1")
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

/// The program with the settings a door needs, on an address.
fn serve_at(address: &str, anywhere: Option<&str>) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orgthread"));
    command
        .args(["serve", "--http", address])
        .env("ORGTHREAD_PUBLIC_URL", BASE)
        .env("ORGTHREAD_LOGIN_HASH", password_hash(PASSWORD))
        .env(
            "ORGTHREAD_LOGINS",
            std::env::temp_dir().join(format!("orgthread-address-{}", std::process::id())),
        )
        .env_remove("ORGTHREAD_LISTEN_ANYWHERE")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(value) = anywhere {
        command.env("ORGTHREAD_LISTEN_ANYWHERE", value);
    }
    command.spawn().unwrap()
}

/// The exit code and the message, or a failed test if the program keeps running.
fn assert_refused(address: &str, anywhere: Option<&str>) {
    let mut child = serve_at(address, anywhere);
    for _ in 0..250 {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
        child.wait().unwrap();
        panic!("{address} was not refused: the program kept running");
    }
    let o = child.wait_with_output().unwrap();
    let message = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(2), "{address}: {message}");
    assert!(
        message.contains("refused to listen on"),
        "{address}: {message}"
    );
    assert!(
        message.contains("ORGTHREAD_LISTEN_ANYWHERE=yes"),
        "{address}: {message}"
    );
}

/// The program listens on `address`, and answers at `reach`.
fn assert_listens(address: &str, reach: &str, anywhere: Option<&str>) {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let mut child = serve_at(address, anywhere);
    let mut answered = false;
    for _ in 0..250 {
        if TcpStream::connect(reach).is_ok() {
            answered = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let running = child.try_wait().unwrap().is_none();
    let _ = child.kill();
    let _ = child.wait();
    assert!(answered && running, "{address} did not listen");
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

#[test]
fn the_door_listens_on_the_machine_itself() {
    let port = free_port();
    let address = format!("127.0.0.1:{port}");
    assert_listens(&address, &address, None);
    let address = format!("[::1]:{port}");
    assert_listens(&address, &address, None);
    let address = format!("localhost:{port}");
    assert_listens(&address, &address, None);
}

#[test]
fn the_door_refuses_every_address_of_the_network() {
    for address in [
        "0.0.0.0:0",
        "[::]:0",
        ":0",
        "10.0.0.1:0",
        "8.8.8.8:0",
        "[2001:db8::1]:0",
        "[::ffff:127.0.0.1]:0",
    ] {
        assert_refused(address, None);
    }
}

#[test]
fn a_name_does_not_pass_for_looking_local() {
    for address in [
        "localhost.example.com:0",
        "127.0.0.1.example.com:0",
        "localhost.:0",
        "notlocalhost:0",
        "127.1:0",
        "0:0",
        "localhost",
        "localhost:",
        "localhost:notaport",
    ] {
        assert_refused(address, None);
    }
}

#[test]
fn only_the_setting_with_yes_allows_a_network_address() {
    for value in ["", "no", "true", "YES"] {
        assert_refused("0.0.0.0:0", Some(value));
    }
    let port = free_port();
    assert_listens(
        &format!("0.0.0.0:{port}"),
        &format!("127.0.0.1:{port}"),
        Some("yes"),
    );
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

/// Bytes that are not text: a NUL sign and no valid UTF-8.
fn picture(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i * 7 % 256) as u8).collect()
}

/// The body of a chunked answer, and whether its last chunk came.
fn dechunk(mut body: &[u8]) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    loop {
        let Some(end) = body.windows(2).position(|w| w == b"\r\n") else {
            return (out, false);
        };
        let size = usize::from_str_radix(std::str::from_utf8(&body[..end]).unwrap(), 16).unwrap();
        body = &body[end + 2..];
        if size == 0 {
            return (out, true);
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}

struct Got {
    status: u16,
    head: String,
    body: Vec<u8>,
}

impl Door {
    /// A request with any bytes, and the answer as bytes, as curl sends and gets them.
    fn bytes(&self, method: &str, path: &str, extra: &str, body: &[u8]) -> Got {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        let head = format!(
            "{method} {path} HTTP/1.1\r\nHost: x\r\n{extra}Content-Length: {}\r\n\r\n",
            body.len()
        );
        s.write_all(head.as_bytes()).unwrap();
        let _ = s.write_all(body);
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out);
        // curl reads a first answer of 100 Continue, and then the real one.
        if let Some(rest) = out.strip_prefix(b"HTTP/1.1 100 Continue\r\n\r\n") {
            out = rest.to_vec();
        }
        let split = out.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8_lossy(&out[..split]).to_string();
        let mut body = out[split + 4..].to_vec();
        if head.contains("Transfer-Encoding: chunked") {
            let (whole, complete) = dechunk(&body);
            assert!(complete, "the last chunk never came");
            body = whole;
        }
        Got {
            status: head.split_whitespace().nth(1).unwrap().parse().unwrap(),
            head,
            body,
        }
    }
    fn scan(&self, name: &str) -> String {
        fs::create_dir_all(self.root.join("org/scans")).unwrap();
        self.root
            .join("org/scans")
            .join(name)
            .to_str()
            .unwrap()
            .to_string()
    }
}

/// The path part of each link in a tool answer, in order.
fn links_in(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.split_whitespace().last())
        .filter_map(|l| l.strip_prefix(BASE))
        .map(str::to_string)
        .collect()
}

#[test]
fn only_the_remote_door_offers_download_and_upload() {
    let d = Door::open("transfer-tools");
    let t = d.log_in();
    let list = d.mcp(&t, &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
    assert!(list.body.contains("\"download\""), "{}", list.body);
    assert!(list.body.contains("\"upload\""));
    assert!(list.body.contains("Up to 1 MB"), "{}", list.body);
    assert_eq!(
        list.body.matches("Allowed websites").count(),
        2,
        "{}",
        list.body
    );
}

#[test]
fn the_version_tool_of_the_remote_door_gives_no_secret() {
    let d = Door::open("version");
    let t = d.log_in();
    let list = d.mcp(&t, &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
    assert!(list.body.contains("\"version\""), "{}", list.body);
    let r = d.call(&t, "version", json!({}));
    assert_eq!(r["result"]["isError"], false, "{r}");
    let text = text_of(&r);
    let want = format!(
        "orgthread {}\ndoor: remote\nstorage: local folder\n\
         tools: read, write, patch, delete, copy, list, download, upload, version\n",
        orgthread::VERSION
    );
    assert_eq!(text, want);
    // Not the address, not the token, not a path, not a limit, not a setting.
    for secret in [
        BASE,
        "door.test",
        &t,
        d.root.to_str().unwrap(),
        "org/scans",
        "MB",
    ] {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
}

#[test]
fn the_version_tool_needs_the_login() {
    let d = Door::open("version-closed");
    let call = json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
        "params":{"name":"version","arguments":{}}})
    .to_string();
    let r = d.raw("POST", "/mcp", "", &call);
    assert_eq!(r.status, 401);
    assert!(r.body.is_empty());
}

#[test]
fn a_picture_goes_up_and_comes_back_through_links() {
    let d = Door::open("transfer-both");
    let t = d.log_in();
    let file = d.scan("p1.jpg");
    let bytes = picture(300_000);
    let reply = d.call(&t, "upload", json!({"files": [&file], "who": "chat a"}));
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    let up = links_in(text_of(&reply));
    assert_eq!(up.len(), 1, "{}", text_of(&reply));
    assert!(up[0].starts_with("/t/"));
    let sent = d.bytes("PUT", &up[0], "Expect: 100-continue\r\n", &bytes);
    assert_eq!(sent.status, 200, "{}", String::from_utf8_lossy(&sent.body));
    assert_eq!(
        String::from_utf8_lossy(&sent.body),
        "written: org/scans/p1.jpg, 300000 bytes\n"
    );
    assert_eq!(fs::read(&file).unwrap(), bytes);

    let reply = d.call(&t, "download", json!({"files": [&file], "who": "chat b"}));
    let text = text_of(&reply);
    assert!(text.contains("300000 bytes"), "{text}");
    let down = links_in(text);
    let got = d.bytes("GET", &down[0], "", b"");
    assert_eq!(got.status, 200, "{}", got.head);
    assert_eq!(got.body, bytes);
    // Each link works once, and a second try gets a clear answer, not a reset.
    assert_eq!(d.bytes("GET", &down[0], "", b"").status, 404);
    let again = d.bytes("PUT", &up[0], "", &bytes);
    assert_eq!(again.status, 404);
    assert!(again.body.is_empty());
}

#[test]
fn a_refused_file_does_not_stop_the_others() {
    let d = Door::open("transfer-lines");
    let t = d.log_in();
    let good = d.scan("p1.jpg");
    fs::write(&good, picture(10)).unwrap();
    let missing = d.scan("none.jpg");
    let reply = d.call(
        &t,
        "download",
        json!({"files": [&missing, &good], "who": "a"}),
    );
    assert_eq!(reply["result"]["isError"], true);
    let text = text_of(&reply);
    assert!(text.starts_with("exit code 6\n"), "{text}");
    assert!(
        text.contains("none.jpg  refused, exit code 6: there is no file"),
        "{text}"
    );
    assert_eq!(links_in(text).len(), 1, "{text}");
    let too_many: Vec<String> = (0..26).map(|i| d.scan(&format!("{i}.jpg"))).collect();
    let r = d.mcp(
        &t,
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":"download","arguments":{"files": too_many, "who": "a"}}}),
    );
    assert!(r.body.contains("-32602"), "{}", r.body);
}

#[test]
fn a_link_works_only_in_its_own_direction() {
    let d = Door::open("transfer-way");
    let t = d.log_in();
    let file = d.scan("p1.jpg");
    fs::write(&file, picture(10)).unwrap();
    let reply = d.call(&t, "download", json!({"files": [&file], "who": "a"}));
    let link = &links_in(text_of(&reply))[0];
    assert_eq!(d.bytes("PUT", link, "", &picture(5)).status, 405);
    assert_eq!(fs::read(&file).unwrap(), picture(10));
    assert_eq!(d.bytes("GET", link, "", b"").body, picture(10));
}

#[test]
fn an_upload_over_the_limit_or_to_a_text_folder_writes_nothing() {
    let d = Door::open("transfer-limit");
    let t = d.log_in();
    let file = d.scan("big.pdf");
    let reply = d.call(&t, "upload", json!({"files": [&file], "who": "a"}));
    let link = &links_in(text_of(&reply))[0];
    let r = d.bytes("PUT", link, "Expect: 100-continue\r\n", &picture(1_000_001));
    assert_eq!(r.status, 409);
    let said = String::from_utf8_lossy(&r.body);
    assert!(said.starts_with("exit code 2\n"), "{said:?}");
    assert!(!Path::new(&file).exists());

    // A picture name in a text folder is refused before any link is made.
    let named = d
        .root
        .join("org/notes/p1.jpg")
        .to_str()
        .unwrap()
        .to_string();
    let reply = d.call(&t, "upload", json!({"files": [&named], "who": "a"}));
    assert!(links_in(text_of(&reply)).is_empty(), "{reply}");
    assert!(
        text_of(&reply).contains("ORGTHREAD_BINARY_ALLOW"),
        "{reply}"
    );

    // A name that does not show it gets a link, and the bytes are refused.
    let notes = d
        .root
        .join("org/notes/p1.bin")
        .to_str()
        .unwrap()
        .to_string();
    let reply = d.call(&t, "upload", json!({"files": [&notes], "who": "a"}));
    let link = &links_in(text_of(&reply))[0];
    let r = d.bytes("PUT", link, "", &picture(100));
    assert_eq!(r.status, 409);
    assert!(String::from_utf8_lossy(&r.body).contains("ORGTHREAD_BINARY_ALLOW"));
    assert!(!Path::new(&notes).exists());
}

#[test]
fn a_replace_by_link_keeps_the_rules_of_write() {
    let d = Door::open("transfer-replace");
    let t = d.log_in();
    let file = d.scan("p1.jpg");
    fs::write(&file, picture(10)).unwrap();
    // The link is refused at once, before any bytes move.
    let reply = d.call(&t, "upload", json!({"files": [&file], "who": "a"}));
    assert!(
        text_of(&reply).contains("refused, exit code 5"),
        "{}",
        text_of(&reply)
    );
    // After a download by the same name, a replace goes through.
    let reply = d.call(&t, "download", json!({"files": [&file], "who": "a"}));
    let link = &links_in(text_of(&reply))[0];
    assert_eq!(d.bytes("GET", link, "", b"").status, 200);
    let reply = d.call(&t, "upload", json!({"files": [&file], "who": "a"}));
    let link = &links_in(text_of(&reply))[0];
    assert_eq!(d.bytes("PUT", link, "", &picture(20)).status, 200);
    assert_eq!(fs::read(&file).unwrap(), picture(20));
}

#[test]
fn a_link_never_appears_in_the_log() {
    let d = Door::open("transfer-log");
    let t = d.log_in();
    let file = d.scan("p1.jpg");
    let reply = d.call(&t, "upload", json!({"files": [&file], "who": "a"}));
    let link = links_in(text_of(&reply)).remove(0);
    assert_eq!(d.bytes("PUT", &link, "", &picture(50)).status, 200);
    let reply = d.call(&t, "download", json!({"files": [&file], "who": "a"}));
    let down = links_in(text_of(&reply)).remove(0);
    assert_eq!(d.bytes("GET", &down, "", b"").status, 200);
    let log = fs::read_to_string(d.root.join("log")).unwrap();
    assert_eq!(log.lines().count(), 2, "{log}");
    for secret in [&link, &down] {
        assert!(!log.contains(secret.trim_start_matches("/t/")), "{log}");
    }
}

#[test]
fn the_curl_lines_of_the_tool_descriptions_work() {
    let d = Door::open("transfer-curl");
    let t = d.log_in();
    let file = d.scan("big.jpg");
    let here = d.root.join("sandbox");
    fs::create_dir_all(&here).unwrap();
    // Larger than 1 MB is refused, so just under it, and larger than one chunk.
    let bytes = picture(999_000);
    fs::write(here.join("big.jpg"), &bytes).unwrap();
    let url = |link: &str| format!("http://127.0.0.1:{}{link}", d.port);

    let reply = d.call(&t, "upload", json!({"files": [&file], "who": "a"}));
    let link = links_in(text_of(&reply)).remove(0);
    let up = Command::new("curl")
        .args(["-sS", "-T", "big.jpg", &url(&link)])
        .current_dir(&here)
        .output()
        .unwrap();
    assert!(
        up.status.success(),
        "{}",
        String::from_utf8_lossy(&up.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&up.stdout),
        "written: org/scans/big.jpg, 999000 bytes\n"
    );
    assert_eq!(fs::read(&file).unwrap(), bytes);

    let reply = d.call(&t, "download", json!({"files": [&file], "who": "a"}));
    let link = links_in(text_of(&reply)).remove(0);
    let down = Command::new("curl")
        .args(["-sS", "-o", "back.jpg", &url(&link)])
        .current_dir(&here)
        .output()
        .unwrap();
    assert!(
        down.status.success(),
        "{}",
        String::from_utf8_lossy(&down.stderr)
    );
    assert_eq!(fs::read(here.join("back.jpg")).unwrap(), bytes);
}

impl Door {
    /// The whole login behind a secret path, and the access token.
    fn log_in_under(&self, prefix: &str, base: &str) -> String {
        let reg = self.raw(
            "POST",
            &format!("{prefix}/register"),
            "",
            &json!({"redirect_uris": [REDIRECT]}).to_string(),
        );
        let v: Value = serde_json::from_str(&reg.body).unwrap();
        let client = v["client_id"].as_str().unwrap().to_string();
        let resource = encode(&format!("{base}/mcp"));
        let query = format!(
            "client_id={client}&redirect_uri={}&response_type=code&code_challenge={}&code_challenge_method=S256&resource={resource}",
            encode(REDIRECT),
            challenge_of("v3"),
        );
        let sent = self.raw(
            "POST",
            &format!("{prefix}/authorize"),
            "",
            &format!("{query}&password={}", encode(PASSWORD)),
        );
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
        let token = self.raw(
            "POST",
            &format!("{prefix}/token"),
            "",
            &format!(
                "grant_type=authorization_code&code={code}&client_id={client}&redirect_uri={}&code_verifier=v3&resource={resource}",
                encode(REDIRECT)
            ),
        );
        let v: Value = serde_json::from_str(&token.body).unwrap();
        v["access_token"].as_str().unwrap().to_string()
    }
}

#[test]
fn a_link_never_holds_the_secret_path_of_the_door() {
    let d = Door::open_at("transfer-secret", SECRET_BASE);
    let t = d.log_in_under(SECRET, SECRET_BASE);
    let file = d.scan("p1.jpg");
    fs::write(&file, picture(10)).unwrap();
    let call = json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
        "params":{"name":"download","arguments":{"files":[&file],"who":"a"}}});
    let extra = format!("Authorization: Bearer {t}\r\n");
    let r = d.raw("POST", &format!("{SECRET}/mcp"), &extra, &call.to_string());
    let reply: Value = serde_json::from_str(&r.body).unwrap();
    let text = text_of(&reply);
    assert!(!text.contains(SECRET), "{text}");
    let link = links_in(text).remove(0);
    assert!(link.starts_with("/t/"), "{link}");
    // Under the secret path there is no link door.
    assert_eq!(
        d.bytes("GET", &format!("{SECRET}{link}"), "", b"").status,
        404
    );
    assert_eq!(d.bytes("GET", &link, "", b"").body, picture(10));
}
