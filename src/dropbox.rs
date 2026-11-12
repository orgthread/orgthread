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

use crate::settings;
use crate::storage::{
    Copied, CopyError, Entry, Expect, Keep, Listing, LocalFolder, Moved, RemoveError, Removed,
    Revision, Storage, Walked, WriteError, kept, new_folders,
};
use serde_json::{Value, json};
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

pub use crate::settings::DROPBOX_TOKEN as TOKEN;
/// For a service: these three get a fresh short-lived token for each command.
pub use crate::settings::REFRESH;

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
    /// The header `Dropbox-API-Result`, when the call asked for it.
    result: Option<String>,
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

/// The body, the header line if asked for, and the status, from curl's output.
fn split_reply(mut out: Vec<u8>, result: bool) -> io::Result<Reply> {
    let no_status = || failure("curl gave no status".to_string());
    let split = out
        .iter()
        .rposition(|b| *b == b'\n')
        .ok_or_else(no_status)?;
    let status = String::from_utf8_lossy(&out[split + 1..])
        .parse()
        .map_err(|_| no_status())?;
    out.truncate(split);
    let mut header = None;
    if result {
        let split = out
            .iter()
            .rposition(|b| *b == b'\n')
            .ok_or_else(no_status)?;
        let text = String::from_utf8_lossy(&out[split + 1..])
            .trim()
            .to_string();
        header = (!text.is_empty()).then_some(text);
        out.truncate(split);
    }
    Ok(Reply {
        status,
        body: out,
        result: header,
    })
}

/// The rev of a download, from its header `Dropbox-API-Result`, only when the
/// hash in the same header is the fingerprint of the bytes that came.
fn rev_of_download(header: Option<&str>, print: &str) -> Option<String> {
    let v: Value = serde_json::from_str(header?).ok()?;
    if v["content_hash"].as_str()? != print {
        return None;
    }
    v["rev"]
        .as_str()
        .filter(|r| !r.is_empty())
        .map(str::to_string)
}

/// What a `delete_v2` reply means for a remove of the file with fingerprint `want`.
fn removed_of(status: u16, body: &[u8], want: &str) -> Result<Removed, Deleted> {
    match status {
        200 => {
            let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
            let meta = &v["metadata"];
            let rev = meta["rev"].as_str().map(str::to_string);
            let size = meta["size"].as_u64();
            let hash = meta["content_hash"].as_str();
            let kind = meta[".tag"].as_str();
            let way_back = match &rev {
                Some(rev) => format!("Dropbox keeps it in Deleted files, rev {rev}"),
                None => "Dropbox keeps it in Deleted files".to_string(),
            };
            if kind != Some("file") || hash != Some(want) {
                return Err(Deleted::Unexpected(format!(
                    "Dropbox deleted the path, but reports {} with hash {}, not the file with {want}. {way_back}",
                    kind.unwrap_or("no kind"),
                    hash.unwrap_or("none")
                )));
            }
            Ok(Removed {
                size: size.unwrap_or_default(),
                rev,
            })
        }
        409 => {
            let text = summary(body);
            if text.contains("not_found") || text.contains("conflict") {
                Err(Deleted::Refused)
            } else {
                Err(Deleted::Error(text))
            }
        }
        _ => Err(Deleted::Error(format!("{status}"))),
    }
}

/// What a path holds, from `get_metadata`.
#[derive(Debug, PartialEq, Eq)]
enum Look {
    File {
        hash: String,
        size: u64,
        rev: String,
    },
    /// A folder, or anything else that is not a plain file.
    Folder,
    Missing,
}

fn look_of(v: &Value) -> Look {
    let text = |key: &str| v[key].as_str().unwrap_or_default().to_string();
    match v[".tag"].as_str() {
        Some("file") => Look::File {
            hash: text("content_hash"),
            size: v["size"].as_u64().unwrap_or_default(),
            rev: text("rev"),
        },
        _ => Look::Folder,
    }
}

/// What a 409 of `copy_v2` means, after the Dropbox docs of `RelocationError`:
/// `to/conflict/...` when something is at the target, `from_lookup/not_found/...`
/// with no source, and `from_lookup/not_file/...` for a source that is no file.
/// `None` for any other error, which is a failure.
fn copy_refusal(summary: &str) -> Option<CopyError> {
    if summary.starts_with("to/conflict") {
        Some(CopyError::Exists)
    } else if summary.starts_with("from_lookup/not_found") {
        Some(CopyError::Missing)
    } else if summary.starts_with("from_lookup/not_file") {
        Some(CopyError::Folder)
    } else {
        None
    }
}

/// What a 200 of `copy_v2` made: `{"metadata": {...}}`, the file at the target.
/// `None` for the hash when the answer gives none, and then the caller looks.
fn made_of(body: &[u8]) -> Result<(Option<String>, u64, Option<String>), String> {
    let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let meta = &v["metadata"];
    match meta[".tag"].as_str() {
        Some("file") => Ok((
            meta["content_hash"].as_str().map(str::to_string),
            meta["size"].as_u64().unwrap_or_default(),
            meta["rev"]
                .as_str()
                .filter(|r| !r.is_empty())
                .map(str::to_string),
        )),
        other => Err(other.unwrap_or("nothing").to_string()),
    }
}

/// Why a `delete_v2` deleted nothing, or deleted something else.
#[derive(Debug, PartialEq, Eq)]
enum Deleted {
    /// Gone, or another version: nothing was deleted.
    Refused,
    Unexpected(String),
    Error(String),
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

/// The path of an entry of a recursive `list_folder` from the listed folder, as
/// Dropbox shows it, and true for a folder. `None` for the folder itself, a
/// deleted entry or an odd one. `depth` is the number of parts of the listed
/// folder: Dropbox may show their case differently than the caller gave them.
fn deep_entry_of(v: &Value, depth: usize) -> Option<(String, bool)> {
    let folder = match v[".tag"].as_str()? {
        "folder" => true,
        "file" => false,
        _ => return None,
    };
    let shown = v["path_display"].as_str()?;
    let parts: Vec<&str> = shown.trim_start_matches('/').split('/').collect();
    if parts.len() <= depth {
        return None;
    }
    Some((parts[depth..].join("/"), folder))
}

/// One page of a recursive `list_folder`: each path and whether it is a folder,
/// the cursor, and whether more pages follow.
type DeepPage = (Vec<(String, bool)>, String, bool);

/// The entries of one page of a recursive `list_folder` answer, its cursor, and
/// whether more pages follow.
fn deep_page_of(body: &[u8], depth: usize) -> io::Result<DeepPage> {
    let v: Value = serde_json::from_slice(body)?;
    let entries = v["entries"]
        .as_array()
        .ok_or_else(|| failure("Dropbox gave no entries".to_string()))?
        .iter()
        .filter_map(|e| deep_entry_of(e, depth))
        .collect();
    let cursor = v["cursor"].as_str().unwrap_or_default().to_string();
    Ok((entries, cursor, v["has_more"].as_bool().unwrap_or(false)))
}

/// Each file of the zip of `download_zip` that `keep` takes, with its path from
/// the zipped folder. Dropbox puts the folder itself at the top of the zip, named
/// `top`: when every entry lies in it, that part goes. The zip and each file stay
/// in memory: nothing goes to a disk. A file above `limit` bytes is left out.
fn zip_texts(
    zip: &[u8],
    top: &str,
    limit: u64,
    keep: Keep,
    found: &mut dyn FnMut(&str, &[u8]),
) -> io::Result<()> {
    let wrong = |e: zip::result::ZipError| failure(format!("the zip of Dropbox is broken: {e}"));
    let mut archive = zip::ZipArchive::new(io::Cursor::new(zip)).map_err(wrong)?;
    let names = archive
        .file_names()
        .map(|n| n.map(|n| n.to_string()))
        .collect::<Result<Vec<String>, _>>()
        .map_err(wrong)?;
    let top = top.to_lowercase();
    let prefix = format!("{top}/");
    let strip = !top.is_empty()
        && names.iter().all(|n| {
            let n = n.to_lowercase();
            n.starts_with(&prefix) || n == top
        });
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(wrong)?;
        if file.is_dir() {
            continue;
        }
        let name = file
            .name()
            .map_err(wrong)?
            .trim_start_matches('/')
            .to_string();
        // Every name starts with the top folder and a `/`: the part after it.
        let inside = match name.split_once('/') {
            Some((_, rest)) if strip => rest,
            _ => name.as_str(),
        };
        if inside.is_empty() || !kept(inside, false, keep) || file.size() > limit {
            continue;
        }
        let mut bytes = Vec::new();
        io::Read::read_to_end(&mut io::Read::take(&mut file, limit + 1), &mut bytes)?;
        if bytes.len() as u64 > limit {
            continue;
        }
        found(inside, &bytes);
    }
    Ok(())
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
        let mut dropbox = Dropbox {
            token: String::new(),
            api: settings::dropbox_api(),
            content: settings::dropbox_content(),
            locks: LocalFolder::new(lock_dir, ""),
        };
        dropbox.token = match settings::dropbox_token() {
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
        let [refresh, key, secret] = settings::dropbox_refresh();
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
        self.call_full(url, headers, body, false, None)
    }

    /// With `result`, the reply also gives the header `Dropbox-API-Result`, on a
    /// line of its own before the status. That header never holds a newline.
    /// With `cap`, a reply body above that many bytes stops the call, with the
    /// error kind `FileTooLarge`, so that it never fills the memory.
    fn call_full(
        &self,
        url: &str,
        headers: &[String],
        body: Option<&[u8]>,
        result: bool,
        cap: Option<u64>,
    ) -> io::Result<Reply> {
        let mut curl = Command::new("curl");
        let format = if result {
            "\n%header{dropbox-api-result}\n%{http_code}"
        } else {
            "\n%{http_code}"
        };
        curl.args(["-sS", "-X", "POST", "--max-time", "120"])
            .args(["-w", format]);
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
            let Some(cap) = cap else {
                return child.wait_with_output();
            };
            let stderr = child.stderr.take();
            let said = scope.spawn(move || {
                let mut text = Vec::new();
                if let Some(mut stderr) = stderr {
                    let _ = io::Read::read_to_end(&mut stderr, &mut text);
                }
                text
            });
            // The status and the header come after the body: room for both.
            let room = cap + 1024;
            let mut stdout = Vec::new();
            if let Some(out) = child.stdout.take() {
                io::Read::read_to_end(&mut io::Read::take(out, room + 1), &mut stdout)?;
            }
            if stdout.len() as u64 > room {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::FileTooLarge,
                    format!("Dropbox gave more than {cap} bytes"),
                ));
            }
            let status = child.wait()?;
            let stderr = said.join().unwrap_or_default();
            Ok(std::process::Output {
                status,
                stdout,
                stderr,
            })
        })?;
        if !output.status.success() {
            let text = String::from_utf8_lossy(&output.stderr);
            return Err(failure(format!("curl failed: {}", text.trim())));
        }
        let reply = split_reply(output.stdout, result)?;
        if let Some(cap) = cap
            && reply.body.len() as u64 > cap
        {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                format!("Dropbox gave more than {cap} bytes"),
            ));
        }
        Ok(reply)
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
        Ok(self.download_rev(full)?.map(|(bytes, _)| bytes))
    }

    /// The bytes, and the rev that Dropbox gives in the same reply.
    fn download_rev(&self, full: &str) -> io::Result<Option<(Vec<u8>, Option<String>)>> {
        let url = format!("{}/2/files/download", self.content);
        let headers = [format!(
            "Dropbox-API-Arg: {}",
            ascii_json(&json!({ "path": full }))
        )];
        let reply = self.call_full(&url, &headers, None, true, None)?;
        match reply.status {
            200 => {
                let print = crate::fingerprint::fingerprint(&reply.body);
                let rev = rev_of_download(reply.result.as_deref(), &print);
                Ok(Some((reply.body, rev)))
            }
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

    /// What is at a path, from one `get_metadata` call.
    fn look(&self, full: &str) -> io::Result<Look> {
        let reply = self.rpc("get_metadata", &json!({ "path": full }))?;
        match reply.status {
            200 => {
                let v: Value = serde_json::from_slice(&reply.body)?;
                Ok(look_of(&v))
            }
            409 if summary(&reply.body).contains("not_found") => Ok(Look::Missing),
            _ => Err(refused(&reply)),
        }
    }

    /// The copy call got no answer, so Dropbox may have made the copy or not.
    /// One look at the target tells. Only the bytes of the source count as a copy:
    /// other bytes may be an older file, so they are never removed here.
    fn after_no_answer(
        &self,
        from: &str,
        to: &str,
        source: &str,
        folders: Vec<String>,
        cause: io::Error,
    ) -> Result<Copied, CopyError> {
        let look = match self.look(to) {
            Ok(look) => look,
            Err(e) => {
                return Err(CopyError::Io(failure(format!(
                    "Dropbox gave no answer to the copy ({cause}), and the look at {to} failed too ({e}). Look at {to} before you try again"
                ))));
            }
        };
        match look {
            Look::File { hash, size, rev } if hash == source => Ok(Copied {
                source: source.to_string(),
                target: hash,
                size,
                rev: (!rev.is_empty()).then_some(rev),
                folders,
            }),
            Look::Missing => Err(CopyError::Io(failure(format!(
                "Dropbox gave no answer to the copy ({cause}), and {to} is not there, so nothing was copied. Try again"
            )))),
            _ => Err(CopyError::Io(failure(format!(
                "Dropbox gave no answer to the copy ({cause}), and {to} holds something other than {from}. It may be older than this copy, so nothing is removed. Look at {to}"
            )))),
        }
    }

    /// The move call got no answer, so Dropbox may have moved the file or not.
    /// One look at each name tells: the file at the new name and none at the old
    /// one is a move; the reverse is no move. Anything else is for a person.
    fn after_no_move_answer(
        &self,
        from: &str,
        to: &str,
        source: &str,
        folders: Vec<String>,
        cause: io::Error,
    ) -> Result<Moved, CopyError> {
        match (self.look(to), self.look(from)) {
            (Ok(Look::File { hash, size, .. }), Ok(Look::Missing)) => Ok(Moved {
                source: source.to_string(),
                target: hash,
                size,
                folders,
            }),
            (Ok(Look::Missing), Ok(Look::File { .. })) => Err(CopyError::Io(failure(format!(
                "Dropbox gave no answer to the move ({cause}), and {from} is still there, so nothing moved. Try again"
            )))),
            _ => Err(CopyError::Unexpected {
                reason: format!(
                    "Dropbox gave no answer to the move ({cause}), and a look could not tell where the file is. Look at {from} and {to}"
                ),
            }),
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
    /// One recursive `list_folder` and its pages. No byte of a file comes.
    fn names(
        &self,
        folder: &str,
        keep: Keep,
        found: &mut dyn FnMut(&str, bool),
    ) -> io::Result<Walked> {
        let full = if folder.is_empty() {
            String::new()
        } else {
            Self::file(folder)
        };
        let depth = if folder.is_empty() {
            0
        } else {
            folder.split('/').count()
        };
        let arg = json!({ "path": full, "recursive": true, "include_deleted": false });
        let mut reply = self.rpc("list_folder", &arg)?;
        if reply.status == 409 {
            let text = summary(&reply.body);
            if text.contains("not_found") {
                return Ok(Walked::Missing);
            }
            if text.contains("not_folder") {
                return Ok(Walked::NotFolder);
            }
        }
        loop {
            if reply.status != 200 {
                return Err(refused(&reply));
            }
            let (entries, cursor, more) = deep_page_of(&reply.body, depth)?;
            for (inside, is_folder) in entries {
                if kept(&inside, is_folder, keep) {
                    found(&inside, is_folder);
                }
            }
            if !more {
                return Ok(Walked::Done);
            }
            reply = self.rpc("list_folder/continue", &json!({ "cursor": cursor }))?;
        }
    }

    /// One `download_zip` of the folder, held in memory only, and refused above
    /// `limit` bytes. Each file of the zip is read, and gone again after `found`.
    fn texts(
        &self,
        folder: &str,
        limit: u64,
        keep: Keep,
        found: &mut dyn FnMut(&str, &[u8]),
    ) -> io::Result<Walked> {
        let url = format!("{}/2/files/download_zip", self.content);
        let headers = [format!(
            "Dropbox-API-Arg: {}",
            ascii_json(&json!({ "path": Self::file(folder) }))
        )];
        let reply = self.call_full(&url, &headers, None, false, Some(limit))?;
        match reply.status {
            200 => {}
            409 => {
                let text = summary(&reply.body);
                if text.contains("not_found") {
                    return Ok(Walked::Missing);
                }
                if text.contains("not_folder") {
                    return Ok(Walked::NotFolder);
                }
                if text.contains("too_large") || text.contains("too_many_files") {
                    return Err(io::Error::new(
                        io::ErrorKind::FileTooLarge,
                        format!("Dropbox cannot zip it: {text}"),
                    ));
                }
                return Err(refused(&reply));
            }
            _ => return Err(refused(&reply)),
        }
        let top = folder.rsplit('/').next().unwrap_or(folder);
        zip_texts(&reply.body, top, limit, keep, found)?;
        Ok(Walked::Done)
    }

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

    fn read_rev(&self, path: &str) -> io::Result<Option<(Vec<u8>, Option<String>)>> {
        self.download_rev(&Self::file(path))
    }

    /// One trip with a noted rev: Dropbox checks the version as it deletes.
    /// With no rev, `get_metadata` first gives it, and its hash is checked.
    fn remove_if(
        &self,
        path: &str,
        expect: &Expect,
        rev: Option<&str>,
    ) -> Result<Removed, RemoveError> {
        let Expect::Fingerprint(want) = expect else {
            return Err(RemoveError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a remove needs the fingerprint of the file",
            )));
        };
        let full = Self::file(path);
        let rev = match rev {
            Some(rev) => rev.to_string(),
            None => {
                let Some(meta) = self.meta(&full)? else {
                    return Err(RemoveError::Changed { found: None });
                };
                if meta.hash != *want {
                    return Err(RemoveError::Changed {
                        found: Some(meta.hash),
                    });
                }
                meta.rev
            }
        };
        // Without a rev, Dropbox would delete whatever is there, also a folder.
        if rev.is_empty() {
            return Err(RemoveError::Io(failure(format!(
                "Dropbox gave no rev for {full}"
            ))));
        }
        let arg = json!({ "path": full, "parent_rev": rev });
        let reply = self.rpc("delete_v2", &arg)?;
        match removed_of(reply.status, &reply.body, want) {
            Ok(removed) => Ok(removed),
            Err(Deleted::Refused) => {
                // Only on a refusal: what is there now, for the answer and the log.
                let found = self.meta(&full).ok().flatten().map(|meta| meta.hash);
                Err(RemoveError::Changed { found })
            }
            Err(Deleted::Unexpected(reason)) => Err(RemoveError::Unexpected { reason }),
            Err(Deleted::Error(_)) => Err(RemoveError::Io(refused(&reply))),
        }
    }

    /// `get_metadata` of the source, a look at the folders of the new name, then
    /// `copy_v2`, which copies inside Dropbox, so no byte passes this server.
    /// `autorename: false`: a target that exists is a refusal, never a renamed
    /// copy. The hash in the answer is the check, as Dropbox makes it from the
    /// stored file. Dropbox makes the missing folders itself; the look before
    /// only names them for the answer.
    fn copy(&self, from: &str, to: &str, expect: Option<&str>) -> Result<Copied, CopyError> {
        let (source, target) = (Self::file(from), Self::file(to));
        let hash = match self.look(&source)? {
            Look::File { hash, .. } if !hash.is_empty() => hash,
            Look::File { .. } => {
                return Err(CopyError::Io(failure(format!(
                    "Dropbox gave no hash for {source}"
                ))));
            }
            Look::Folder => return Err(CopyError::Folder),
            Look::Missing => return Err(CopyError::Missing),
        };
        if let Some(want) = expect
            && want != hash
        {
            return Err(CopyError::Changed { found: hash });
        }
        let folders = new_folders(to, |folder| self.exists(folder))?;
        let result = (|| -> Result<Copied, CopyError> {
            let arg = json!({ "from_path": source, "to_path": target, "autorename": false });
            let reply = match self.rpc("copy_v2", &arg) {
                Ok(reply) => reply,
                Err(cause) => {
                    return self.after_no_answer(&source, &target, &hash, folders.clone(), cause);
                }
            };
            match reply.status {
                200 => match made_of(&reply.body) {
                    Ok((Some(made), size, rev)) => Ok(Copied {
                        source: hash,
                        target: made,
                        size,
                        rev,
                        folders: folders.clone(),
                    }),
                    // No hash in the answer: one look gives it.
                    Ok((None, size, rev)) => match self.look(&target)? {
                        Look::File { hash: made, .. } => Ok(Copied {
                            source: hash,
                            target: made,
                            size,
                            rev,
                            folders: folders.clone(),
                        }),
                        _ => Err(CopyError::Unexpected {
                            reason: format!(
                                "Dropbox reported a copy at {target}, but a look finds no file there"
                            ),
                        }),
                    },
                    Err(kind) => Err(CopyError::Unexpected {
                        reason: format!(
                            "Dropbox copied {source}, but reports {kind} at {target}, not a file"
                        ),
                    }),
                },
                409 => Err(copy_refusal(&summary(&reply.body))
                    .unwrap_or_else(|| CopyError::Io(refused(&reply)))),
                _ => Err(CopyError::Io(refused(&reply))),
            }
        })();
        // Dropbox makes nothing on a refusal, so only a failure may leave folders.
        result.map_err(|e| match e {
            CopyError::Io(_) | CopyError::Unexpected { .. } => e.after(&folders),
            e => e,
        })
    }

    /// `get_metadata` of the source, a look at the folders of the new name, then
    /// `move_v2`, which moves inside Dropbox. `autorename: false`: a new name
    /// that exists is a refusal, never a renamed file. Dropbox makes the missing
    /// folders itself; the look before only names them for the answer.
    fn move_file(&self, from: &str, to: &str, expect: Option<&str>) -> Result<Moved, CopyError> {
        let (source, target) = (Self::file(from), Self::file(to));
        let hash = match self.look(&source)? {
            Look::File { hash, .. } if !hash.is_empty() => hash,
            Look::File { .. } => {
                return Err(CopyError::Io(failure(format!(
                    "Dropbox gave no hash for {source}"
                ))));
            }
            Look::Folder => return Err(CopyError::Folder),
            Look::Missing => return Err(CopyError::Missing),
        };
        if let Some(want) = expect
            && want != hash
        {
            return Err(CopyError::Changed { found: hash });
        }
        let folders = new_folders(to, |folder| self.exists(folder))?;
        let result = (|| -> Result<Moved, CopyError> {
            let arg = json!({
                "from_path": source,
                "to_path": target,
                "autorename": false,
                "allow_ownership_transfer": false,
            });
            let reply = match self.rpc("move_v2", &arg) {
                Ok(reply) => reply,
                Err(cause) => {
                    return self.after_no_move_answer(
                        &source,
                        &target,
                        &hash,
                        folders.clone(),
                        cause,
                    );
                }
            };
            match reply.status {
                200 => match made_of(&reply.body) {
                    Ok((Some(made), size, _)) => Ok(Moved {
                        source: hash,
                        target: made,
                        size,
                        folders: folders.clone(),
                    }),
                    // No hash in the answer: one look gives it.
                    Ok((None, size, _)) => match self.look(&target)? {
                        Look::File { hash: made, .. } => Ok(Moved {
                            source: hash,
                            target: made,
                            size,
                            folders: folders.clone(),
                        }),
                        _ => Err(CopyError::Unexpected {
                            reason: format!(
                                "Dropbox reported a move to {target}, but a look finds no file there. Look at {source} and {target}"
                            ),
                        }),
                    },
                    Err(kind) => Err(CopyError::Unexpected {
                        reason: format!(
                            "Dropbox moved {source}, but reports {kind} at {target}, not a file"
                        ),
                    }),
                },
                409 => Err(copy_refusal(&summary(&reply.body))
                    .unwrap_or_else(|| CopyError::Io(refused(&reply)))),
                _ => Err(CopyError::Io(refused(&reply))),
            }
        })();
        // Dropbox makes nothing on a refusal, so only a failure may leave folders.
        result.map_err(|e| match e {
            CopyError::Io(_) | CopyError::Unexpected { .. } => e.after(&folders),
            e => e,
        })
    }

    fn exists(&self, path: &str) -> io::Result<bool> {
        Ok(self.look(&Self::file(path))? != Look::Missing)
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
    fn curl_output_splits_into_body_header_and_status() {
        let out = b"line one\nline two\n\n{\"rev\":\"a1\"}\n200".to_vec();
        let reply = split_reply(out, true).unwrap();
        assert_eq!(reply.status, 200);
        assert_eq!(reply.body, b"line one\nline two\n");
        assert_eq!(reply.result.as_deref(), Some("{\"rev\":\"a1\"}"));
        let reply = split_reply(b"body\n\n409".to_vec(), true).unwrap();
        assert_eq!((reply.status, reply.result), (409, None));
        let reply = split_reply(b"a\nb\n200".to_vec(), false).unwrap();
        assert_eq!(reply.body, b"a\nb");
    }

    #[test]
    fn a_download_rev_counts_only_with_the_hash_of_its_bytes() {
        let header = r#"{"rev":"015f","content_hash":"aa"}"#;
        assert_eq!(rev_of_download(Some(header), "aa").as_deref(), Some("015f"));
        assert_eq!(rev_of_download(Some(header), "bb"), None);
        assert_eq!(rev_of_download(Some("not json"), "aa"), None);
        assert_eq!(rev_of_download(None, "aa"), None);
    }

    #[test]
    fn a_delete_reply_of_200_gives_the_size_and_rev() {
        let body = br#"{"metadata":{".tag":"file","name":"a.md","size":12,"rev":"015f","content_hash":"aa"}}"#;
        assert_eq!(
            removed_of(200, body, "aa"),
            Ok(Removed {
                size: 12,
                rev: Some("015f".into())
            })
        );
    }

    #[test]
    fn a_delete_reply_of_200_with_another_hash_or_a_folder_is_damage() {
        let body = br#"{"metadata":{".tag":"file","size":12,"rev":"015f","content_hash":"bb"}}"#;
        let Err(Deleted::Unexpected(reason)) = removed_of(200, body, "aa") else {
            panic!("another hash must not pass");
        };
        assert!(reason.contains("rev 015f"), "{reason}");
        let body = br#"{"metadata":{".tag":"folder","name":"notes"}}"#;
        assert!(matches!(
            removed_of(200, body, "aa"),
            Err(Deleted::Unexpected(_))
        ));
    }

    #[test]
    fn a_delete_reply_of_409_conflict_or_not_found_deletes_nothing() {
        let conflict = br#"{"error_summary":"path_write/conflict/file/.."}"#;
        assert_eq!(removed_of(409, conflict, "aa"), Err(Deleted::Refused));
        let gone = br#"{"error_summary":"path_lookup/not_found/.."}"#;
        assert_eq!(removed_of(409, gone, "aa"), Err(Deleted::Refused));
        let other = br#"{"error_summary":"too_many_write_operations/.."}"#;
        assert!(matches!(
            removed_of(409, other, "aa"),
            Err(Deleted::Error(_))
        ));
        assert!(matches!(removed_of(500, b"", "aa"), Err(Deleted::Error(_))));
    }

    #[test]
    fn the_refusals_of_copy_follow_the_dropbox_error_tags() {
        let is = |text: &str, want: &str| {
            let got = format!("{:?}", copy_refusal(text));
            assert!(got.contains(want), "{text}: {got}");
        };
        is("to/conflict/file/..", "Exists");
        is("to/conflict/folder/...", "Exists");
        is("to/conflict/file_ancestor/..", "Exists");
        is("from_lookup/not_found/..", "Missing");
        is("from_lookup/not_file/..", "Folder");
        // A missing source is not a target conflict, and the other way round.
        is("to/no_write_permission/..", "None");
        is("from_write/conflict/..", "None");
        is("too_many_write_operations/..", "None");
        is("insufficient_quota/..", "None");
    }

    #[test]
    fn a_copy_answer_gives_the_hash_size_and_rev_of_a_file() {
        let body = br#"{"metadata":{".tag":"file","name":"b.md","size":12,"rev":"015f","content_hash":"aa"}}"#;
        assert_eq!(
            made_of(body),
            Ok((Some("aa".into()), 12, Some("015f".into())))
        );
        let no_hash = br#"{"metadata":{".tag":"file","name":"b.md","size":12,"rev":"015f"}}"#;
        assert_eq!(made_of(no_hash), Ok((None, 12, Some("015f".into()))));
        let folder = br#"{"metadata":{".tag":"folder","name":"b"}}"#;
        assert_eq!(made_of(folder), Err("folder".into()));
        assert_eq!(made_of(b"not json"), Err("nothing".into()));
    }

    /// A zip as `download_zip` gives it, with the files packed by deflate.
    fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        use zip::write::SimpleFileOptions;
        let mut out = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in files {
            if name.ends_with('/') {
                out.add_directory(*name, options).unwrap();
            } else {
                out.start_file(*name, options).unwrap();
                out.write_all(bytes).unwrap();
            }
        }
        out.finish().unwrap().into_inner()
    }

    fn texts_of(zip: &[u8], top: &str, limit: u64, keep: Keep) -> Vec<(String, Vec<u8>)> {
        let mut seen = Vec::new();
        zip_texts(zip, top, limit, keep, &mut |p, b| {
            seen.push((p.to_string(), b.to_vec()))
        })
        .unwrap();
        seen
    }

    #[test]
    fn a_zip_gives_each_file_from_the_zipped_folder() {
        let zip = zip_of(&[
            ("Health/", b""),
            ("Health/log.md", b"blood pressure\n"),
            ("Health/old/", b""),
            ("Health/old/a.md", b"old"),
            ("Health/.hidden/b.md", b"b"),
        ]);
        let all = |_: &str, _: bool| true;
        let seen = texts_of(&zip, "health", 100, &all);
        assert_eq!(
            seen,
            vec![
                ("log.md".to_string(), b"blood pressure\n".to_vec()),
                ("old/a.md".to_string(), b"old".to_vec()),
                (".hidden/b.md".to_string(), b"b".to_vec()),
            ]
        );
        // A folder that keep refuses leaves out what it holds.
        let no_dots = |p: &str, _: bool| !p.rsplit('/').next().unwrap_or(p).starts_with('.');
        let seen = texts_of(&zip, "health", 100, &no_dots);
        assert_eq!(seen.len(), 2);
        // A file above the limit stays out.
        let seen = texts_of(&zip, "health", 5, &all);
        assert_eq!(seen.len(), 2, "log.md has 15 bytes");
    }

    #[test]
    fn a_zip_with_no_top_folder_keeps_its_names() {
        let zip = zip_of(&[("log.md", b"x"), ("notes/a.md", b"y")]);
        let all = |_: &str, _: bool| true;
        let names: Vec<String> = texts_of(&zip, "notes", 100, &all)
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        assert_eq!(names, ["log.md", "notes/a.md"]);
    }

    #[test]
    fn a_broken_zip_is_a_failure() {
        let all = |_: &str, _: bool| true;
        let e = zip_texts(b"not a zip", "x", 100, &all, &mut |_, _| {}).unwrap_err();
        assert!(e.to_string().contains("broken"), "{e}");
    }

    #[test]
    fn a_recursive_entry_is_named_from_the_listed_folder() {
        let entry = |v: Value| deep_entry_of(&v, 2);
        assert_eq!(
            entry(json!({".tag": "file", "path_display": "/Org/Notes/Drafts/C.md"})),
            Some(("Drafts/C.md".to_string(), false))
        );
        assert_eq!(
            entry(json!({".tag": "folder", "path_display": "/Org/Notes/Drafts"})),
            Some(("Drafts".to_string(), true))
        );
        assert_eq!(
            entry(json!({".tag": "folder", "path_display": "/Org/Notes"})),
            None
        );
        assert_eq!(
            entry(json!({".tag": "deleted", "path_display": "/Org/Notes/x"})),
            None
        );
        assert_eq!(
            deep_entry_of(&json!({".tag": "file", "path_display": "/a.md"}), 0),
            Some(("a.md".to_string(), false))
        );
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
