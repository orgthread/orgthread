//! Files in Dropbox, through its web API, behind the same interface as a folder.
//!
//! The requests go through the `curl` program, so this needs no web library. The
//! token stays in the environment: it never appears in a command line, a file or
//! a message. Text goes through a pipe and is not written to disk, with one
//! exception: a rescue, what a failed write found in the file, stays beside the
//! program until its owner settles it on the screen.
//!
//! The locks are small local files beside the program: a lock costs no web call.
//! Between writers elsewhere, Dropbox's own version check decides.

use crate::storage::{Entry, Expect, Listing, LocalFolder, Revision, Storage, WriteError};
use serde_json::{Value, json};
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

pub const TOKEN: &str = "ORGTHREAD_DROPBOX_TOKEN";
/// For a service: these three get a fresh short-lived token for each command.
pub const REFRESH: [&str; 3] = [
    "ORGTHREAD_DROPBOX_REFRESH",
    "ORGTHREAD_DROPBOX_APP_KEY",
    "ORGTHREAD_DROPBOX_APP_SECRET",
];

pub struct Dropbox {
    token: String,
    api: String,
    content: String,
    /// Only its lock and rescue steps are used: every file goes to Dropbox.
    locks: LocalFolder,
}

struct Reply {
    status: u16,
    body: Vec<u8>,
}

struct Meta {
    rev: String,
    hash: String,
}

fn failure(message: String) -> io::Error {
    io::Error::other(message)
}

/// A header can only carry plain ASCII, so other signs are written as `\uXXXX`.
fn ascii_json(value: &Value) -> String {
    let mut out = String::new();
    for c in value.to_string().chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

/// One entry of a `list_folder` answer. `None` for a deleted entry or an odd one.
fn entry_of(v: &Value) -> Option<Entry> {
    let name = v["name"].as_str()?.to_string();
    match v[".tag"].as_str()? {
        "folder" => Some(Entry {
            name,
            folder: true,
            size: None,
            modified: None,
            hash: None,
        }),
        "file" => Some(Entry {
            name,
            folder: false,
            size: v["size"].as_u64(),
            modified: v["server_modified"]
                .as_str()
                .and_then(|t| t.parse::<jiff::Timestamp>().ok())
                .map(|t| t.as_second()),
            hash: v["content_hash"].as_str().map(str::to_string),
        }),
        _ => None,
    }
}

/// The entries of one page of a `list_folder` answer, its cursor, and whether
/// more pages follow.
fn page_of(body: &[u8]) -> io::Result<(Vec<Entry>, String, bool)> {
    let v: Value = serde_json::from_slice(body)?;
    let entries = v["entries"]
        .as_array()
        .ok_or_else(|| failure("Dropbox gave no entries".to_string()))?
        .iter()
        .filter_map(entry_of)
        .collect();
    let cursor = v["cursor"].as_str().unwrap_or_default().to_string();
    Ok((entries, cursor, v["has_more"].as_bool().unwrap_or(false)))
}

/// The versions in a `list_revisions` answer, newest first, as Dropbox gives them.
fn revisions_of(body: &[u8]) -> io::Result<Vec<Revision>> {
    let v: Value = serde_json::from_slice(body)?;
    let entries = v["entries"]
        .as_array()
        .ok_or_else(|| failure("Dropbox gave no revisions".to_string()))?;
    Ok(entries
        .iter()
        .filter_map(|e| {
            Some(Revision {
                rev: e["rev"].as_str()?.to_string(),
                hash: e["content_hash"].as_str()?.to_string(),
                size: e["size"].as_u64()?,
                modified: e["server_modified"]
                    .as_str()
                    .and_then(|t| t.parse::<jiff::Timestamp>().ok())
                    .map(|t| t.as_second()),
            })
        })
        .collect())
}

fn summary(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v["error_summary"].as_str().map(str::to_string))
        .unwrap_or_else(|| String::from_utf8_lossy(body).chars().take(200).collect())
}

/// A failure that no step can recover from, in plain words.
fn refused(reply: &Reply) -> io::Error {
    match reply.status {
        401 => failure("Dropbox refused the token: it is wrong or has expired".to_string()),
        status => failure(format!(
            "Dropbox answered {status}: {}",
            summary(&reply.body)
        )),
    }
}

impl Dropbox {
    pub fn new(lock_dir: &Path) -> Result<Self, String> {
        let url = |name: &str, default: &str| std::env::var(name).unwrap_or(default.to_string());
        let mut dropbox = Dropbox {
            token: String::new(),
            api: url("ORGTHREAD_DROPBOX_API", "https://api.dropboxapi.com"),
            content: url(
                "ORGTHREAD_DROPBOX_CONTENT",
                "https://content.dropboxapi.com",
            ),
            locks: LocalFolder::new(lock_dir, ""),
        };
        dropbox.token = match std::env::var(TOKEN).ok().filter(|t| !t.is_empty()) {
            Some(token) => token,
            None => dropbox.fresh_token()?,
        };
        Ok(dropbox)
    }

    /// Text for a form field: everything but plain signs written as `%XX`.
    fn form(text: &str) -> String {
        text.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect()
    }

    fn fresh_token(&self) -> Result<String, String> {
        let [refresh, key, secret] = REFRESH.map(|name| std::env::var(name).unwrap_or_default());
        if refresh.is_empty() || key.is_empty() || secret.is_empty() {
            return Err(format!(
                "{TOKEN} is not set, and neither are the {} values",
                REFRESH[0]
            ));
        }
        let body = format!(
            "grant_type=refresh_token&refresh_token={}&client_id={}&client_secret={}",
            Self::form(&refresh),
            Self::form(&key),
            Self::form(&secret)
        );
        let url = format!("{}/oauth2/token", self.api);
        let headers = ["Content-Type: application/x-www-form-urlencoded".to_string()];
        let reply = self
            .call(&url, &headers, Some(body.as_bytes()))
            .map_err(|e| e.to_string())?;
        if reply.status != 200 {
            return Err(format!("Dropbox gave no token: {}", reply.status));
        }
        serde_json::from_slice::<Value>(&reply.body)
            .ok()
            .and_then(|v| v["access_token"].as_str().map(str::to_string))
            .ok_or_else(|| "Dropbox gave no token".to_string())
    }

    fn file(path: &str) -> String {
        format!("/{path}")
    }

    fn call(&self, url: &str, headers: &[String], body: Option<&[u8]>) -> io::Result<Reply> {
        let mut curl = Command::new("curl");
        curl.args(["-sS", "-X", "POST", "--max-time", "120"])
            .args(["-w", "\n%{http_code}"]);
        // Only the request for a fresh token goes without one.
        if !self.token.is_empty() {
            curl.args(["--variable", &format!("%{TOKEN}")]).args([
                "--expand-header",
                &format!("Authorization: Bearer {{{{{TOKEN}}}}}"),
            ]);
        }
        for header in headers {
            curl.args(["-H", header]);
        }
        if body.is_some() {
            curl.args(["--data-binary", "@-"]);
        }
        curl.env(TOKEN, &self.token)
            .arg(url)
            .stdin(if body.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = curl
            .spawn()
            .map_err(|e| failure(format!("could not start curl: {e}")))?;
        let stdin = child.stdin.take();
        let output = std::thread::scope(|scope| {
            if let (Some(mut stdin), Some(body)) = (stdin, body) {
                scope.spawn(move || {
                    let _ = stdin.write_all(body);
                });
            }
            child.wait_with_output()
        })?;
        if !output.status.success() {
            let text = String::from_utf8_lossy(&output.stderr);
            return Err(failure(format!("curl failed: {}", text.trim())));
        }
        let split = output
            .stdout
            .iter()
            .rposition(|b| *b == b'\n')
            .ok_or_else(|| failure("curl gave no status".to_string()))?;
        let status = String::from_utf8_lossy(&output.stdout[split + 1..])
            .parse()
            .map_err(|_| failure("curl gave no status".to_string()))?;
        Ok(Reply {
            status,
            body: output.stdout[..split].to_vec(),
        })
    }

    fn rpc(&self, endpoint: &str, arg: &Value) -> io::Result<Reply> {
        let url = format!("{}/2/files/{endpoint}", self.api);
        let headers = ["Content-Type: application/json".to_string()];
        self.call(&url, &headers, Some(arg.to_string().as_bytes()))
    }

    fn content(&self, endpoint: &str, arg: &Value, body: Option<&[u8]>) -> io::Result<Reply> {
        let url = format!("{}/2/files/{endpoint}", self.content);
        let mut headers = vec![format!("Dropbox-API-Arg: {}", ascii_json(arg))];
        if body.is_some() {
            headers.push("Content-Type: application/octet-stream".to_string());
        }
        self.call(&url, &headers, body)
    }

    fn download(&self, full: &str) -> io::Result<Option<Vec<u8>>> {
        let reply = self.content("download", &json!({ "path": full }), None)?;
        match reply.status {
            200 => Ok(Some(reply.body)),
            409 if summary(&reply.body).contains("not_found") => Ok(None),
            _ => Err(refused(&reply)),
        }
    }

    fn meta(&self, full: &str) -> io::Result<Option<Meta>> {
        let reply = self.rpc("get_metadata", &json!({ "path": full }))?;
        match reply.status {
            200 => {
                let v: Value = serde_json::from_slice(&reply.body)?;
                if v[".tag"] != "file" {
                    return Err(failure(format!("{full} is not a file")));
                }
                let text = |key: &str| v[key].as_str().unwrap_or_default().to_string();
                Ok(Some(Meta {
                    rev: text("rev"),
                    hash: text("content_hash"),
                }))
            }
            409 if summary(&reply.body).contains("not_found") => Ok(None),
            _ => Err(refused(&reply)),
        }
    }

    /// False means that the file is not as `mode` asks, and nothing was written.
    fn upload(&self, full: &str, mode: Value, bytes: &[u8]) -> io::Result<bool> {
        // Decision of 2026-11-06: no renamed copy on a clash, ever.
        let arg = json!({
            "path": full,
            "mode": mode,
            "autorename": false,
            "mute": true,
            "strict_conflict": true,
        });
        let reply = self.content("upload", &arg, Some(bytes))?;
        match reply.status {
            200 => Ok(true),
            409 if summary(&reply.body).contains("conflict") => Ok(false),
            _ => Err(refused(&reply)),
        }
    }

    fn delete(&self, full: &str, rev: Option<&str>) -> io::Result<bool> {
        let arg = match rev {
            Some(rev) => json!({ "path": full, "parent_rev": rev }),
            None => json!({ "path": full }),
        };
        let reply = self.rpc("delete_v2", &arg)?;
        match reply.status {
            200 => Ok(true),
            409 => {
                let text = summary(&reply.body);
                if text.contains("not_found") || text.contains("conflict") {
                    Ok(false)
                } else {
                    Err(refused(&reply))
                }
            }
            _ => Err(refused(&reply)),
        }
    }
}

impl Dropbox {
    /// For the live check only: one `list_revisions` call with any limit, and
    /// its status with the error summary or the number of versions. It reads.
    #[doc(hidden)]
    pub fn probe_revisions(&self, path: &str, limit: u64) -> io::Result<(u16, String)> {
        let arg = json!({ "path": Self::file(path), "mode": "path", "limit": limit });
        let reply = self.rpc("list_revisions", &arg)?;
        let text = match reply.status {
            200 => format!("{} versions", revisions_of(&reply.body)?.len()),
            _ => summary(&reply.body),
        };
        Ok((reply.status, text))
    }
}

impl Storage for Dropbox {
    fn list(&self, path: &str, max: usize) -> io::Result<Listing> {
        // Dropbox names its root with an empty path.
        let full = if path.is_empty() {
            String::new()
        } else {
            Self::file(path)
        };
        let arg = json!({ "path": full, "recursive": false, "include_deleted": false });
        let reply = self.rpc("list_folder", &arg)?;
        if reply.status == 409 {
            let text = summary(&reply.body);
            if text.contains("not_found") {
                return Ok(Listing::Missing);
            }
            if text.contains("not_folder") {
                let reply = self.rpc("get_metadata", &json!({ "path": full }))?;
                if reply.status != 200 {
                    return Err(refused(&reply));
                }
                let v: Value = serde_json::from_slice(&reply.body)?;
                return entry_of(&v)
                    .map(Listing::File)
                    .ok_or_else(|| failure(format!("{full} is neither a file nor a folder")));
            }
        }
        if reply.status != 200 {
            return Err(refused(&reply));
        }
        let (mut entries, mut cursor, mut more) = page_of(&reply.body)?;
        // Each page costs a web call, so it stops once the list is full.
        while more && entries.len() <= max {
            let reply = self.rpc("list_folder/continue", &json!({ "cursor": cursor }))?;
            if reply.status != 200 {
                return Err(refused(&reply));
            }
            let (next, next_cursor, next_more) = page_of(&reply.body)?;
            entries.extend(next);
            (cursor, more) = (next_cursor, next_more);
        }
        Ok(Listing::Folder {
            entries,
            complete: !more,
        })
    }

    fn read(&self, path: &str) -> io::Result<Option<Vec<u8>>> {
        self.download(&Self::file(path))
    }

    /// One call: Dropbox lists at most 100 versions of a file, newest first.
    fn revisions(&self, path: &str, limit: usize) -> io::Result<Vec<Revision>> {
        let arg = json!({ "path": Self::file(path), "mode": "path", "limit": limit.clamp(1, 100) });
        let reply = self.rpc("list_revisions", &arg)?;
        match reply.status {
            200 => revisions_of(&reply.body),
            409 if summary(&reply.body).contains("not_found") => Ok(Vec::new()),
            _ => Err(refused(&reply)),
        }
    }

    /// Dropbox takes `rev:` and the rev in place of a path, one version per call.
    fn read_revision(&self, rev: &str) -> io::Result<Option<Vec<u8>>> {
        if rev.is_empty() || !rev.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(failure(
                "this is not the name of a Dropbox version".to_string(),
            ));
        }
        self.download(&format!("rev:{rev}"))
    }

    fn size(&self, path: &str) -> io::Result<Option<u64>> {
        let full = Self::file(path);
        let reply = self.rpc("get_metadata", &json!({ "path": full }))?;
        match reply.status {
            200 => {
                let v: Value = serde_json::from_slice(&reply.body)?;
                if v[".tag"] != "file" {
                    return Err(failure(format!("{full} is not a file")));
                }
                v["size"]
                    .as_u64()
                    .map(Some)
                    .ok_or_else(|| failure(format!("Dropbox gave no size for {full}")))
            }
            409 if summary(&reply.body).contains("not_found") => Ok(None),
            _ => Err(refused(&reply)),
        }
    }

    /// The bytes pass through in parts, from curl to `out`. There is no time
    /// limit for the whole transfer, as a large file takes long: only a transfer
    /// that stalls for 60 seconds is stopped.
    fn read_to(&self, path: &str, out: &mut dyn Write) -> io::Result<bool> {
        if self.size(path)?.is_none() {
            return Ok(false);
        }
        let url = format!("{}/2/files/download", self.content);
        let arg = ascii_json(&json!({ "path": Self::file(path) }));
        let mut child = Command::new("curl")
            .args(["-sS", "-f", "-X", "POST", "--connect-timeout", "30"])
            .args(["--speed-limit", "1", "--speed-time", "60"])
            .args(["--variable", &format!("%{TOKEN}")])
            .args([
                "--expand-header",
                &format!("Authorization: Bearer {{{{{TOKEN}}}}}"),
            ])
            .args(["-H", &format!("Dropbox-API-Arg: {arg}")])
            .env(TOKEN, &self.token)
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| failure(format!("could not start curl: {e}")))?;
        let copied = match child.stdout.take() {
            Some(mut stdout) => io::copy(&mut stdout, out).map(|_| ()),
            None => Err(failure("curl gave no output".to_string())),
        };
        let output = child.wait_with_output()?;
        if !output.status.success() {
            let text = String::from_utf8_lossy(&output.stderr);
            return Err(failure(format!("curl failed: {}", text.trim())));
        }
        copied.map(|()| true)
    }

    fn write_if_unchanged(
        &self,
        path: &str,
        expect: &Expect,
        bytes: &[u8],
    ) -> Result<(), WriteError> {
        let full = Self::file(path);
        let written = match expect {
            Expect::Absent => self.upload(&full, json!("add"), bytes)?,
            Expect::Fingerprint(want) => {
                let Some(meta) = self.meta(&full)? else {
                    return Err(WriteError::Changed { found: None });
                };
                if meta.hash != *want {
                    return Err(WriteError::Changed {
                        found: Some(meta.hash),
                    });
                }
                // Dropbox itself refuses this if another version came first.
                self.upload(
                    &full,
                    json!({ ".tag": "update", "update": meta.rev }),
                    bytes,
                )?
            }
        };
        if written {
            return Ok(());
        }
        let found = self.meta(&full)?.map(|meta| meta.hash);
        Err(WriteError::Changed { found })
    }

    fn restore(&self, path: &str, old: Option<&[u8]>) -> io::Result<()> {
        let full = Self::file(path);
        match old {
            Some(bytes) => self.upload(&full, json!("overwrite"), bytes).map(|_| ()),
            None => self.delete(&full, None).map(|_| ()),
        }
    }

    fn rescue(&self, path: &str, bytes: &[u8]) -> io::Result<String> {
        self.locks.rescue(path, bytes)
    }

    fn lock_create(&self, name: &str, line: &str) -> io::Result<bool> {
        self.locks.lock_create(name, line)
    }

    fn lock_read(&self, name: &str) -> io::Result<Option<String>> {
        self.locks.lock_read(name)
    }

    fn lock_age(&self, name: &str) -> io::Result<Option<Duration>> {
        self.locks.lock_age(name)
    }

    fn lock_take_stale(&self, name: &str, max_age: Duration) -> io::Result<Option<String>> {
        self.locks.lock_take_stale(name, max_age)
    }

    fn lock_remove(&self, name: &str) -> io::Result<()> {
        self.locks.lock_remove(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_holds_only_ascii() {
        let arg = json!({ "path": "/org/Grüße 😀.md" });
        let text = ascii_json(&arg);
        assert!(text.is_ascii());
        assert!(text.contains("\\u00fc"));
        assert!(text.contains("\\ud83d\\ude00"));
        let back: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(back, arg);
    }

    #[test]
    fn a_form_field_writes_odd_signs_safely() {
        assert_eq!(Dropbox::form("a-b_c.d~1"), "a-b_c.d~1");
        assert_eq!(Dropbox::form("a b&c=ü"), "a%20b%26c%3D%C3%BC");
    }

    #[test]
    fn a_page_of_list_folder_becomes_entries() {
        let body = br#"{"entries":[
            {".tag":"file","name":"a.md","size":12,"server_modified":"2026-11-07T10:00:00Z","content_hash":"ab12"},
            {".tag":"folder","name":"drafts"},
            {".tag":"deleted","name":"gone.md"}
        ],"cursor":"next-page","has_more":true}"#;
        let (entries, cursor, more) = page_of(body).unwrap();
        assert_eq!(cursor, "next-page");
        assert!(more);
        assert_eq!(
            entries,
            [
                Entry {
                    name: "a.md".into(),
                    folder: false,
                    size: Some(12),
                    modified: Some(1_794_045_600),
                    hash: Some("ab12".into()),
                },
                Entry {
                    name: "drafts".into(),
                    folder: true,
                    size: None,
                    modified: None,
                    hash: None,
                },
            ]
        );
    }

    #[test]
    fn an_answer_of_list_revisions_becomes_versions() {
        let body = br#"{"is_deleted":false,"entries":[
            {".tag":"file","name":"plan.md","rev":"a1b2c3","size":40,"server_modified":"2026-11-07T10:00:00Z","content_hash":"new1"},
            {".tag":"file","name":"plan.md","rev":"a1b2c2","size":31,"content_hash":"old1"},
            {".tag":"file","name":"plan.md","size":5,"content_hash":"no-rev"}
        ]}"#;
        assert_eq!(
            revisions_of(body).unwrap(),
            [
                Revision {
                    rev: "a1b2c3".into(),
                    hash: "new1".into(),
                    size: 40,
                    modified: Some(1_794_045_600),
                },
                Revision {
                    rev: "a1b2c2".into(),
                    hash: "old1".into(),
                    size: 31,
                    modified: None,
                },
            ]
        );
        assert!(revisions_of(b"{}").is_err());
    }

    #[test]
    fn a_dropbox_error_gives_its_summary() {
        assert_eq!(
            summary(br#"{"error_summary":"path/not_found/.."}"#),
            "path/not_found/.."
        );
        assert_eq!(summary(b"plain text"), "plain text");
    }
}
