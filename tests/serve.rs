//! The MCP server: real messages to a real `orgthread serve`, on invented files.

use orgthread::fingerprint::fingerprint;
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
            .env("ORGTHREAD_LOCK_DIR", self.root.join("org/.locks"))
            .env("ORGTHREAD_LOG", self.log())
            .env(
                "ORGTHREAD_READS",
                self.root.with_extension("outside").join("reads"),
            )
            .env(
                "ORGTHREAD_VERSIONS",
                self.root.with_extension("outside").join("versions"),
            );
        c
    }
    fn serve(&self) -> Server {
        self.serve_with(&[])
    }
    fn serve_with(&self, settings: &[(&str, &str)]) -> Server {
        let mut command = self.command(&["serve"]);
        command.envs(settings.iter().copied());
        let mut child = command
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
fn it_greets_and_offers_the_eight_tools() {
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
    assert_eq!(
        names,
        [
            "read", "write", "patch", "delete", "copy", "move", "list", "version"
        ]
    );
    assert_eq!(tools[7]["inputSchema"]["properties"], json!({}));
    assert_eq!(tools[0]["inputSchema"]["required"], json!(["file", "who"]));
    assert_eq!(
        tools[1]["inputSchema"]["required"],
        json!(["file", "who", "text"])
    );
    assert_eq!(tools[2]["inputSchema"]["required"], json!(["file", "who"]));
    assert_eq!(tools[3]["inputSchema"]["required"], json!(["file", "who"]));
    assert_eq!(tools[4]["inputSchema"]["required"], json!(["pairs", "who"]));
    assert_eq!(
        tools[4]["inputSchema"]["properties"]["pairs"]["maxItems"],
        25
    );
    let says = tools[4]["description"].as_str().unwrap();
    assert!(
        says.contains("Up to 25 pairs in one call. For more, call again with the rest."),
        "{says}"
    );
    assert_eq!(tools[5]["inputSchema"]["required"], json!(["pairs", "who"]));
    assert_eq!(
        tools[5]["inputSchema"]["properties"]["pairs"]["maxItems"],
        100
    );
    let says = tools[5]["description"].as_str().unwrap();
    assert!(
        says.contains("Up to 100 pairs in one call. For more, call again with the rest."),
        "{says}"
    );
    assert_eq!(tools[6]["inputSchema"]["required"], json!(["folders"]));
}

#[test]
fn the_version_tool_names_the_build_the_door_the_storage_and_the_tools() {
    let place = Place::new();
    let mut server = place.serve();
    let (text, refused) = server.tool("version", json!({}));
    assert!(!refused, "{text}");
    let want = format!(
        "orgthread {}\ndoor: local\nstorage: local folder\ntools: read, write, patch, delete, copy, move, list, version\n",
        orgthread::VERSION
    );
    assert_eq!(text, want);
    // The commit is in it, as with --version, and no path of this machine.
    assert!(text.contains(env!("CARGO_PKG_VERSION")));
    assert!(text.contains(" ("));
    assert!(!text.contains(place.root.to_str().unwrap()), "{text}");
}

#[test]
fn the_version_tool_names_dropbox_and_no_setting() {
    let place = Place::new();
    let token = "invented-dropbox-token-a1b2c3";
    let mut server = place.serve_with(&[
        ("ORGTHREAD_STORAGE", "dropbox"),
        ("ORGTHREAD_DROPBOX_TOKEN", token),
        ("ORGTHREAD_ALLOW", "/org/invented-allowed"),
    ]);
    // Arguments that it does not need change nothing.
    let (text, refused) = server.tool("version", json!({"file": "/org/a.md"}));
    assert!(!refused, "{text}");
    assert!(text.contains("\nstorage: Dropbox\n"), "{text}");
    for secret in [token, "invented-allowed", place.root.to_str().unwrap()] {
        assert!(!text.contains(secret), "{text}");
    }
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

#[test]
fn a_list_gives_the_same_text_as_the_command() {
    let place = Place::new();
    fs::write(place.file(), "four").unwrap();
    fs::write(place.root.join("org/notes/.DS_Store"), "x").unwrap();
    fs::create_dir_all(place.root.join("org/notes/drafts")).unwrap();
    fs::write(place.root.join("org/notes/drafts/c.md"), "c").unwrap();
    let notes = place.root.join("org/notes");
    let notes = notes.to_str().unwrap();
    let mut server = place.serve();
    let (said, refused) = server.tool("list", json!({"folders": [notes]}));
    assert!(!refused, "{said}");
    let command = place
        .command(&["list", notes])
        .env("TZ", "UTC")
        .output()
        .unwrap();
    assert!(said.starts_with(&format!("{notes}:\n")), "{said}");
    assert!(said.contains("        4  "), "{said}");
    assert!(said.contains("  invented.md\n"), "{said}");
    assert!(said.contains("  drafts/\n"), "{said}");
    assert!(!said.contains(".DS_Store"), "{said}");
    assert!(!said.contains("c.md"), "{said}");
    // The times may differ by zone, so the rest is compared line by line.
    assert_eq!(
        said.lines().count(),
        String::from_utf8(command.stdout).unwrap().lines().count()
    );

    let (said, _) = server.tool("list", json!({"folders": [notes], "depth": 2, "all": true}));
    assert!(said.contains(".DS_Store"), "{said}");
    assert!(said.contains("/drafts:\n"), "{said}");
    assert!(said.contains("  c.md\n"), "{said}");
    let (said, _) = server.tool("list", json!({"folders": [notes], "depth": "all"}));
    assert!(said.contains("  c.md\n"), "{said}");
    assert!(
        !said.contains(&fingerprint(b"four")),
        "no fingerprint unless asked: {said}"
    );

    let (said, refused) = server.tool("list", json!({"folders": [notes], "fingerprint": true}));
    assert!(!refused, "{said}");
    assert!(
        said.contains(&format!("  {}  invented.md\n", fingerprint(b"four"))),
        "{said}"
    );
}

#[test]
fn a_list_that_fails_in_part_is_an_error_with_the_rest_still_listed() {
    let place = Place::new();
    fs::write(place.file(), "four").unwrap();
    let notes = place.root.join("org/notes");
    let gone = place.root.join("org/gone");
    let mut server = place.serve();
    let (said, refused) = server.tool(
        "list",
        json!({"folders": [gone.to_str().unwrap(), notes.to_str().unwrap()]}),
    );
    assert!(refused);
    assert!(said.starts_with("exit code 6\n"), "{said}");
    assert!(said.contains("invented.md"), "{said}");
    let outside = place.root.with_extension("outside");
    let (said, refused) = server.tool("list", json!({"folders": [outside.to_str().unwrap()]}));
    assert!(refused);
    assert!(said.starts_with("exit code 2\n"), "{said}");
}

#[test]
fn a_folder_named_like_an_option_stays_a_folder() {
    let place = Place::new();
    let mut server = place.serve();
    for folder in ["--all", "--depth", "--help"] {
        let (said, refused) = server.tool("list", json!({"folders": [folder]}));
        assert!(refused, "{folder}: {said}");
        assert!(said.starts_with("exit code 6\n"), "{folder}: {said}");
    }
}

#[test]
fn wrong_list_fields_get_an_error_and_the_server_goes_on() {
    let place = Place::new();
    let mut server = place.serve();
    let notes = place.root.join("org/notes");
    let notes = notes.to_str().unwrap();
    for arguments in [
        json!({}),
        json!({"folders": []}),
        json!({"folders": notes}),
        json!({"folders": [1]}),
        json!({"folders": [notes], "depth": 0}),
        json!({"folders": [notes], "depth": "deep"}),
        json!({"folders": [notes], "all": "yes"}),
        json!({"folders": [notes], "fingerprint": "yes"}),
    ] {
        let answer = server.ask(
            "tools/call",
            json!({"name": "list", "arguments": arguments}),
        );
        assert_eq!(answer["error"]["code"], -32602, "{arguments}");
    }
    assert_eq!(server.ask("ping", json!({}))["result"], json!({}));
}

#[test]
fn a_patch_changes_part_of_the_file() {
    let place = Place::new();
    fs::write(place.file(), "date,km\n2026-11-07,3.8\n").unwrap();
    let mut server = place.serve();
    server.read(&place.f(), "chat a");
    let (said, refused) = server.tool(
        "patch",
        json!({"file": place.f(), "who": "chat a",
               "edits": [{"old": "2026-11-07,3.8\n", "new": "2026-11-07,4.1\n"}],
               "append": "2026-11-08,7.5\n"}),
    );
    assert!(!refused, "{said}");
    assert_eq!(
        said,
        "patched: org/notes/invented.md, 1 edit, 1 append, 38 bytes\n"
    );
    assert_eq!(
        fs::read(place.file()).unwrap(),
        b"date,km\n2026-11-07,4.1\n2026-11-08,7.5\n"
    );
    let log = fs::read_to_string(place.log()).unwrap();
    assert!(log.contains("patched") && log.contains("chat a"), "{log}");
}

#[test]
fn a_patch_that_does_not_fit_is_refused_with_exit_11() {
    let place = Place::new();
    fs::write(place.file(), "a\na\n").unwrap();
    let mut server = place.serve();
    server.read(&place.f(), "chat a");
    let (said, refused) = server.tool(
        "patch",
        json!({"file": place.f(), "who": "chat a", "edits": [{"old": "a\n", "new": "b\n"}]}),
    );
    assert!(refused);
    assert!(said.starts_with("exit code 11\n"), "{said}");
    assert!(said.contains("at lines 1 and 2"), "{said}");
    assert_eq!(fs::read(place.file()).unwrap(), b"a\na\n");
}

#[test]
fn a_patch_of_an_unread_file_is_refused_with_exit_5() {
    let place = Place::new();
    fs::write(place.file(), "a\n").unwrap();
    let (said, refused) = place.serve().tool(
        "patch",
        json!({"file": place.f(), "who": "chat a", "append": "b\n"}),
    );
    assert!(refused);
    assert!(said.starts_with("exit code 5\n"), "{said}");
    assert_eq!(fs::read(place.file()).unwrap(), b"a\n");
}

#[test]
fn the_delete_tool_has_the_rules_of_the_command() {
    let place = Place::new();
    fs::write(place.file(), "old\n").unwrap();
    let file = place.f();

    // With the setting empty, every delete is refused.
    let mut server = place.serve();
    server.read(&file, "chat a");
    let (said, refused) = server.tool("delete", json!({"file": file, "who": "chat a"}));
    assert!(refused && said.starts_with("exit code 2"), "{said}");
    assert!(place.file().exists());
    drop(server);

    let mut server = place.serve_with(&[("ORGTHREAD_DELETE_ALLOW", "org/notes")]);
    // Another name read nothing: refused, exit code 4.
    let (said, refused) = server.tool("delete", json!({"file": file, "who": "chat b"}));
    assert!(refused && said.starts_with("exit code 4"), "{said}");
    // A check deletes nothing.
    server.read(&file, "chat a");
    let args = json!({"file": file, "who": "chat a", "check": true});
    let (said, refused) = server.tool("delete", args);
    assert!(!refused, "{said}");
    assert!(place.file().exists());
    // The fingerprint from a list works for any name.
    let args = json!({"file": file, "who": "chat b", "expect": fingerprint(b"old\n")});
    let (said, refused) = server.tool("delete", args);
    assert!(!refused, "{said}");
    assert!(
        said.starts_with("deleted: org/notes/invented.md, 4 bytes"),
        "{said}"
    );
    assert!(!place.file().exists());
}

fn pair(from: &str, to: &str) -> Value {
    json!({"from": from, "to": to})
}

#[test]
fn the_copy_tool_copies_each_pair_and_names_each_refusal() {
    let place = Place::new();
    fs::write(place.file(), "Grüße\n").unwrap();
    fs::write(place.root.join("org/notes/taken.md"), "old\n").unwrap();
    let file = place.f();
    let new = place.root.join("org/new/deep/b.md");
    let taken = place.root.join("org/notes/taken.md");
    let mut server = place.serve();
    let pairs = json!([
        pair(&file, new.to_str().unwrap()),
        pair(&file, taken.to_str().unwrap()),
        {"from": file, "to": place.root.join("org/c.md").to_str().unwrap(), "fingerprint": fingerprint(b"other")},
        pair(&file, place.root.join("org/d.md").to_str().unwrap()),
    ]);
    let (said, refused) = server.tool("copy", json!({"pairs": pairs, "who": "chat"}));
    assert!(refused, "{said}");
    let lines: Vec<&str> = said.lines().collect();
    assert_eq!(lines[0], "exit code 5", "{said}");
    let print = fingerprint("Grüße\n".as_bytes());
    assert_eq!(
        lines[1],
        format!(
            "copied: org/notes/invented.md -> org/new/deep/b.md, 8 bytes, {print}, new folders: org/new, org/new/deep"
        )
    );
    assert!(
        lines[2].starts_with(&format!(
            "{file} -> {}  refused, exit code 5: refused, nothing copied.",
            taken.display()
        )),
        "{said}"
    );
    assert!(lines[3].contains("refused, exit code 4"), "{said}");
    assert!(
        lines[4].starts_with("copied: "),
        "the pairs after a refusal still run: {said}"
    );
    assert_eq!(fs::read(&new).unwrap(), "Grüße\n".as_bytes());
    assert_eq!(fs::read(&taken).unwrap(), b"old\n");
    assert!(!place.root.join("org/c.md").exists());
    let log = fs::read_to_string(place.log()).unwrap();
    assert_eq!(log.lines().filter(|l| l.contains("\tcopied\t")).count(), 2);
    assert_eq!(
        log.lines().count(),
        6,
        "one log line per pair, and one per new folder: {log}"
    );
}

#[test]
fn the_copy_tool_takes_25_pairs_and_refuses_26() {
    let place = Place::new();
    fs::write(place.file(), "text\n").unwrap();
    let file = place.f();
    let mut server = place.serve();
    let pairs = |n: usize| -> Vec<Value> {
        (0..n)
            .map(|i| {
                pair(
                    &file,
                    place
                        .root
                        .join(format!("org/many/{i}.md"))
                        .to_str()
                        .unwrap(),
                )
            })
            .collect()
    };
    let answer = server.ask(
        "tools/call",
        json!({"name": "copy", "arguments": {"pairs": pairs(26), "who": "chat"}}),
    );
    assert_eq!(answer["error"]["code"], -32602, "{answer}");
    assert!(
        !place.root.join("org/many").exists(),
        "26 pairs copy nothing"
    );
    let (said, refused) = server.tool("copy", json!({"pairs": pairs(25), "who": "chat"}));
    assert!(!refused, "{said}");
    assert_eq!(said.lines().count(), 25);
    assert_eq!(
        fs::read_dir(place.root.join("org/many")).unwrap().count(),
        25
    );
    // No pairs, a pair with no to, or no who: wrong use, and nothing copied.
    for arguments in [
        json!({"pairs": [], "who": "chat"}),
        json!({"pairs": [{"from": file}], "who": "chat"}),
        json!({"pairs": [pair(&file, "x")]}),
        json!({"pairs": [pair(&file, "x"), {"from": file, "to": 7}], "who": "chat"}),
    ] {
        let answer = server.ask(
            "tools/call",
            json!({"name": "copy", "arguments": arguments}),
        );
        assert_eq!(answer["error"]["code"], -32602, "{answer}");
    }
    assert!(!place.root.join("x").exists());
}

#[test]
fn a_copy_path_named_like_an_option_stays_a_path() {
    let place = Place::new();
    fs::write(place.file(), "text\n").unwrap();
    let mut server = place.serve();
    let pairs = json!([pair(&place.f(), "--who")]);
    let (said, refused) = server.tool("copy", json!({"pairs": pairs, "who": "chat"}));
    // A plain name, made in the folder of the server, which is outside the root.
    assert!(refused, "{said}");
    assert!(said.contains("is outside the folder"), "{said}");
}

#[test]
fn a_copy_pair_with_an_empty_fingerprint_is_refused_for_that_pair() {
    let place = Place::new();
    fs::write(place.file(), "text\n").unwrap();
    let file = place.f();
    let b = place.root.join("org/b.md");
    let c = place.root.join("org/c.md");
    let mut server = place.serve();
    let pairs = json!([
        {"from": file, "to": b.to_str().unwrap(), "fingerprint": ""},
        pair(&file, c.to_str().unwrap()),
    ]);
    let (said, refused) = server.tool("copy", json!({"pairs": pairs, "who": "chat"}));
    assert!(refused, "{said}");
    let lines: Vec<&str> = said.lines().collect();
    assert_eq!(lines[0], "exit code 2", "{said}");
    assert!(
        lines[1].ends_with("refused, exit code 2: --fingerprint needs the fingerprint of the source, and it is empty"),
        "{said}"
    );
    assert!(lines[2].starts_with("copied: "), "{said}");
    assert!(!b.exists());
    assert!(c.exists());
}

#[test]
fn the_delete_tool_refuses_an_empty_expect_as_wrong_use() {
    let place = Place::new();
    fs::write(place.file(), "old\n").unwrap();
    let file = place.f();
    let mut server = place.serve_with(&[("ORGTHREAD_DELETE_ALLOW", "org/notes")]);
    server.read(&file, "chat");
    for args in [
        json!({"file": file, "who": "chat", "expect": ""}),
        json!({"file": file, "who": "chat", "expect": " ", "check": true}),
    ] {
        let (said, refused) = server.tool("delete", args);
        assert!(refused, "{said}");
        assert!(
            said.starts_with(
                "exit code 2\norgthread: --expect needs the fingerprint you saw, and it is empty"
            ),
            "{said}"
        );
    }
    assert!(place.file().exists());
    assert!(
        !fs::read_to_string(place.log())
            .unwrap_or_default()
            .contains("delete")
    );
}

#[test]
fn the_move_tool_moves_each_pair_and_names_each_refusal() {
    let place = Place::new();
    fs::write(place.file(), "Grüße\n").unwrap();
    fs::write(place.root.join("org/notes/second.md"), "two\n").unwrap();
    fs::write(place.root.join("org/notes/taken.md"), "old\n").unwrap();
    let file = place.f();
    let second = place.root.join("org/notes/second.md");
    let new = place.root.join("org/new/deep/plan.md");
    let taken = place.root.join("org/notes/taken.md");
    let mut server = place.serve();
    let pairs = json!([
        pair(&file, new.to_str().unwrap()),
        pair(second.to_str().unwrap(), taken.to_str().unwrap()),
        {"from": second, "to": place.root.join("org/c.md").to_str().unwrap(), "fingerprint": fingerprint(b"other")},
        pair(second.to_str().unwrap(), place.root.join("org/notes/second-b.md").to_str().unwrap()),
    ]);
    let (said, refused) = server.tool("move", json!({"pairs": pairs, "who": "chat"}));
    assert!(refused, "{said}");
    let lines: Vec<&str> = said.lines().collect();
    assert_eq!(lines[0], "exit code 5", "{said}");
    let print = fingerprint("Grüße\n".as_bytes());
    assert_eq!(
        lines[1],
        format!(
            "moved: org/notes/invented.md -> org/new/deep/plan.md, 8 bytes, {print}, new folders: org/new, org/new/deep"
        )
    );
    assert_eq!(
        lines[2],
        "note: the file name changed, so wikilinks to [[invented]] break. Fix them to the new name."
    );
    assert!(
        lines[3].starts_with(&format!(
            "{} -> {}  refused, exit code 5: refused, nothing moved.",
            second.display(),
            taken.display()
        )),
        "{said}"
    );
    assert!(lines[4].contains("refused, exit code 4"), "{said}");
    assert!(
        lines[5].starts_with("moved: org/notes/second.md -> org/notes/second-b.md"),
        "the pairs after a refusal still run: {said}"
    );
    assert_eq!(fs::read(&new).unwrap(), "Grüße\n".as_bytes());
    assert!(!place.file().exists());
    assert_eq!(fs::read(&taken).unwrap(), b"old\n");
    assert!(!second.exists());
    assert!(!place.root.join("org/c.md").exists());
    let log = fs::read_to_string(place.log()).unwrap();
    assert_eq!(log.lines().filter(|l| l.contains("\tmoved\t")).count(), 2);
    assert_eq!(
        log.lines()
            .filter(|l| l.contains("\tmade-folder\t"))
            .count(),
        2
    );
    assert_eq!(
        log.lines().count(),
        6,
        "one log line per pair, and one per new folder: {log}"
    );
}

#[test]
fn the_move_tool_takes_100_pairs_and_refuses_101() {
    let place = Place::new();
    let mut server = place.serve();
    let pairs = |n: usize| -> Vec<Value> {
        (0..n)
            .map(|i| {
                let from = place.root.join(format!("org/notes/{i}.md"));
                let to = place.root.join(format!("org/many/{i}.md"));
                pair(from.to_str().unwrap(), to.to_str().unwrap())
            })
            .collect()
    };
    for i in 0..101 {
        fs::write(place.root.join(format!("org/notes/{i}.md")), "text\n").unwrap();
    }
    let answer = server.ask(
        "tools/call",
        json!({"name": "move", "arguments": {"pairs": pairs(101), "who": "chat"}}),
    );
    assert_eq!(answer["error"]["code"], -32602, "{answer}");
    assert!(
        !place.root.join("org/many").exists(),
        "101 pairs move nothing"
    );
    let (said, refused) = server.tool("move", json!({"pairs": pairs(100), "who": "chat"}));
    assert!(!refused, "{said}");
    assert_eq!(said.lines().count(), 100);
    assert_eq!(
        fs::read_dir(place.root.join("org/many")).unwrap().count(),
        100
    );
    // No pairs, a pair with no from or no to, or no who: wrong use, and nothing moved.
    let left = place.root.join("org/notes/100.md");
    let left = left.to_str().unwrap();
    for arguments in [
        json!({"pairs": [], "who": "chat"}),
        json!({"pairs": [{"from": left}], "who": "chat"}),
        json!({"pairs": [{"to": "x"}], "who": "chat"}),
        json!({"pairs": [pair(left, "x")]}),
        json!({"pairs": [pair(left, "x"), {"from": left, "to": 7}], "who": "chat"}),
    ] {
        let answer = server.ask(
            "tools/call",
            json!({"name": "move", "arguments": arguments}),
        );
        assert_eq!(answer["error"]["code"], -32602, "{answer}");
    }
    assert!(place.root.join("org/notes/100.md").exists());
    assert!(!place.root.join("x").exists());
}
