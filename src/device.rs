//! The device door: one door per app, and one token per app and device.
//!
//! A device, such as an e-reader, gets no password and no OAuth. The owner runs
//! `orgthread device pair <name>` in a terminal, and types the code of 8 signs
//! on the device. The device trades it at `POST /pair` for a token, within 5
//! minutes, once. The token lasts 90 days. This file keeps only hashes.
//!
//! The rules of the door are strict, unlike the limits of the full door: each
//! path starts with `/`, has no `//`, no `.` and no `..` part, and capitals
//! count. A folder rule opens what lies inside it for `read` and `list`. A file
//! rule opens the files it matches for `read` and `patch`, never more. A list
//! rule opens what lies inside it for `list` only, and closes it to `read` and
//! `patch`, also where a read or a patch rule covers it: the stricter wins.
//!
//! No function here reads a setting: the door hands each one in.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// A pairing code works for this long, in seconds.
pub const CODE_LIFE: u64 = 5 * 60;
/// A token works for this long, in seconds: 90 days.
pub const TOKEN_LIFE: u64 = 90 * 24 * 3600;
/// The windows of the read cap, in seconds.
pub const HOUR: u64 = 3600;
pub const DAY: u64 = 24 * 3600;
/// The signs of a pairing code: capitals and digits, with no I, L, O, 0 or 1,
/// which are easy to mix up on a small screen.
pub const CODE_SIGNS: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
pub const CODE_LENGTH: usize = 8;
/// A device and an app have a short name, as it goes into the log.
const MAX_NAME: usize = 32;

/// Seconds since 1970.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The SHA-256 of a token or a code, as the file keeps it.
pub fn hash(secret: &str) -> String {
    hex(&Sha256::digest(secret.as_bytes()))
}

fn random_bytes(bytes: &mut [u8]) -> io::Result<()> {
    fs::File::open("/dev/urandom")?.read_exact(bytes)
}

/// A new token: 32 random bytes, as 64 hex signs.
fn new_token() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    random_bytes(&mut bytes)?;
    Ok(hex(&bytes))
}

/// A new pairing code of 8 signs. Each sign is drawn with no bias: a byte at or
/// above the last whole multiple of the number of signs is drawn again.
fn new_code() -> io::Result<String> {
    let signs = CODE_SIGNS.len();
    let fair = 256 - 256 % signs;
    let mut code = String::new();
    while code.len() < CODE_LENGTH {
        let mut byte = [0u8; 1];
        random_bytes(&mut byte)?;
        if usize::from(byte[0]) < fair {
            code.push(CODE_SIGNS[usize::from(byte[0]) % signs] as char);
        }
    }
    Ok(code)
}

/// A code as the device may type it: capitals or small letters, with spaces or
/// a dash between the signs. `None` when it cannot be a code.
pub fn normal_code(typed: &str) -> Option<String> {
    let code: String = typed
        .chars()
        .filter(|c| *c != ' ' && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect();
    let fits = code.len() == CODE_LENGTH && code.bytes().all(|b| CODE_SIGNS.contains(&b));
    fits.then_some(code)
}

/// A code shown to the owner: two groups of four, `ABCD-EFGH`.
pub fn shown_code(code: &str) -> String {
    let (a, b) = code.split_at(code.len() / 2);
    format!("{a}-{b}")
}

/// `Ok` for the name of a device or an app: small letters, digits and `-`,
/// up to 32 signs, starting with a letter or a digit.
pub fn check_name(name: &str) -> Result<(), String> {
    let fits = !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-');
    if fits {
        Ok(())
    } else {
        Err(format!(
            "{name:?} is not a name for a device or an app: use small letters, digits and -, up to {MAX_NAME} signs, such as tablet"
        ))
    }
}

/// The name of a device in the log and in the notes of what it read.
pub fn who(device: &str, app: &str) -> String {
    format!("{device} ({app})")
}

/// A date and time in UTC, such as `2026-11-17 14:05 UTC`.
pub fn time_text(seconds: u64) -> String {
    jiff::Timestamp::from_second(seconds as i64)
        .map(|t| t.strftime("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_default()
}

/// A date in UTC, such as `2026-11-17`.
pub fn date_text(seconds: u64) -> String {
    jiff::Timestamp::from_second(seconds as i64)
        .map(|t| t.strftime("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

// The rules of the paths.

/// The items of a comma-separated list, trimmed, with no empty ones.
pub fn split(list: &str) -> Vec<String> {
    list.split(',')
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

/// `Ok` when `path` is in the one form the door takes: it starts with `/`, it
/// has no `//`, no `.` and no `..` part, no `\` and no control sign. A path that
/// ends with `/` names a folder.
pub fn clean(path: &str) -> Result<(), String> {
    let shown = shown(path);
    if !path.starts_with('/') {
        return Err(format!(
            "{shown} is not a path of this door: it must start with /"
        ));
    }
    let inside = path.strip_prefix('/').unwrap_or(path);
    let inside = inside.strip_suffix('/').unwrap_or(inside);
    let bad = inside.is_empty()
        || path.contains('\\')
        || path.chars().any(char::is_control)
        || inside
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..");
    if bad {
        return Err(format!(
            "{shown} is not a clean path: no //, no . and no .. part"
        ));
    }
    Ok(())
}

/// A path as a message or the log may show it: control signs become `?`, and
/// a very long path is cut. The log has one line per entry, with tabs between
/// the fields, so a path from outside must never bring a tab or a newline.
pub fn shown(path: &str) -> String {
    let mut out: String = path
        .chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .take(300)
        .collect();
    if path.chars().count() > 300 {
        out.push('…');
    }
    out
}

/// True when `name` fits `pattern`, where `*` stands for any part of the name,
/// also an empty one. The signs must be the same, capitals too.
fn glob(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    let (mut i, mut j) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while j < n.len() {
        if i < p.len() && p[i] == '*' {
            star = Some((i, j));
            i += 1;
        } else if i < p.len() && p[i] == n[j] {
            i += 1;
            j += 1;
        } else if let Some((si, sj)) = star {
            i = si + 1;
            j = sj + 1;
            star = Some((si, sj + 1));
        } else {
            return false;
        }
    }
    p[i..].iter().all(|c| *c == '*')
}

/// True when the file `path` fits the file rule `rule`: the same number of
/// parts, each part fits. A `*` never covers a `/`.
pub fn fits(rule: &str, path: &str) -> bool {
    let rule: Vec<&str> = rule.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    rule.len() == path.len() && rule.iter().zip(&path).all(|(r, p)| glob(r, p))
}

/// The check of `ORGTHREAD_DEVICE_READ` and `ORGTHREAD_DEVICE_LIST`: folders,
/// each with `/` at both ends, where `*` may stand for any part of one folder
/// name.
pub fn check_folders(list: &str) -> Result<(), String> {
    for rule in split(list) {
        clean(&rule)?;
        if !rule.ends_with('/') {
            return Err(format!(
                "{} must be a folder with / at both ends, such as /books/ or /books/*/",
                shown(&rule)
            ));
        }
    }
    Ok(())
}

/// The parts of a folder rule or a path, with no `/` at either end.
fn parts(path: &str) -> Vec<&str> {
    path.trim_matches('/').split('/').collect()
}

/// True when the parts of `path` start with the parts of one of the folder
/// `rules`, each part fitting. With `deeper`, the path must also go on below
/// the rule, as a file in it does. A `*` covers one part, never a `/`.
fn under_a_rule(rules: &[String], path: &[&str], deeper: bool) -> bool {
    rules.iter().any(|rule| {
        let rule = parts(rule);
        let long_enough = if deeper {
            path.len() > rule.len()
        } else {
            path.len() >= rule.len()
        };
        long_enough && rule.iter().zip(path).all(|(r, p)| glob(r, p))
    })
}

/// The check of `ORGTHREAD_DEVICE_PATCH` and `ORGTHREAD_DEVICE_CAPPED`: files,
/// where `*` may stand for any part of a name.
pub fn check_files(list: &str) -> Result<(), String> {
    for rule in split(list) {
        clean(&rule)?;
        if rule.ends_with('/') {
            return Err(format!(
                "{} must be a file, not a folder: no / at its end",
                shown(&rule)
            ));
        }
    }
    Ok(())
}

/// What a device may touch: the folders it may read and list, the files it may
/// read and patch, and the folders it may only list. Anything else is closed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rules {
    /// Folders, each with `/` at both ends, where `*` stands for any part of
    /// one folder name: `/books/*/` opens each folder in `/books/`, and what
    /// it holds, but no file directly in `/books/`.
    pub read: Vec<String>,
    /// Files, where `*` stands for any part of a name.
    pub patch: Vec<String>,
    /// Folders in the form of `read`, open to `list` only. A file in them is
    /// closed to `read` and `patch`, even where `read` or `patch` names it.
    pub list: Vec<String>,
    /// Folders in the form of `read`, open to `download` of exact bytes.
    pub download: Vec<String>,
    /// Folders in the form of `read`, open to `upload` of exact bytes.
    pub upload: Vec<String>,
    /// Folders in the form of `read`, in which a file may go to the trash.
    pub trash: Vec<String>,
}

/// The name of the trash folder inside a folder of the trash rules. No new
/// tool touches a path with a part of this name: the trash is the owner's.
pub const TRASH: &str = ".trash";

/// How much of a folder a device may list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Listing {
    /// Every entry: the folder lies inside a read folder.
    All,
    /// The names of its folders only: the folder holds the folders of a rule
    /// with `*`, such as `/books/` for `/books/*/`, and its files stay closed.
    FoldersOnly,
}

impl Rules {
    pub fn new(read: &str, patch: &str, list: &str) -> Rules {
        Rules {
            read: split(read),
            patch: split(patch),
            list: split(list),
            ..Rules::default()
        }
    }

    /// The same rules, with the folders of `download`, `upload` and `trash`.
    pub fn with_transfer(mut self, download: &str, upload: &str, trash: &str) -> Rules {
        self.download = split(download);
        self.upload = split(upload);
        self.trash = split(trash);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.read.is_empty()
            && self.patch.is_empty()
            && self.list.is_empty()
            && self.download.is_empty()
            && self.upload.is_empty()
            && self.trash.is_empty()
    }

    /// `Ok` when `path` is a clean file with no `.trash` part.
    fn transfer_file(path: &str) -> Result<(), String> {
        clean(path)?;
        if path.ends_with('/') {
            return Err(format!("{} names a folder, not a file", shown(path)));
        }
        if parts(path).contains(&TRASH) {
            return Err(format!(
                "{} lies in a {TRASH} folder, which this door leaves to its owner",
                shown(path)
            ));
        }
        Ok(())
    }

    /// `Ok` when a device may download the exact bytes of the file `path`.
    pub fn download(&self, path: &str) -> Result<(), String> {
        Rules::transfer_file(path)?;
        if under_a_rule(&self.download, &parts(path), true) {
            return Ok(());
        }
        Err(format!(
            "{} is outside the folders this door may download from",
            shown(path)
        ))
    }

    /// `Ok` when a device may upload the exact bytes of the file `path`.
    pub fn upload(&self, path: &str) -> Result<(), String> {
        Rules::transfer_file(path)?;
        if under_a_rule(&self.upload, &parts(path), true) {
            return Ok(());
        }
        Err(format!(
            "{} is outside the folders this door may upload to",
            shown(path)
        ))
    }

    /// The place in the trash for the file `path` on the day `date`, such as
    /// `/notes/.trash/2026-11-25/a/b.note` for `/notes/a/b.note` and the rule
    /// `/notes/`: the folder of the rule, `.trash`, the day, and the rest of
    /// the path.
    pub fn trash(&self, path: &str, date: &str) -> Result<String, String> {
        Rules::transfer_file(path)?;
        let asked = parts(path);
        let rule = self.trash.iter().map(|r| parts(r)).find(|rule| {
            asked.len() > rule.len() && rule.iter().zip(&asked).all(|(r, p)| glob(r, p))
        });
        let Some(rule) = rule else {
            return Err(format!(
                "{} is outside the folders this door may move to the trash",
                shown(path)
            ));
        };
        let (folder, rest) = asked.split_at(rule.len());
        Ok(format!(
            "/{}/{TRASH}/{date}/{}",
            folder.join("/"),
            rest.join("/")
        ))
    }

    fn in_read_folder(&self, path: &str) -> bool {
        under_a_rule(&self.read, &parts(path), true)
    }

    /// True when `path` lies in a folder of the list rules: then it is closed
    /// to `read` and `patch`, whatever the other rules say.
    fn in_list_folder(&self, path: &str) -> bool {
        under_a_rule(&self.list, &parts(path), true)
    }

    fn in_patch_files(&self, path: &str) -> bool {
        self.patch.iter().any(|rule| fits(rule, path))
    }

    /// The refusal of a read or a patch in a folder of the list rules.
    fn list_only(path: &str) -> String {
        format!(
            "{} lies in a folder that this door may only list, not read or patch",
            shown(path)
        )
    }

    /// `Ok` when a device may read the file `path`.
    pub fn read(&self, path: &str) -> Result<(), String> {
        clean(path)?;
        if path.ends_with('/') {
            return Err(format!("{} names a folder, not a file", shown(path)));
        }
        if self.in_list_folder(path) {
            return Err(Rules::list_only(path));
        }
        if self.in_read_folder(path) || self.in_patch_files(path) {
            return Ok(());
        }
        Err(format!(
            "{} is outside the folders this door may touch",
            shown(path)
        ))
    }

    /// `Ok` when a device may patch the file `path`.
    pub fn patch(&self, path: &str) -> Result<(), String> {
        clean(path)?;
        if self.in_list_folder(path) {
            return Err(Rules::list_only(path));
        }
        if self.in_patch_files(path) {
            return Ok(());
        }
        if self.in_read_folder(path) {
            return Err(format!("{} is read only for this door", shown(path)));
        }
        Err(format!(
            "{} is outside the folders this door may touch",
            shown(path)
        ))
    }

    /// How much a device may list of `folder`, given with or without its last
    /// `/`: all of a read or list folder or a folder inside one, and only the
    /// folder names of the folder right above the `*` of a rule.
    pub fn list(&self, folder: &str) -> Result<Listing, String> {
        clean(folder)?;
        let asked = parts(folder);
        if under_a_rule(&self.read, &asked, false) || under_a_rule(&self.list, &asked, false) {
            return Ok(Listing::All);
        }
        let above_a_star = self.read.iter().chain(&self.list).any(|rule| {
            let rule = parts(rule);
            let star = rule.iter().position(|part| part.contains('*'));
            star == Some(asked.len()) && rule[..asked.len()] == asked[..]
        });
        if above_a_star {
            return Ok(Listing::FoldersOnly);
        }
        Err(format!(
            "{} is outside the folders this door may list",
            shown(folder)
        ))
    }
}

/// The `n`th name to try in the trash, from 2 on: `a (2).note` for
/// `a.note`, so a second file of the same name and day never goes over the
/// first.
pub fn numbered(path: &str, n: usize) -> String {
    let (folder, name) = path.rsplit_once('/').unwrap_or(("", path));
    let numbered = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} ({n}).{ext}"),
        _ => format!("{name} ({n})"),
    };
    format!("{folder}/{numbered}")
}

/// `Ok` when no part of `path` below `root` is a link. A part that does not
/// exist is fine: the command then says that the file is missing. Only a local
/// folder has links: Dropbox has none.
pub fn no_link(root: &Path, path: &str) -> Result<(), String> {
    let mut place = root.to_path_buf();
    for part in path.trim_matches('/').split('/') {
        place.push(part);
        match fs::symlink_metadata(&place) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(format!(
                    "{} goes through a link, and this door follows none",
                    shown(path)
                ));
            }
            Ok(_) => {}
            Err(_) => return Ok(()),
        }
    }
    Ok(())
}

// The file of the tokens and the codes.

/// A pairing code that waits for its device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Code {
    pub hash: String,
    pub device: String,
    pub until: u64,
}

/// A token of one app on one device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub hash: String,
    pub device: String,
    pub app: String,
    pub made: u64,
    pub until: u64,
    /// The times of the reads that count for the cap, of the last 24 hours.
    pub reads: Vec<u64>,
    /// When the owner was last told that this device reached its cap.
    pub warned: u64,
    /// The time and the bytes of each download and upload of the last 24
    /// hours.
    pub moved: Vec<(u64, u64)>,
    /// When the owner was last told that this device reached its cap in MB.
    pub warned_moved: u64,
}

/// Everything the file holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Devices {
    pub codes: Vec<Code>,
    pub tokens: Vec<Token>,
}

fn number(value: &Value) -> u64 {
    value.as_u64().unwrap_or(0)
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

impl Devices {
    fn from_value(value: &Value) -> Devices {
        let list = |key: &str| value[key].as_array().cloned().unwrap_or_default();
        Devices {
            codes: list("codes")
                .iter()
                .map(|c| Code {
                    hash: text(&c["hash"]),
                    device: text(&c["device"]),
                    until: number(&c["until"]),
                })
                .collect(),
            tokens: list("tokens")
                .iter()
                .map(|t| Token {
                    hash: text(&t["hash"]),
                    device: text(&t["device"]),
                    app: text(&t["app"]),
                    made: number(&t["made"]),
                    until: number(&t["until"]),
                    reads: t["reads"]
                        .as_array()
                        .map(|r| r.iter().map(number).collect())
                        .unwrap_or_default(),
                    warned: number(&t["warned"]),
                    moved: t["moved"]
                        .as_array()
                        .map(|m| {
                            m.iter()
                                .map(|pair| (number(&pair[0]), number(&pair[1])))
                                .collect()
                        })
                        .unwrap_or_default(),
                    warned_moved: number(&t["warned_moved"]),
                })
                .collect(),
        }
    }

    fn to_value(&self) -> Value {
        json!({
            "codes": self.codes.iter().map(|c| json!({
                "hash": c.hash, "device": c.device, "until": c.until,
            })).collect::<Vec<_>>(),
            "tokens": self.tokens.iter().map(|t| json!({
                "hash": t.hash, "device": t.device, "app": t.app, "made": t.made,
                "until": t.until, "reads": t.reads, "warned": t.warned,
                "moved": t.moved.iter().map(|(at, bytes)| json!([at, bytes])).collect::<Vec<_>>(),
                "warned_moved": t.warned_moved,
            })).collect::<Vec<_>>(),
        })
    }

    /// Codes and tokens that ran out go, and reads older than a day.
    fn prune(&mut self, now: u64) {
        self.codes.retain(|c| c.until > now);
        self.tokens.retain(|t| t.until > now);
        for token in &mut self.tokens {
            token.reads.retain(|r| r + DAY > now);
            token.moved.retain(|(at, _)| at + DAY > now);
        }
    }
}

/// What a pairing gives the device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paired {
    pub token: String,
    pub device: String,
    pub until: u64,
}

/// The holder of a token that works.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holder {
    pub hash: String,
    pub device: String,
    pub app: String,
}

/// The read cap of one device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cap {
    pub hour: u64,
    pub day: u64,
}

/// Why a read is refused by the cap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capped {
    /// The most reads, and the window, in words: `8 in 60 minutes`.
    pub reached: String,
    /// When the next read is allowed again.
    pub until: u64,
    /// True the first time in an hour: then the owner gets a push.
    pub warn: bool,
}

/// The file of the codes and the tokens. It holds hashes only, never a raw
/// token or code, and only its owner may read it. Each change takes a lock
/// beside it, so the commands and the door never lose each other's changes.
pub struct Store {
    file: PathBuf,
}

impl Store {
    pub fn new(file: impl Into<PathBuf>) -> Store {
        Store { file: file.into() }
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    /// What the file holds. A missing file holds nothing. A broken file is an
    /// error, so that a fault never opens or closes the door by chance.
    pub fn load(&self) -> io::Result<Devices> {
        match fs::read_to_string(&self.file) {
            Ok(text) => {
                let value: Value = serde_json::from_str(&text).map_err(|e| {
                    io::Error::other(format!("{} is broken: {e}", self.file.display()))
                })?;
                Ok(Devices::from_value(&value))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Devices::default()),
            Err(e) => Err(e),
        }
    }

    /// The new file in one step, a temporary file and then a rename, for its
    /// owner alone.
    fn save(&self, devices: &Devices) -> io::Result<()> {
        if let Some(parent) = self.file.parent() {
            crate::log::private_dir(parent)?;
        }
        let mut temp = self.file.as_os_str().to_owned();
        temp.push(".new");
        let temp = PathBuf::from(temp);
        let _ = fs::remove_file(&temp);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let text = devices.to_value().to_string();
        let result = options
            .open(&temp)
            .and_then(|mut file| {
                file.write_all(text.as_bytes())?;
                file.sync_all()
            })
            .and_then(|()| fs::rename(&temp, &self.file));
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }

    /// Runs `change` on what the file holds, under the lock, and saves.
    fn change<T>(&self, now: u64, change: impl FnOnce(&mut Devices) -> T) -> io::Result<T> {
        if let Some(parent) = self.file.parent() {
            crate::log::private_dir(parent)?;
        }
        let mut name = self.file.as_os_str().to_owned();
        name.push(".lock");
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(PathBuf::from(name))?;
        lock.lock()?;
        let mut devices = self.load()?;
        devices.prune(now);
        let out = change(&mut devices);
        self.save(&devices)?;
        Ok(out)
    }

    /// A new pairing code for `device`, valid for 5 minutes and once. An older
    /// code of the same device ends.
    pub fn issue(&self, device: &str, now: u64) -> io::Result<String> {
        check_name(device).map_err(io::Error::other)?;
        let code = new_code()?;
        let hash = hash(&code);
        self.change(now, |d| {
            d.codes.retain(|c| c.device != device);
            d.codes.push(Code {
                hash,
                device: device.to_string(),
                until: now + CODE_LIFE,
            });
        })?;
        Ok(code)
    }

    /// Trades a code for a token of `app`. The code ends with its first use, and
    /// an older token of the same device ends too: one token per app and device.
    /// `None` for a code that is wrong, used or too old.
    pub fn pair(&self, app: &str, typed: &str, now: u64) -> io::Result<Option<Paired>> {
        let Some(code) = normal_code(typed) else {
            return Ok(None);
        };
        let wanted = hash(&code);
        let token = new_token()?;
        let token_hash = hash(&token);
        self.change(now, |d| {
            let at = d.codes.iter().position(|c| c.hash == wanted)?;
            let device = d.codes.remove(at).device;
            d.tokens.retain(|t| t.device != device);
            let until = now + TOKEN_LIFE;
            d.tokens.push(Token {
                hash: token_hash,
                device: device.clone(),
                app: app.to_string(),
                made: now,
                until,
                reads: Vec::new(),
                warned: 0,
                moved: Vec::new(),
                warned_moved: 0,
            });
            Some(Paired {
                token,
                device,
                until,
            })
        })
    }

    /// The holder of `token`, when it is a token of `app` that has not run out.
    /// It reads the file at each call, so a revoke holds at once.
    pub fn holder(&self, app: &str, token: &str, now: u64) -> io::Result<Option<Holder>> {
        if token.is_empty() {
            return Ok(None);
        }
        let wanted = hash(token);
        let devices = self.load()?;
        Ok(devices
            .tokens
            .iter()
            .find(|t| t.hash == wanted && t.app == app && t.until > now)
            .map(|t| Holder {
                hash: t.hash.clone(),
                device: t.device.clone(),
                app: t.app.clone(),
            }))
    }

    /// Counts one read of `holder` against the cap, or refuses it. A refused
    /// read does not count. `Ok(None)` means the read may go on.
    pub fn count(&self, holder: &Holder, cap: Cap, now: u64) -> io::Result<Option<Capped>> {
        self.change(now, |d| {
            let Some(token) = d.tokens.iter_mut().find(|t| t.hash == holder.hash) else {
                // Revoked since the check: refuse, with no push.
                return Some(Capped {
                    reached: "a revoked token".into(),
                    until: now,
                    warn: false,
                });
            };
            let in_hour: Vec<u64> = token
                .reads
                .iter()
                .copied()
                .filter(|r| r + HOUR > now)
                .collect();
            let reached = if in_hour.len() as u64 >= cap.hour {
                let oldest = in_hour.iter().min().copied().unwrap_or(now);
                Some((format!("{} in 60 minutes", cap.hour), oldest + HOUR))
            } else if token.reads.len() as u64 >= cap.day {
                let oldest = token.reads.iter().min().copied().unwrap_or(now);
                Some((format!("{} in 24 hours", cap.day), oldest + DAY))
            } else {
                None
            };
            match reached {
                Some((reached, until)) => {
                    let warn = token.warned + HOUR <= now;
                    if warn {
                        token.warned = now;
                    }
                    Some(Capped {
                        reached,
                        until,
                        warn,
                    })
                }
                None => {
                    token.reads.push(now);
                    None
                }
            }
        })
    }

    /// Counts a download or an upload of `bytes` of `holder` against the cap
    /// of `cap` bytes in 24 hours, or refuses it. A refused one does not
    /// count. `Ok(None)` means the transfer may go on.
    pub fn count_bytes(
        &self,
        holder: &Holder,
        cap: u64,
        bytes: u64,
        now: u64,
    ) -> io::Result<Option<Capped>> {
        self.change(now, |d| {
            let Some(token) = d.tokens.iter_mut().find(|t| t.hash == holder.hash) else {
                return Some(Capped {
                    reached: "a revoked token".into(),
                    until: now,
                    warn: false,
                });
            };
            let used: u64 = token.moved.iter().map(|(_, b)| b).sum();
            if used.saturating_add(bytes) <= cap {
                token.moved.push((now, bytes));
                return None;
            }
            // The next transfer of this size fits once enough of the oldest
            // ones are older than a day.
            let mut moved = token.moved.clone();
            moved.sort_unstable();
            let mut left = used;
            let mut until = now + DAY;
            for (at, b) in moved {
                if left.saturating_add(bytes) <= cap {
                    break;
                }
                left -= b;
                until = at + DAY;
            }
            let warn = token.warned_moved + HOUR <= now;
            if warn {
                token.warned_moved = now;
            }
            Some(Capped {
                reached: format!("{} MB in 24 hours", cap / 1_000_000),
                until,
                warn,
            })
        })
    }

    /// Ends every token and code of `device`. False when it had none.
    pub fn revoke(&self, device: &str, now: u64) -> io::Result<bool> {
        self.change(now, |d| {
            let before = d.codes.len() + d.tokens.len();
            d.codes.retain(|c| c.device != device);
            d.tokens.retain(|t| t.device != device);
            d.codes.len() + d.tokens.len() < before
        })
    }

    /// What the file holds now, with what ran out left away.
    pub fn list(&self, now: u64) -> io::Result<Devices> {
        let mut devices = self.load()?;
        devices.prune(now);
        Ok(devices)
    }
}

/// The text of `orgthread device list`. It shows no token, no code and no hash.
pub fn list_text(devices: &Devices, now: u64) -> String {
    if devices.tokens.is_empty() && devices.codes.is_empty() {
        return "no device is paired\n".to_string();
    }
    let mut out = String::new();
    for t in &devices.tokens {
        let hour = t.reads.iter().filter(|r| *r + HOUR > now).count();
        out.push_str(&format!(
            "{}  paired {}  ends {}  capped reads: {} in 60 minutes, {} in 24 hours\n",
            who(&t.device, &t.app),
            date_text(t.made),
            date_text(t.until),
            hour,
            t.reads.len()
        ));
        if !t.moved.is_empty() {
            let bytes: u64 = t.moved.iter().map(|(_, b)| b).sum();
            out.push_str(&format!(
                "{}  downloads and uploads: {} in 24 hours, {} bytes\n",
                who(&t.device, &t.app),
                t.moved.len(),
                bytes
            ));
        }
    }
    for c in &devices.codes {
        out.push_str(&format!(
            "{}  a pairing code waits, until {}\n",
            c.device,
            time_text(c.until)
        ));
    }
    out
}

/// The push when a device reaches its cap. It names the device and the cap,
/// never a path or a text of a file.
pub fn cap_notice(holder: &Holder, capped: &Capped) -> String {
    format!(
        "orgthread: the device {} reached its read cap of {}. The door refuses its capped reads until {}. To end its token: orgthread device revoke {}",
        who(&holder.device, &holder.app),
        capped.reached,
        time_text(capped.until),
        holder.device
    )
}

/// The push when a device reaches its cap in MB. It names the device and the
/// cap, never a path or a text of a file.
pub fn moved_notice(holder: &Holder, capped: &Capped) -> String {
    format!(
        "orgthread: the device {} reached its cap of {} for downloads and uploads. The door refuses its downloads and uploads until {}. To end its token: orgthread device revoke {}",
        who(&holder.device, &holder.app),
        capped.reached,
        time_text(capped.until),
        holder.device
    )
}

/// The push when a device pairs.
pub fn pair_notice(device: &str, app: &str) -> String {
    format!(
        "orgthread: the device {} paired with the door. If this was not you: orgthread device revoke {device}",
        who(device, app)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const READ: &str = "/books/, /notes/glossary/";
    const PATCH: &str = "/notes/progress-*.md";

    fn rules() -> Rules {
        Rules::new(READ, PATCH, "")
    }

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "orgthread-device-unit-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_rules_open_reads_in_the_folders_and_patches_of_the_files() {
        let r = rules();
        assert!(r.read("/books/an-invented-book/chapter-01.txt").is_ok());
        assert!(r.read("/notes/glossary/a.md").is_ok());
        assert!(r.read("/notes/progress-an-invented-book.md").is_ok());
        assert!(r.patch("/notes/progress-an-invented-book.md").is_ok());
        assert!(r.list("/books/").is_ok());
        assert!(r.list("/books").is_ok());
        assert!(r.list("/books/an-invented-book").is_ok());
    }

    #[test]
    fn everything_else_is_closed() {
        let r = rules();
        let outside = "outside the folders this door may touch";
        assert!(r.read("/notes/health.md").unwrap_err().contains(outside));
        assert!(r.read("/notes/progress.md").unwrap_err().contains(outside));
        assert!(
            r.patch("/books/a/chapter-01.txt")
                .unwrap_err()
                .contains("read only for this door")
        );
        assert!(r.patch("/notes/glossary/a.md").is_err());
        assert!(r.patch("/notes/sub/progress-a.md").is_err());
        assert!(r.list("/notes/").is_err());
        assert!(r.list("/").is_err());
        assert!(r.read("/books/").is_err(), "a folder is no file to read");
    }

    #[test]
    fn path_tricks_are_refused() {
        let r = rules();
        for path in [
            "/books/../notes/health.md",
            "/books/./a.txt",
            "/books//a.txt",
            "//books/a.txt",
            "books/a.txt",
            "/Books/a.txt",
            "/BOOKS/a.txt",
            "/booksx/a.txt",
            "/books",
            "/books\\..\\notes/health.md",
            "/books/a\n.txt",
            "/books/a\t.txt",
            "",
            "/",
        ] {
            assert!(r.read(path).is_err(), "{path:?}");
        }
        for path in [
            "/notes/Progress-a.md",
            "/notes/progress-a.MD",
            "/notes//progress-a.md",
            "notes/progress-a.md",
            "/notes/progress-a.md/",
            "/notes/../notes/progress-a.md",
        ] {
            assert!(r.patch(path).is_err(), "{path:?}");
        }
        for folder in ["/books/..", "/books//", "books/", "/Books/", "/booksx"] {
            assert!(r.list(folder).is_err(), "{folder:?}");
        }
    }

    #[test]
    fn a_star_folder_opens_each_folder_in_it_and_no_file_beside_them() {
        let r = Rules::new("/books/*/", "", "");
        assert!(r.read("/books/an-invented-book/chapter-01.txt").is_ok());
        assert!(r.read("/books/an-invented-book/deeper/notes.txt").is_ok());
        for closed in [
            "/books/an-invented-book.epub",
            "/books/a.txt",
            "/books/x/../a.epub",
            "/Books/an-invented-book/chapter-01.txt",
            "/booksx/an-invented-book/chapter-01.txt",
        ] {
            assert!(r.read(closed).is_err(), "{closed}");
        }
        assert_eq!(r.list("/books/"), Ok(Listing::FoldersOnly));
        assert_eq!(r.list("/books"), Ok(Listing::FoldersOnly));
        assert_eq!(r.list("/books/an-invented-book/"), Ok(Listing::All));
        assert_eq!(r.list("/books/an-invented-book/deeper"), Ok(Listing::All));
        for closed in ["/Books/", "/booksx/", "/notes/"] {
            assert!(r.list(closed).is_err(), "{closed}");
        }
        // A star in the middle covers one folder name, never a `/`.
        let middle = Rules::new("/a/*-book/b/", "", "");
        assert!(middle.read("/a/one-book/b/x.txt").is_ok());
        assert!(middle.read("/a/one/two-book/b/x.txt").is_err());
        assert!(middle.read("/a/one-book/x.txt").is_err());
        assert!(middle.read("/a/one-book/c/x.txt").is_err());
        assert_eq!(middle.list("/a/"), Ok(Listing::FoldersOnly));
        assert!(middle.list("/a/one-book/").is_err(), "not above the star");
    }

    #[test]
    fn a_star_stands_for_part_of_one_name_and_never_for_a_slash() {
        assert!(fits("/a/progress-*.md", "/a/progress-x.md"));
        assert!(fits("/a/progress-*.md", "/a/progress-.md"));
        assert!(fits("/a/*/chapter-*.txt", "/a/book/chapter-01.txt"));
        assert!(!fits("/a/progress-*.md", "/a/progress-x/y.md"));
        assert!(!fits("/a/progress-*.md", "/a/b/progress-x.md"));
        assert!(!fits("/a/progress-*.md", "/a/progress-x.mdx"));
        assert!(!fits("/a/*", "/a/b/c"));
    }

    #[test]
    fn a_list_folder_opens_list_and_closes_read_and_patch() {
        let r = Rules::new("", "", "/lessons/");
        assert!(!r.is_empty(), "a list rule alone is a rule");
        assert_eq!(r.list("/lessons/"), Ok(Listing::All));
        assert_eq!(r.list("/lessons"), Ok(Listing::All));
        assert_eq!(r.list("/lessons/Archive/2026/"), Ok(Listing::All));
        let only = "may only list, not read or patch";
        for file in [
            "/lessons/Lesson 1 Mon.pdf",
            "/lessons/Lesson 1 Mon.pdf.mark",
            "/lessons/00 Week.json",
            "/lessons/Archive/2026/11/Lesson 0 Sun.pdf",
        ] {
            assert!(r.read(file).unwrap_err().contains(only), "{file}");
            assert!(r.patch(file).unwrap_err().contains(only), "{file}");
        }
        for closed in ["/", "/Lessons/", "/lessonsx/", "/books/"] {
            assert!(r.list(closed).is_err(), "{closed}");
        }
        for trick in [
            "/lessons/../books/a.txt",
            "/lessons//a.pdf",
            "lessons/a.pdf",
            "/Lessons/a.pdf",
        ] {
            assert!(r.read(trick).is_err(), "{trick}");
        }
        assert!(r.list("/lessons/..").is_err());
        assert!(r.read("/books/a.txt").unwrap_err().contains("outside"));
    }

    #[test]
    fn where_list_and_read_or_patch_meet_the_stricter_wins() {
        // The same folder in both, a list folder inside a read folder, and a
        // patch file inside a list folder: all closed to read and patch.
        let r = Rules::new(
            "/lessons/, /books/",
            "/books/a/progress-*.md",
            "/lessons/, /books/a/",
        );
        let only = "may only list, not read or patch";
        assert!(r.read("/lessons/a.pdf").unwrap_err().contains(only));
        assert!(
            r.read("/books/a/chapter-01.txt")
                .unwrap_err()
                .contains(only)
        );
        assert!(r.read("/books/a/progress-1.md").unwrap_err().contains(only));
        assert!(
            r.patch("/books/a/progress-1.md")
                .unwrap_err()
                .contains(only)
        );
        assert_eq!(r.list("/books/a/"), Ok(Listing::All));
        // Beside the list folder, the read rule holds as before.
        assert!(r.read("/books/b/chapter-01.txt").is_ok());
        assert!(r.read("/books/a.epub").is_ok());
        // A read folder inside a list folder opens nothing either.
        let inside = Rules::new("/lessons/open/", "", "/lessons/");
        assert!(
            inside
                .read("/lessons/open/a.md")
                .unwrap_err()
                .contains(only)
        );
    }

    #[test]
    fn a_star_in_a_list_rule_works_as_in_a_read_rule() {
        let r = Rules::new("", "", "/lessons/*/");
        assert_eq!(r.list("/lessons/"), Ok(Listing::FoldersOnly));
        assert_eq!(r.list("/lessons/week-47/"), Ok(Listing::All));
        assert!(r.read("/lessons/week-47/a.pdf").is_err());
        assert!(r.list("/").is_err());
    }

    #[test]
    fn the_settings_check_wants_clean_folders_and_files() {
        assert!(check_folders("/books/, /notes/glossary/").is_ok());
        assert!(check_folders("").is_ok());
        assert!(check_folders("/books/*/, /notes/glossary/").is_ok());
        for wrong in ["/books", "books/", "/books/../x/", "/books/*", "/a//b/"] {
            assert!(check_folders(wrong).is_err(), "{wrong}");
        }
        assert!(check_files("/notes/progress-*.md, /a/b.md").is_ok());
        for wrong in ["/notes/", "notes/a.md", "/a/../b.md"] {
            assert!(check_files(wrong).is_err(), "{wrong}");
        }
    }

    #[test]
    fn a_link_in_the_path_is_refused() {
        let dir = folder("link");
        fs::create_dir_all(dir.join("books")).unwrap();
        fs::create_dir_all(dir.join("private")).unwrap();
        fs::write(dir.join("private/a.md"), "invented").unwrap();
        std::os::unix::fs::symlink(dir.join("private"), dir.join("books/inside")).unwrap();
        std::os::unix::fs::symlink(dir.join("private/a.md"), dir.join("books/a.md")).unwrap();
        assert!(no_link(&dir, "/books/inside/a.md").is_err());
        assert!(no_link(&dir, "/books/a.md").is_err());
        assert!(no_link(&dir, "/books/missing.md").is_ok());
        assert!(no_link(&dir, "/private/a.md").is_ok());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_code_has_8_signs_and_may_be_typed_loosely() {
        for _ in 0..50 {
            let code = new_code().unwrap();
            assert_eq!(code.len(), CODE_LENGTH);
            assert!(code.bytes().all(|b| CODE_SIGNS.contains(&b)));
        }
        assert_eq!(normal_code("abcd-efgh").as_deref(), Some("ABCDEFGH"));
        assert_eq!(normal_code("ABCD EFGH").as_deref(), Some("ABCDEFGH"));
        assert_eq!(normal_code("ABCDEFG"), None);
        assert_eq!(normal_code("ABCDEFG1"), None, "1 is not a sign of a code");
        assert_eq!(shown_code("ABCDEFGH"), "ABCD-EFGH");
    }

    #[test]
    fn a_code_works_once_within_5_minutes_and_the_file_holds_hashes_only() {
        let dir = folder("pair");
        let store = Store::new(dir.join("devices.json"));
        let t0 = 1_800_000_000;
        let code = store.issue("tablet", t0).unwrap();
        let paired = store.pair("reader", &code, t0 + 10).unwrap().unwrap();
        assert_eq!(paired.device, "tablet");
        assert_eq!(paired.until, t0 + 10 + TOKEN_LIFE);
        assert_eq!(paired.token.len(), 64);
        assert_eq!(store.pair("reader", &code, t0 + 11).unwrap(), None, "used");
        let text = fs::read_to_string(store.file()).unwrap();
        assert!(!text.contains(&paired.token), "no raw token");
        assert!(!text.contains(&code), "no raw code");
        assert!(text.contains(&hash(&paired.token)));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(store.file()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        let late = store.issue("tablet", t0).unwrap();
        assert_eq!(store.pair("reader", &late, t0 + CODE_LIFE).unwrap(), None);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_token_lasts_90_days_for_its_app_alone() {
        let dir = folder("token");
        let store = Store::new(dir.join("devices.json"));
        let t0 = 1_800_000_000;
        let code = store.issue("tablet", t0).unwrap();
        let token = store.pair("reader", &code, t0).unwrap().unwrap().token;
        let held = store.holder("reader", &token, t0 + TOKEN_LIFE - 1).unwrap();
        assert_eq!(held.unwrap().device, "tablet");
        assert_eq!(
            store.holder("reader", &token, t0 + TOKEN_LIFE).unwrap(),
            None
        );
        assert_eq!(store.holder("other-app", &token, t0).unwrap(), None);
        assert_eq!(store.holder("reader", "", t0).unwrap(), None);
        assert_eq!(store.holder("reader", "wrong", t0).unwrap(), None);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_new_pairing_ends_the_old_token_of_the_device() {
        let dir = folder("again");
        let store = Store::new(dir.join("devices.json"));
        let t0 = 1_800_000_000;
        let first = store.issue("tablet", t0).unwrap();
        let old = store.pair("reader", &first, t0).unwrap().unwrap().token;
        let second = store.issue("tablet", t0).unwrap();
        let new = store.pair("reader", &second, t0).unwrap().unwrap().token;
        assert_eq!(store.holder("reader", &old, t0).unwrap(), None);
        assert!(store.holder("reader", &new, t0).unwrap().is_some());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_revoke_ends_the_token_and_the_codes_of_a_device() {
        let dir = folder("revoke");
        let store = Store::new(dir.join("devices.json"));
        let t0 = 1_800_000_000;
        let code = store.issue("tablet", t0).unwrap();
        let token = store.pair("reader", &code, t0).unwrap().unwrap().token;
        let waiting = store.issue("tablet", t0).unwrap();
        assert!(store.revoke("tablet", t0).unwrap());
        assert_eq!(store.holder("reader", &token, t0).unwrap(), None);
        assert_eq!(store.pair("reader", &waiting, t0).unwrap(), None);
        assert!(!store.revoke("tablet", t0).unwrap());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn the_cap_counts_per_hour_and_per_day_and_warns_once_an_hour() {
        let dir = folder("cap");
        let store = Store::new(dir.join("devices.json"));
        let t0 = 1_800_000_000;
        let code = store.issue("tablet", t0).unwrap();
        let token = store.pair("reader", &code, t0).unwrap().unwrap().token;
        let holder = store.holder("reader", &token, t0).unwrap().unwrap();
        let cap = Cap { hour: 2, day: 3 };
        assert_eq!(store.count(&holder, cap, t0).unwrap(), None);
        assert_eq!(store.count(&holder, cap, t0 + 1).unwrap(), None);
        let capped = store.count(&holder, cap, t0 + 2).unwrap().unwrap();
        assert_eq!(capped.reached, "2 in 60 minutes");
        assert_eq!(capped.until, t0 + HOUR);
        assert!(capped.warn);
        let again = store.count(&holder, cap, t0 + 3).unwrap().unwrap();
        assert!(!again.warn, "one push an hour");
        // After the hour, one more fits, and then the day is full.
        assert_eq!(store.count(&holder, cap, t0 + HOUR).unwrap(), None);
        let day = store.count(&holder, cap, t0 + 2 * HOUR).unwrap().unwrap();
        assert_eq!(day.reached, "3 in 24 hours");
        assert_eq!(day.until, t0 + DAY);
        assert!(day.warn, "a new hour, a new push");
        assert_eq!(store.count(&holder, cap, t0 + DAY + 1).unwrap(), None);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn the_list_shows_no_token_no_code_and_no_hash() {
        let dir = folder("list");
        let store = Store::new(dir.join("devices.json"));
        let t0 = 1_800_000_000;
        let code = store.issue("tablet", t0).unwrap();
        let token = store.pair("reader", &code, t0).unwrap().unwrap().token;
        let waiting = store.issue("phone", t0).unwrap();
        let text = list_text(&store.list(t0).unwrap(), t0);
        assert!(text.contains("tablet (reader)  paired 2027-01-15  ends 2027-04-15"));
        assert!(text.contains("phone  a pairing code waits"));
        for secret in [token.clone(), hash(&token), waiting.clone(), hash(&waiting)] {
            assert!(!text.contains(&secret));
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn names_of_devices_are_short_and_plain() {
        assert!(check_name("tablet").is_ok());
        assert!(check_name("tablet-2").is_ok());
        for wrong in ["", "Tablet", "my tablet", "-x", "a\tb", &"x".repeat(33)] {
            assert!(check_name(wrong).is_err(), "{wrong:?}");
        }
    }

    #[test]
    fn a_trash_place_keeps_the_rest_of_the_path_and_a_number_never_goes_over() {
        let r = Rules::default().with_transfer("", "", "/notes/, /a/*/");
        assert_eq!(
            r.trash("/notes/sub/b.note", "2026-11-25").unwrap(),
            "/notes/.trash/2026-11-25/sub/b.note"
        );
        assert_eq!(
            r.trash("/a/x/c.note", "2026-11-25").unwrap(),
            "/a/x/.trash/2026-11-25/c.note"
        );
        assert!(r.trash("/notes/", "2026-11-25").is_err());
        assert!(
            r.trash("/notes/.trash/2026-11-25/b.note", "2026-11-25")
                .is_err()
        );
        assert!(
            r.trash("/a/c.note", "2026-11-25").is_err(),
            "the star needs a folder"
        );
        assert_eq!(numbered("/n/b.note", 2), "/n/b (2).note");
        assert_eq!(numbered("/n/README", 3), "/n/README (3)");
        assert_eq!(numbered("/n/.hidden", 2), "/n/.hidden (2)");
    }

    #[test]
    fn the_cap_in_mb_counts_downloads_and_uploads_and_opens_after_a_day() {
        let dir = std::env::temp_dir().join(format!("orgthread-moved-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let store = Store::new(dir.join("devices.json"));
        let now = 10 * DAY;
        let code = store.issue("tablet", now).unwrap();
        let paired = store.pair("desk", &code, now).unwrap().unwrap();
        let holder = store.holder("desk", &paired.token, now).unwrap().unwrap();
        let cap = 10_000_000;
        assert_eq!(
            store.count_bytes(&holder, cap, 6_000_000, now).unwrap(),
            None
        );
        assert_eq!(
            store
                .count_bytes(&holder, cap, 3_000_000, now + 60)
                .unwrap(),
            None
        );
        let capped = store
            .count_bytes(&holder, cap, 2_000_000, now + 120)
            .unwrap()
            .unwrap();
        assert_eq!(capped.reached, "10 MB in 24 hours");
        assert_eq!(capped.until, now + DAY, "once the first is a day old");
        assert!(capped.warn);
        let again = store
            .count_bytes(&holder, cap, 2_000_000, now + 180)
            .unwrap()
            .unwrap();
        assert!(!again.warn, "one push an hour");
        assert_eq!(
            store
                .count_bytes(&holder, cap, 1_000_000, now + 240)
                .unwrap(),
            None
        );
        assert_eq!(
            store
                .count_bytes(&holder, cap, 2_000_000, now + DAY)
                .unwrap(),
            None
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
