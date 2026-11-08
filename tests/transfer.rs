//! The commands download and upload, on invented files.

use orgthread::fingerprint::fingerprint;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

struct Place {
    root: PathBuf,
}

/// Bytes that are not text: a NUL sign and no valid UTF-8.
fn picture(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i * 7 % 256) as u8).collect()
}

impl Place {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let root =
            std::env::temp_dir().join(format!("orgthread-transfer-{}-{n}", std::process::id()));
        fs::create_dir_all(root.join(".locks")).unwrap();
        fs::create_dir_all(root.join("scans")).unwrap();
        fs::create_dir_all(root.join("notes")).unwrap();
        fs::create_dir_all(root.with_extension("outside")).unwrap();
        Place {
            root: fs::canonicalize(root).unwrap(),
        }
    }
    fn path(&self, name: &str) -> String {
        self.root.join(name).to_str().unwrap().to_string()
    }
    fn outside(&self, name: &str) -> String {
        self.root
            .with_extension("outside")
            .join(name)
            .to_str()
            .unwrap()
            .to_string()
    }
    fn log(&self) -> String {
        fs::read_to_string(self.outside("writes.log")).unwrap_or_default()
    }
    fn run(&self, args: &[&str], stdin: &[u8], env: &[(&str, &str)]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_orgthread"));
        c.args(args)
            .env("ORGTHREAD_ROOT", &self.root)
            .env("ORGTHREAD_LOCK_DIR", self.root.join(".locks"))
            .env("ORGTHREAD_LOG", self.outside("writes.log"))
            .env("ORGTHREAD_READS", self.outside("reads"))
            .env("ORGTHREAD_VERSIONS", self.outside("versions"))
            .env("ORGTHREAD_BINARY_ALLOW", "scans")
            .env_remove("ORGTHREAD_DOWNLOAD_MB")
            .env_remove("ORGTHREAD_UPLOAD_MB");
        for (name, value) in env {
            c.env(name, value);
        }
        let mut child = c
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        match child.stdin.take().unwrap().write_all(stdin) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::BrokenPipe => {}
            Err(e) => panic!("{e}"),
        }
        child.wait_with_output().unwrap()
    }
    fn upload(&self, name: &str, who: &str, bytes: &[u8]) -> Output {
        self.run(&["upload", &self.path(name), "--who", who], bytes, &[])
    }
    fn download(&self, name: &str, who: &str) -> Output {
        self.run(&["download", &self.path(name), "--who", who], b"", &[])
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
        let _ = fs::remove_dir_all(self.root.with_extension("outside"));
    }
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap()
}

fn said(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

#[test]
fn a_picture_goes_up_and_comes_back_exact() {
    let p = Place::new();
    let bytes = picture(300_000);
    let up = p.upload("scans/p1.jpg", "chat a", &bytes);
    assert_eq!(code(&up), 0, "{}", said(&up));
    assert_eq!(
        String::from_utf8_lossy(&up.stdout),
        "written: scans/p1.jpg, 300000 bytes\n"
    );
    assert_eq!(fs::read(p.path("scans/p1.jpg")).unwrap(), bytes);
    let down = p.download("scans/p1.jpg", "chat b");
    assert_eq!(code(&down), 0, "{}", said(&down));
    assert_eq!(down.stdout, bytes);
}

#[test]
fn download_to_a_copy_gives_the_same_bytes() {
    let p = Place::new();
    let bytes = picture(5_000_000);
    fs::write(p.path("scans/big.pdf"), &bytes).unwrap();
    let copy = p.outside("big.pdf");
    let o = p.run(
        &[
            "download",
            &p.path("scans/big.pdf"),
            "--who",
            "a",
            "--to",
            &copy,
        ],
        b"",
        &[],
    );
    assert_eq!(code(&o), 0, "{}", said(&o));
    assert_eq!(fs::read(&copy).unwrap(), bytes);
    // A copy that exists is never replaced.
    let again = p.run(
        &[
            "download",
            &p.path("scans/big.pdf"),
            "--who",
            "a",
            "--to",
            &copy,
        ],
        b"",
        &[],
    );
    assert_eq!(code(&again), 5);
}

#[test]
fn a_file_that_is_not_text_goes_only_to_the_named_folders() {
    let p = Place::new();
    let o = p.upload("notes/p1.jpg", "a", &picture(100));
    assert_eq!(code(&o), 2);
    assert!(said(&o).contains("ORGTHREAD_BINARY_ALLOW"), "{}", said(&o));
    assert!(!PathBuf::from(p.path("notes/p1.jpg")).exists());
    // With the setting empty, it goes nowhere.
    let none = p.run(
        &["upload", &p.path("scans/p1.jpg"), "--who", "a"],
        &picture(100),
        &[("ORGTHREAD_BINARY_ALLOW", "")],
    );
    assert_eq!(code(&none), 2);
}

#[test]
fn text_goes_wherever_write_may_write() {
    let p = Place::new();
    let o = p.upload("notes/a.md", "a", b"First line\n");
    assert_eq!(code(&o), 0, "{}", said(&o));
    assert_eq!(fs::read(p.path("notes/a.md")).unwrap(), b"First line\n");
}

#[test]
fn a_replace_needs_a_download_by_the_same_name_first() {
    let p = Place::new();
    assert_eq!(code(&p.upload("scans/p1.jpg", "a", &picture(100))), 0);
    let other = p.upload("scans/p1.jpg", "b", &picture(200));
    assert_eq!(code(&other), 5);
    assert_eq!(fs::read(p.path("scans/p1.jpg")).unwrap(), picture(100));
    assert_eq!(code(&p.download("scans/p1.jpg", "b")), 0);
    let after = p.upload("scans/p1.jpg", "b", &picture(200));
    assert_eq!(code(&after), 0, "{}", said(&after));
    assert_eq!(fs::read(p.path("scans/p1.jpg")).unwrap(), picture(200));
}

#[test]
fn a_file_changed_since_the_download_is_not_replaced() {
    let p = Place::new();
    fs::write(p.path("scans/p1.jpg"), picture(100)).unwrap();
    assert_eq!(code(&p.download("scans/p1.jpg", "a")), 0);
    fs::write(p.path("scans/p1.jpg"), picture(150)).unwrap();
    let o = p.upload("scans/p1.jpg", "a", &picture(200));
    assert_eq!(code(&o), 4);
    assert_eq!(fs::read(p.path("scans/p1.jpg")).unwrap(), picture(150));
}

#[test]
fn the_limits_refuse_and_move_nothing() {
    let p = Place::new();
    fs::write(p.path("scans/big.pdf"), picture(2_000_000)).unwrap();
    let down = p.run(
        &["download", &p.path("scans/big.pdf"), "--who", "a"],
        b"",
        &[("ORGTHREAD_DOWNLOAD_MB", "1")],
    );
    assert_eq!(code(&down), 2);
    assert!(down.stdout.is_empty());
    assert!(
        said(&down).contains("more than the limit of 1 MB"),
        "{}",
        said(&down)
    );
    let up = p.run(
        &["upload", &p.path("scans/p2.pdf"), "--who", "a"],
        &picture(1_000_001),
        &[("ORGTHREAD_UPLOAD_MB", "1")],
    );
    assert_eq!(code(&up), 2);
    assert!(!PathBuf::from(p.path("scans/p2.pdf")).exists());
    let bad = p.run(
        &["upload", &p.path("scans/p2.pdf"), "--who", "a"],
        b"x",
        &[("ORGTHREAD_UPLOAD_MB", "a lot")],
    );
    assert_eq!(code(&bad), 2);
}

#[test]
fn check_moves_nothing() {
    let p = Place::new();
    fs::write(p.path("scans/p1.jpg"), picture(1234)).unwrap();
    let down = p.run(
        &["download", "--check", &p.path("scans/p1.jpg"), "--who", "a"],
        b"",
        &[],
    );
    assert_eq!(code(&down), 0);
    assert_eq!(String::from_utf8_lossy(&down.stdout), "1234\n");
    // A check is no read: a replace is still refused.
    let up = p.run(
        &["upload", "--check", &p.path("scans/p1.jpg"), "--who", "a"],
        b"",
        &[],
    );
    assert_eq!(code(&up), 5);
    let new = p.run(
        &["upload", "--check", &p.path("scans/p2.jpg"), "--who", "a"],
        b"",
        &[],
    );
    assert_eq!(code(&new), 0);
    assert!(!PathBuf::from(p.path("scans/p2.jpg")).exists());
    assert!(p.log().is_empty());
}

#[test]
fn closed_and_missing_paths_are_refused() {
    let p = Place::new();
    fs::write(p.path("scans/p1.jpg"), picture(10)).unwrap();
    let closed = p.run(
        &["download", &p.path("scans/p1.jpg"), "--who", "a"],
        b"",
        &[("ORGTHREAD_DENY", "scans")],
    );
    assert_eq!(code(&closed), 2);
    let read_only = p.run(
        &["upload", &p.path("scans/p2.jpg"), "--who", "a"],
        &picture(10),
        &[("ORGTHREAD_READ_ONLY", "scans")],
    );
    assert_eq!(code(&read_only), 2);
    assert_eq!(code(&p.download("scans/none.pdf", "a")), 6);
    assert_eq!(code(&p.upload("scans/empty.jpg", "a", b"")), 2);
}

#[test]
fn each_transfer_leaves_one_log_line_with_its_fingerprint() {
    let p = Place::new();
    let bytes = picture(1000);
    assert_eq!(code(&p.upload("scans/p1.jpg", "a", &bytes)), 0);
    assert_eq!(code(&p.download("scans/p1.jpg", "b")), 0);
    let log = p.log();
    let lines: Vec<Vec<&str>> = log.lines().map(|l| l.split('\t').collect()).collect();
    assert_eq!(lines.len(), 2, "{log}");
    assert_eq!(lines[0][1], "written");
    assert_eq!(lines[1][1], "downloaded");
    assert_eq!(lines[1][2], "scans/p1.jpg");
    assert_eq!(lines[1][3], "b");
    assert_eq!(lines[1][5], fingerprint(&bytes));
    assert_eq!(lines[1][6], "1000");
}

#[test]
fn the_local_server_offers_no_links() {
    let p = Place::new();
    let o = p.run(
        &["serve"],
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"download\",\"arguments\":{\"files\":[\"/a\"],\"who\":\"a\"}}}\n",
        &[],
    );
    let out = String::from_utf8_lossy(&o.stdout);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert!(
        lines[0].contains("\"read\"") && !lines[0].contains("\"download\""),
        "{out}"
    );
    assert!(lines[1].contains("there is no tool download"), "{out}");
}
