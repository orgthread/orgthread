//! The MCP server: real messages to a real `orgthread serve`, on invented files.

use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

struct Place {
    root: PathBuf,
}

impl Place {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("orgthread-serve-{}-{n}", std::process::id()));
        fs::create_dir_all(root.join("org/.locks")).unwrap();
        fs::create_dir_all(root.join("org/notes")).unwrap();
        fs::create_dir_all(root.with_extension("outside")).unwrap();
        Place {
            root: fs::canonicalize(root).unwrap(),
        }
    }
    fn file(&self) -> PathBuf {
        self.root.join("org/notes/invented.md")
    }
    fn f(&self) -> String {
        self.file().to_str().unwrap().to_string()
    }
    fn log(&self) -> PathBuf {
        self.root.join("log/writes.log")
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_orgthread"));
        c.args(args)
            .env("ORGTHREAD_ROOT", &self.root)
            .env("ORGTHREAD_LOG", self.log())
            .env(
                "ORGTHREAD_READS",
                self.root.with_extension("outside").join("reads"),
            );
        c
    }
    fn serve(&self) -> Server {
        let mut child = self
            .command(&["serve"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Server {
            stdin: Some(child.stdin.take().unwrap()),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
            next: 0,
        }
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
        let _ = fs::remove_dir_all(self.root.with_extension("outside"));
    }
}

struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl Server {
    fn send(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }
    fn receive(&mut self) -> Value {
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }
    fn ask(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let message =
            json!({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params});
        self.send(&message.to_string());
        let answer = self.receive();
        assert_eq!(answer["id"], self.next);
        answer
    }
    fn tool(&mut self, name: &str, arguments: Value) -> (String, bool) {
        let answer = self.ask("tools/call", json!({"name": name, "arguments": arguments}));
        let result = &answer["result"];
        (
            result["content"][0]["text"].as_str().unwrap().to_string(),
            result["isError"].as_bool().unwrap(),
        )
    }
    fn read(&mut self, file: &str, who: &str) -> (String, bool) {
        self.tool("read", json!({"file": file, "who": who}))
    }
    fn write(&mut self, file: &str, who: &str, text: &str) -> (String, bool) {
        self.tool("write", json!({"file": file, "who": who, "text": text}))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.wait();
    }
}

#[test]
fn it_greets_and_offers_only_the_two_tools() {
    let place = Place::new();
    let mut server = place.serve();
    let hello = server.ask("initialize", json!({"protocolVersion": "2025-03-26"}));
    assert_eq!(hello["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(hello["result"]["serverInfo"]["name"], "orgthread");
    assert!(hello["result"]["capabilities"]["tools"].is_object());
    // A notification gets no answer: the next answer is the one for the ping.
    server.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    assert_eq!(server.ask("ping", json!({}))["result"], json!({}));

    let list = server.ask("tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["read", "write"]);
    assert_eq!(tools[0]["inputSchema"]["required"], json!(["file", "who"]));
    assert_eq!(
        tools[1]["inputSchema"]["required"],
        json!(["file", "who", "text"])
    );
}

#[test]
fn it_ends_cleanly_when_its_input_ends() {
    let place = Place::new();
    let mut server = place.serve();
    drop(server.stdin.take());
    assert!(server.child.wait().unwrap().success());
}

#[test]
fn a_read_gives_the_exact_text() {
    let place = Place::new();
    fs::write(place.file(), "ș și ü\n\n  two spaces \"quoted\"\t\n").unwrap();
    let (text, refused) = place.serve().read(&place.f(), "chat a");
    assert!(!refused);
    assert_eq!(text, "ș și ü\n\n  two spaces \"quoted\"\t\n");
}

#[test]
fn a_new_file_is_written_logged_and_can_change_again() {
    let place = Place::new();
    let mut server = place.serve();
    let (said, refused) = server.write(&place.f(), "chat a", "first ș\n");
    assert!(!refused, "{said}");
    assert_eq!(said, "written: org/notes/invented.md, 9 bytes\n");
    assert_eq!(fs::read(place.file()).unwrap(), "first ș\n".as_bytes());
    let log = fs::read_to_string(place.log()).unwrap();
    assert!(log.contains("written") && log.contains("chat a"), "{log}");

    // It knows the text it wrote, as with the command.
    let (said, refused) = server.write(&place.f(), "chat a", "second\n");
    assert!(!refused, "{said}");
    assert_eq!(fs::read(place.file()).unwrap(), b"second\n");
}

#[test]
fn a_read_then_a_write_changes_the_file() {
    let place = Place::new();
    fs::write(place.file(), "old\n").unwrap();
    let mut server = place.serve();
    server.read(&place.f(), "chat a");
    let (said, refused) = server.write(&place.f(), "chat a", "new\n");
    assert!(!refused, "{said}");
    assert_eq!(fs::read(place.file()).unwrap(), b"new\n");
}

/// The refusal of the command itself, for the same file and name.
fn command_says(place: &Place, who: &str) -> Output {
    let mut child = place
        .command(&["write", &place.f(), "--who", who])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"new\n").unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn an_unread_file_gets_the_same_refusal_as_the_command() {
    let place = Place::new();
    fs::write(place.file(), "old\n").unwrap();
    let command = command_says(&place, "chat a");
    assert_eq!(command.status.code(), Some(5));
    let (said, refused) = place.serve().write(&place.f(), "chat a", "new\n");
    assert!(refused);
    assert_eq!(
        said,
        format!("exit code 5\n{}", String::from_utf8_lossy(&command.stderr))
    );
    assert_eq!(fs::read(place.file()).unwrap(), b"old\n");
}

#[test]
fn a_file_changed_since_the_read_is_refused() {
    let place = Place::new();
    fs::write(place.file(), "old\n").unwrap();
    let mut server = place.serve();
    server.read(&place.f(), "chat a");
    fs::write(place.file(), "a person's edit\n").unwrap();
    let (said, refused) = server.write(&place.f(), "chat a", "new\n");
    assert!(refused);
    assert!(said.starts_with("exit code 4\n"), "{said}");
    assert!(said.contains("changed since you read it"), "{said}");
    assert_eq!(fs::read(place.file()).unwrap(), b"a person's edit\n");
}

#[test]
fn a_locked_file_is_refused() {
    let place = Place::new();
    fs::write(place.file(), "old\n").unwrap();
    fs::write(
        place.root.join("org/.locks/org--notes--invented.md.lock"),
        "another writer",
    )
    .unwrap();
    let mut server = place.serve();
    server.read(&place.f(), "chat a");
    let (said, refused) = server.write(&place.f(), "chat a", "new\n");
    assert!(refused);
    assert!(said.starts_with("exit code 3\n"), "{said}");
    assert!(said.contains("another writer"), "{said}");
    assert_eq!(fs::read(place.file()).unwrap(), b"old\n");
}

#[test]
fn a_missing_file_and_an_outside_file_are_refused() {
    let place = Place::new();
    let mut server = place.serve();
    let (said, refused) = server.read(&place.f(), "chat a");
    assert!(refused);
    assert!(said.starts_with("exit code 6\n"), "{said}");

    let outside = place.root.with_extension("outside").join("x.md");
    fs::write(&outside, "secret\n").unwrap();
    let (said, refused) = server.read(outside.to_str().unwrap(), "chat a");
    assert!(refused);
    assert!(said.starts_with("exit code 2\n"), "{said}");
    assert!(!said.contains("secret"), "{said}");
}

#[test]
fn text_that_is_empty_or_holds_a_nul_is_refused() {
    let place = Place::new();
    let mut server = place.serve();
    for text in ["", "a\0b"] {
        let (said, refused) = server.write(&place.f(), "chat a", text);
        assert!(refused);
        assert!(said.starts_with("exit code 2\n"), "{said}");
    }
    assert!(!place.file().exists());
}

#[test]
fn a_file_named_like_an_option_stays_a_refusal() {
    let place = Place::new();
    let mut server = place.serve();
    for file in ["--to", "--who", "--size", "--help"] {
        let (said, refused) = server.write(file, "chat a", "new\n");
        assert!(refused, "{file}: {said}");
        assert!(said.starts_with("exit code 2\n"), "{file}: {said}");
        let (said, refused) = server.read(file, "chat a");
        assert!(refused, "{file}: {said}");
    }
}

#[test]
fn wrong_messages_get_an_error_and_the_server_goes_on() {
    let place = Place::new();
    let mut server = place.serve();

    server.send("this is not json");
    assert_eq!(server.receive()["error"]["code"], -32700);
    server.send("[1, 2]");
    assert_eq!(server.receive()["error"]["code"], -32600);
    assert_eq!(
        server.ask("no/such/method", json!({}))["error"]["code"],
        -32601
    );
    let no_tool = server.ask("tools/call", json!({"name": "delete", "arguments": {}}));
    assert_eq!(no_tool["error"]["code"], -32602);
    let no_who = server.ask(
        "tools/call",
        json!({"name": "read", "arguments": {"file": place.f()}}),
    );
    assert_eq!(no_who["error"]["code"], -32602);
    assert!(no_who["error"]["message"].as_str().unwrap().contains("who"));

    assert_eq!(server.ask("ping", json!({}))["result"], json!({}));
}
