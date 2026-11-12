//! The one list of all settings, and the only place that reads them.
//!
//! Each setting is a line `ORGTHREAD_<NAME>=<value>`, in the environment or in
//! the file `.env` beside the program. A name that the environment holds wins
//! over `.env`. The list below gives each setting its kind of value, its
//! default, the doors that use it, its group on the screen, whether it is a
//! secret, and its help line. `--help`, `docs/settings.md`, the screen of
//! `orgthread config`, `config --check` and the check at start all come from it.
//!
//! No other file of `src/` reads an `ORGTHREAD_` setting: each reader below
//! keeps the exact meaning that the code had before this list, value by value.
//! The tests at the end hold the old code and prove it.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The start of every setting name.
pub const PREFIX: &str = "ORGTHREAD_";

pub const ROOT: &str = "ORGTHREAD_ROOT";
pub const LOCK_DIR: &str = "ORGTHREAD_LOCK_DIR";
pub const LOG: &str = "ORGTHREAD_LOG";
pub const READS: &str = "ORGTHREAD_READS";
pub const VERSIONS: &str = "ORGTHREAD_VERSIONS";
pub const OWNER: &str = "ORGTHREAD_OWNER";
pub const ALLOW: &str = "ORGTHREAD_ALLOW";
pub const DENY: &str = "ORGTHREAD_DENY";
pub const READ_ONLY: &str = "ORGTHREAD_READ_ONLY";
pub const BINARY_ALLOW: &str = "ORGTHREAD_BINARY_ALLOW";
pub const DELETE_ALLOW: &str = "ORGTHREAD_DELETE_ALLOW";
pub const MOVE_ALLOW: &str = "ORGTHREAD_MOVE_ALLOW";
pub const FIXED_FOLDERS: &str = "ORGTHREAD_FIXED_FOLDERS";
pub const DOWNLOAD_MB: &str = "ORGTHREAD_DOWNLOAD_MB";
pub const UPLOAD_MB: &str = "ORGTHREAD_UPLOAD_MB";
pub const SEARCH_MB: &str = "ORGTHREAD_SEARCH_MB";
pub const STORAGE: &str = "ORGTHREAD_STORAGE";
pub const DROPBOX_APP_KEY: &str = "ORGTHREAD_DROPBOX_APP_KEY";
pub const DROPBOX_API: &str = "ORGTHREAD_DROPBOX_API";
pub const DROPBOX_CONTENT: &str = "ORGTHREAD_DROPBOX_CONTENT";
pub const PUBLIC_URL: &str = "ORGTHREAD_PUBLIC_URL";
pub const LOGINS: &str = "ORGTHREAD_LOGINS";
pub const LISTEN_ANYWHERE: &str = "ORGTHREAD_LISTEN_ANYWHERE";
pub const DROPBOX_TOKEN: &str = "ORGTHREAD_DROPBOX_TOKEN";
pub const DROPBOX_REFRESH: &str = "ORGTHREAD_DROPBOX_REFRESH";
pub const DROPBOX_APP_SECRET: &str = "ORGTHREAD_DROPBOX_APP_SECRET";
pub const LOGIN_HASH: &str = "ORGTHREAD_LOGIN_HASH";

/// The kind of value a setting takes. It decides what a right value is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A path of a file or a folder. It may not be empty.
    Path,
    /// Folders or files, separated by commas.
    Paths,
    /// A whole number of MB of 1,000,000 bytes. Empty means the default.
    Megabytes,
    /// A name on one line. Empty means the default.
    Name,
    /// `dropbox` or `local`. Empty means a local folder.
    Storage,
    /// `yes` or `no`. Empty means no.
    YesNo,
    /// An address that starts with `https://`. Empty means not set.
    PublicAddress,
    /// An address that starts with `http://` or `https://`.
    Address,
    /// The 64 signs that `orgthread password-hash` gives. Empty means not set.
    Hash,
    /// Any text on one line. Empty means not set.
    Text,
}

impl Kind {
    /// What a right value looks like, in a few words.
    pub fn rule(self) -> &'static str {
        match self {
            Kind::Path => "a path, not empty",
            Kind::Paths => "paths, separated by commas",
            Kind::Megabytes => "a whole number",
            Kind::Name => "a name on one line",
            Kind::Storage => "dropbox or local",
            Kind::YesNo => "yes or no",
            Kind::PublicAddress => "an address that starts with https://",
            Kind::Address => "an address that starts with http:// or https://",
            Kind::Hash => "the 64 signs of orgthread password-hash",
            Kind::Text => "text on one line",
        }
    }
}

/// The doors that read a setting: the local folder on the Mac, the Dropbox API
/// on a server, or both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Doors {
    Both,
    Local,
    Dropbox,
}

impl Doors {
    pub fn words(self) -> &'static str {
        match self {
            Doors::Both => "both",
            Doors::Local => "local folder",
            Doors::Dropbox => "Dropbox",
        }
    }
}

/// The groups of the screen, in their order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    FoldersAndLogs,
    Limits,
    Sizes,
    Dropbox,
    Server,
    Secrets,
}

impl Group {
    pub const ALL: [Group; 6] = [
        Group::FoldersAndLogs,
        Group::Limits,
        Group::Sizes,
        Group::Dropbox,
        Group::Server,
        Group::Secrets,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Group::FoldersAndLogs => "Folders and logs",
            Group::Limits => "Limits",
            Group::Sizes => "Sizes",
            Group::Dropbox => "Dropbox",
            Group::Server => "Server",
            Group::Secrets => "Secrets",
        }
    }
}

/// One setting of the list.
#[derive(Debug, PartialEq, Eq)]
pub struct Setting {
    pub name: &'static str,
    pub kind: Kind,
    /// The value or the place that holds when the setting is not there, in words.
    pub default: &'static str,
    pub doors: Doors,
    pub group: Group,
    /// A secret is never shown: only "set" or "not set".
    pub secret: bool,
    /// One sentence or two: what the setting does.
    pub help: &'static str,
}

const fn setting(
    name: &'static str,
    kind: Kind,
    default: &'static str,
    doors: Doors,
    group: Group,
    help: &'static str,
) -> Setting {
    Setting {
        name,
        kind,
        default,
        doors,
        group,
        secret: false,
        help,
    }
}

const fn secret(name: &'static str, kind: Kind, doors: Doors, help: &'static str) -> Setting {
    Setting {
        name,
        kind,
        default: "none",
        doors,
        group: Group::Secrets,
        secret: true,
        help,
    }
}

use Doors::{Both, Dropbox as DropboxDoor, Local};
use Group::{Dropbox as DropboxGroup, FoldersAndLogs, Limits, Server, Sizes};

/// Every setting, in the order of the screen.
pub const ALL: [Setting; 27] = [
    setting(
        ROOT,
        Kind::Path,
        "~/Library/CloudStorage/Dropbox",
        Local,
        FoldersAndLogs,
        "The folder that all paths live in.",
    ),
    setting(
        LOCK_DIR,
        Kind::Path,
        ".locks beside the program",
        Both,
        FoldersAndLogs,
        "The locks of orgthread, on this machine only. A path that does not start with / starts at the folder of the program. For a local folder, keep it on the same disk as ORGTHREAD_ROOT.",
    ),
    setting(
        LOG,
        Kind::Path,
        "state/writes.log beside the program",
        Both,
        FoldersAndLogs,
        "The log file: one line per write, refusal and change of a setting, never a text.",
    ),
    setting(
        READS,
        Kind::Path,
        "state/reads beside the program",
        Both,
        FoldersAndLogs,
        "The notes of what each name read.",
    ),
    setting(
        VERSIONS,
        Kind::Path,
        "state/versions beside the program",
        Local,
        FoldersAndLogs,
        "A copy of each version written, for 30 days, for the diff screen and the revert of watch.",
    ),
    setting(
        OWNER,
        Kind::Name,
        "owner",
        Both,
        FoldersAndLogs,
        "Your name, for the log lines of a revert in watch and of a change in config: <name> (watch), <name> (config).",
    ),
    setting(
        ALLOW,
        Kind::Paths,
        "all",
        Both,
        Limits,
        "Comma-separated folders or files that a door may touch.",
    ),
    setting(
        DENY,
        Kind::Paths,
        "none",
        Both,
        Limits,
        "Comma-separated folders or files that a door may not touch.",
    ),
    setting(
        READ_ONLY,
        Kind::Paths,
        "none",
        Both,
        Limits,
        "Comma-separated folders or files that a door may only read.",
    ),
    setting(
        BINARY_ALLOW,
        Kind::Paths,
        "none",
        Both,
        Limits,
        "Comma-separated folders where a file that is not text may go, by upload, copy or move.",
    ),
    setting(
        DELETE_ALLOW,
        Kind::Paths,
        "none, so every delete is refused",
        Both,
        Limits,
        "Comma-separated folders where delete may remove a file.",
    ),
    setting(
        MOVE_ALLOW,
        Kind::Paths,
        "not set, so move has only the other limits",
        Both,
        Limits,
        "Comma-separated folders. When set, move needs both names in them.",
    ),
    setting(
        FIXED_FOLDERS,
        Kind::Paths,
        "not set, so any new folder is made",
        Both,
        Limits,
        "Comma-separated folders: no new folder may appear directly inside them, for write, upload, copy and move. Deeper new folders are fine. / stands for the root.",
    ),
    setting(
        DOWNLOAD_MB,
        Kind::Megabytes,
        "250",
        Both,
        Sizes,
        "The largest download, in MB of 1,000,000 bytes.",
    ),
    setting(
        UPLOAD_MB,
        Kind::Megabytes,
        "20",
        Both,
        Sizes,
        "The largest upload, in MB of 1,000,000 bytes.",
    ),
    setting(
        SEARCH_MB,
        Kind::Megabytes,
        "25",
        Both,
        Sizes,
        "With Dropbox, the largest zip of a search, in MB. With a local folder, the largest file that a search reads.",
    ),
    setting(
        STORAGE,
        Kind::Storage,
        "local",
        Both,
        DropboxGroup,
        "dropbox: use the Dropbox API, with paths from its root, such as /org/a.md. It keeps no copies on the disk. local: a folder on this machine.",
    ),
    setting(
        DROPBOX_APP_KEY,
        Kind::Text,
        "none",
        DropboxDoor,
        DropboxGroup,
        "The app key. With ORGTHREAD_DROPBOX_REFRESH and ORGTHREAD_DROPBOX_APP_SECRET, it gets a fresh token for each command.",
    ),
    setting(
        DROPBOX_API,
        Kind::Address,
        "https://api.dropboxapi.com",
        DropboxDoor,
        DropboxGroup,
        "The address of the Dropbox API. Change it only for a test.",
    ),
    setting(
        DROPBOX_CONTENT,
        Kind::Address,
        "https://content.dropboxapi.com",
        DropboxDoor,
        DropboxGroup,
        "The address of the Dropbox content API. Change it only for a test.",
    ),
    setting(
        PUBLIC_URL,
        Kind::PublicAddress,
        "none, required by serve --http",
        Both,
        Server,
        "The public address of serve --http, with its secret path.",
    ),
    setting(
        LOGINS,
        Kind::Path,
        "state/logins.json beside the program",
        Both,
        Server,
        "The logins of a door over a restart: hashes of its tokens, no raw token and no text of a note.",
    ),
    setting(
        LISTEN_ANYWHERE,
        Kind::YesNo,
        "no: only 127.0.0.1, [::1] and localhost",
        Both,
        Server,
        "yes: let serve --http listen on an address that others can reach, with no encryption.",
    ),
    secret(
        DROPBOX_TOKEN,
        Kind::Text,
        DropboxDoor,
        "A Dropbox token. With none, ORGTHREAD_DROPBOX_REFRESH, ORGTHREAD_DROPBOX_APP_KEY and ORGTHREAD_DROPBOX_APP_SECRET get a fresh one for each command.",
    ),
    secret(
        DROPBOX_REFRESH,
        Kind::Text,
        DropboxDoor,
        "The refresh token of the Dropbox app.",
    ),
    secret(
        DROPBOX_APP_SECRET,
        Kind::Text,
        DropboxDoor,
        "The app secret of the Dropbox app.",
    ),
    secret(
        LOGIN_HASH,
        Kind::Hash,
        Both,
        "The hash of the password of serve --http, from orgthread password-hash.",
    ),
];

/// The setting of this name, if the list has one.
pub fn find(name: &str) -> Option<&'static Setting> {
    ALL.iter().find(|s| s.name == name)
}

// The readers. Each one takes the value in effect from the environment, which
// the loader filled from `.env` at start. The `_from` forms take any lookup, so
// the tests can compare them with the old code without the real environment.

/// A lookup of one name: the value, or `None` when the name is not there.
pub type Get<'a> = &'a dyn Fn(&str) -> Option<OsString>;

fn env_get(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

/// The value as text, or `None` when it is not there or not UTF-8, as the old
/// `env::var(name).ok()`.
fn text_from(get: Get, name: &str) -> Option<String> {
    get(name).and_then(|v| v.into_string().ok())
}

fn dropbox_from(get: Get) -> bool {
    text_from(get, STORAGE).is_some_and(|v| v == "dropbox")
}

/// True when the door uses the Dropbox API, with `ORGTHREAD_STORAGE=dropbox`.
pub fn dropbox() -> bool {
    dropbox_from(&env_get)
}

fn path_from(get: Get, name: &str) -> Option<PathBuf> {
    get(name).map(PathBuf::from)
}

/// The value of a path setting, if set.
fn path(name: &str) -> Option<PathBuf> {
    path_from(&env_get, name)
}

/// The home folder, for the default root.
pub fn home() -> Result<PathBuf, String> {
    // HOME is not a setting of orgthread, but the default of one.
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set".to_string())
}

/// The root of a local folder, before it is checked: the setting, else the
/// Dropbox folder in the home folder.
pub fn root() -> Result<PathBuf, String> {
    match path(ROOT) {
        Some(root) => Ok(root),
        None => Ok(home()?.join("Library/CloudStorage/Dropbox")),
    }
}

fn lock_dir_from(get: Get, folder: &Path) -> PathBuf {
    match get(LOCK_DIR) {
        Some(dir) => folder.join(dir),
        None => folder.join(".locks"),
    }
}

/// The folder of the locks. A path that does not start with `/` starts at the
/// folder of the program.
pub fn lock_dir(folder: &Path) -> PathBuf {
    lock_dir_from(&env_get, folder)
}

fn state_from(get: Get, name: &str, folder: &Path, default: &str) -> PathBuf {
    match path_from(get, name) {
        Some(path) => path,
        None => folder.join("state").join(default),
    }
}

/// The log file. A path that does not start with `/` starts at the folder the
/// command runs in, as before.
pub fn log(folder: &Path) -> PathBuf {
    state_from(&env_get, LOG, folder, "writes.log")
}

/// The notes of what each name read.
pub fn reads(folder: &Path) -> PathBuf {
    state_from(&env_get, READS, folder, "reads")
}

/// The copies of each version written, on a local folder.
pub fn versions(folder: &Path) -> PathBuf {
    state_from(&env_get, VERSIONS, folder, "versions")
}

/// The file of the logins, if the setting names one. With none, the caller
/// makes `state` beside the program and uses `logins.json` in it.
pub fn logins() -> Option<PathBuf> {
    path(LOGINS)
}

fn list_from(get: Get, name: &str) -> String {
    text_from(get, name).unwrap_or_default()
}

/// The text of a list setting, empty when it is not there.
fn list(name: &str) -> String {
    list_from(&env_get, name)
}

pub fn allow() -> String {
    list(ALLOW)
}

pub fn deny() -> String {
    list(DENY)
}

pub fn read_only() -> String {
    list(READ_ONLY)
}

pub fn binary_allow() -> String {
    list(BINARY_ALLOW)
}

pub fn delete_allow() -> String {
    list(DELETE_ALLOW)
}

pub fn move_allow() -> String {
    list(MOVE_ALLOW)
}

pub fn fixed_folders() -> String {
    list(FIXED_FOLDERS)
}

/// The default of a setting of MB, from the list.
fn default_mb(name: &str) -> u64 {
    find(name)
        .and_then(|s| s.default.parse().ok())
        .unwrap_or_default()
}

fn megabytes_from(get: Get, name: &str) -> Result<u64, String> {
    match text_from(get, name) {
        Some(value) if !value.trim().is_empty() => value
            .trim()
            .parse::<u64>()
            .ok()
            .and_then(|mb| mb.checked_mul(1_000_000))
            .ok_or_else(|| format!("{name} needs a whole number of MB, not {value}")),
        _ => Ok(default_mb(name) * 1_000_000),
    }
}

/// A limit in bytes, from a setting in MB. A MB here is 1,000,000 bytes.
pub fn megabytes(name: &str) -> Result<u64, String> {
    megabytes_from(&env_get, name)
}

/// The value of `ORGTHREAD_OWNER`, as it stands.
pub fn owner() -> Option<String> {
    text_from(&env_get, OWNER)
}

/// The name for the log: the owner on one line, trimmed, or `owner`.
pub fn name_of(owner: Option<&str>) -> Result<String, String> {
    let owner = owner.map(str::trim).filter(|o| !o.is_empty());
    let owner = owner.unwrap_or(default_of(OWNER));
    if owner.chars().any(char::is_control) {
        return Err(format!("{OWNER} must be a name on one line"));
    }
    Ok(owner.to_string())
}

fn default_of(name: &str) -> &'static str {
    find(name).map_or("", |s| s.default)
}

fn listen_anywhere_from(get: Get) -> bool {
    text_from(get, LISTEN_ANYWHERE).is_some_and(|v| v == "yes")
}

/// True when `serve --http` may listen on any address.
pub fn listen_anywhere() -> bool {
    listen_anywhere_from(&env_get)
}

/// The public address of `serve --http`, empty when it is not there.
pub fn public_url() -> String {
    list(PUBLIC_URL)
}

/// The hash of the password of `serve --http`, empty when it is not there.
pub fn login_hash() -> String {
    list(LOGIN_HASH)
}

fn address_from(get: Get, name: &str) -> String {
    text_from(get, name).unwrap_or(default_of(name).to_string())
}

/// The address of the Dropbox API.
pub fn dropbox_api() -> String {
    address_from(&env_get, DROPBOX_API)
}

/// The address of the Dropbox content API.
pub fn dropbox_content() -> String {
    address_from(&env_get, DROPBOX_CONTENT)
}

fn dropbox_token_from(get: Get) -> Option<String> {
    text_from(get, DROPBOX_TOKEN).filter(|t| !t.is_empty())
}

/// The Dropbox token, when it is there and not empty.
pub fn dropbox_token() -> Option<String> {
    dropbox_token_from(&env_get)
}

/// The three values that get a fresh token, each empty when not there.
pub const REFRESH: [&str; 3] = [DROPBOX_REFRESH, DROPBOX_APP_KEY, DROPBOX_APP_SECRET];

pub fn dropbox_refresh() -> [String; 3] {
    REFRESH.map(list)
}

fn secret_values_from(get: Get) -> Vec<String> {
    std::iter::once(DROPBOX_TOKEN)
        .chain(REFRESH)
        .filter_map(|name| text_from(get, name))
        .collect()
}

/// The values of the Dropbox secrets that are there, to take out of a message.
pub fn secret_values() -> Vec<String> {
    secret_values_from(&env_get)
}

// The check of each value.

/// `Ok` when `value` is right for `setting`, else the reason, which names the
/// setting. A secret, the hash and the public address never show their value.
pub fn check(setting: &Setting, value: &OsStr) -> Result<(), String> {
    let name = setting.name;
    // A path may hold any bytes, as before. Every other value is text.
    let text = match (setting.kind, value.to_str()) {
        (_, Some(text)) => text.to_string(),
        (Kind::Path, None) => value.to_string_lossy().into_owned(),
        (_, None) => return Err(format!("{name} is not valid UTF-8")),
    };
    if text.chars().any(char::is_control) {
        return Err(format!(
            "{name} must be on one line, with no tab and no control sign"
        ));
    }
    let shown = if setting.secret { "" } else { text.as_str() };
    let empty = text.trim().is_empty();
    let fits = match setting.kind {
        Kind::Path => !empty,
        Kind::Paths | Kind::Name | Kind::Text => true,
        Kind::Megabytes => megabytes_from(&|_: &str| Some(value.to_owned()), name).is_ok(),
        Kind::Storage => matches!(text.as_str(), "" | "dropbox" | "local"),
        Kind::YesNo => matches!(text.as_str(), "" | "yes" | "no"),
        Kind::PublicAddress => text.is_empty() || text.starts_with("https://"),
        Kind::Address => text.starts_with("https://") || text.starts_with("http://"),
        Kind::Hash => {
            text.is_empty() || (text.len() == 64 && text.chars().all(|c| c.is_ascii_hexdigit()))
        }
    };
    if fits {
        return Ok(());
    }
    Err(match setting.kind {
        Kind::Path => {
            format!("{name} needs a path, and it is empty. Remove the line for the default")
        }
        Kind::Megabytes => format!("{name} needs a whole number of MB, not {shown}"),
        Kind::Hash => format!("{name} needs the 64 signs that orgthread password-hash gives"),
        Kind::PublicAddress => format!("{name} needs an address that starts with https://"),
        _ if empty => format!("{name} needs {}, and it is empty", setting.kind.rule()),
        _ => format!("{name} needs {}, not {shown}", setting.kind.rule()),
    })
}

/// Each setting whose value in effect is wrong, with the reason.
pub fn problems() -> Vec<String> {
    problems_from(&env_get)
}

fn problems_from(get: Get) -> Vec<String> {
    ALL.iter()
        .filter_map(|s| get(s.name).and_then(|value| check(s, &value).err()))
        .collect()
}

// The file `.env`.

/// The folder of this program, wherever it was started from.
pub fn program_folder() -> Result<PathBuf, String> {
    let program =
        std::env::current_exe().map_err(|e| format!("could not find this program: {e}"))?;
    fs::canonicalize(&program)
        .ok()
        .and_then(|real| real.parent().map(Path::to_path_buf))
        .ok_or_else(|| format!("could not find the folder of {}", program.display()))
}

/// The file `.env` beside the program.
pub fn env_file() -> Result<PathBuf, String> {
    Ok(program_folder()?.join(".env"))
}

/// The name and the value of a line of `.env`, when it names a setting. The
/// loader and the screen read a line the same way: the name is all before the
/// first `=`, after the spaces at the ends of the line are gone.
pub fn parse_line(line: &str) -> Option<(&str, &str)> {
    let (name, value) = line.trim().split_once('=')?;
    name.starts_with(PREFIX).then_some((name, value))
}

/// The names that the loader took from `.env`, so the screen can tell them
/// from the names that the environment holds.
static FROM_FILE: OnceLock<Vec<String>> = OnceLock::new();

/// True when the loader took this name from `.env`.
pub fn from_file(name: &str) -> bool {
    FROM_FILE
        .get()
        .is_some_and(|names| names.iter().any(|n| n == name))
}

/// Each `ORGTHREAD_` line of `file`, unless the environment holds that name
/// already. Any other name stays out. A missing file is fine.
///
/// Call it only as the first step of `main`, before any other thread exists.
pub fn load_file(file: &Path) -> io::Result<()> {
    let text = match fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let mut taken = Vec::new();
    for line in text.lines() {
        let Some((name, value)) = parse_line(line) else {
            continue;
        };
        if std::env::var_os(name).is_none() {
            // SAFETY: the first step of main, before any other thread exists.
            unsafe { std::env::set_var(name, value) };
            taken.push(name.to_string());
        }
    }
    let _ = FROM_FILE.set(taken);
    Ok(())
}

/// The names of the environment that the loader did not take from `.env`,
/// with their values: the settings that `.env` cannot change.
pub fn from_environment() -> Vec<(String, OsString)> {
    std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value)))
        .filter(|(name, _)| name.starts_with(PREFIX) && !from_file(name))
        .collect()
}

// The help text and the docs, made from the list.

/// Breaks text at spaces to fit `width`. A longer word stays whole.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let mut row = String::new();
    for word in text.split(' ') {
        if !row.is_empty() && row.chars().count() + 1 + word.chars().count() > width {
            rows.push(std::mem::take(&mut row));
        }
        if !row.is_empty() {
            row.push(' ');
        }
        row.push_str(word);
    }
    rows.push(row);
    rows
}

/// The part of `--help` on the settings.
pub fn help() -> String {
    const NAME: usize = 28;
    const WIDTH: usize = 80;
    let mut out = String::from(
        "Settings:
  Each setting comes from the environment, or from a file .env beside the
  program, with lines ORGTHREAD_NAME=value. The environment wins. A wrong value
  stops every command with code 2, and names the setting.

  orgthread config                               a screen to change .env, in a terminal
  orgthread config --check                       lists each setting and checks it, changes nothing
",
    );
    for group in Group::ALL {
        out.push_str(&format!("\n  {}:\n", group.title()));
        for s in ALL.iter().filter(|s| s.group == group) {
            let text = format!("{} (default: {})", s.help, s.default);
            let rows = wrap(&text, WIDTH - NAME);
            let head = format!("  {}", s.name);
            let mut rows = rows.into_iter();
            if head.chars().count() < NAME {
                let first = rows.next().unwrap_or_default();
                out.push_str(&format!("{head:<NAME$}{first}\n"));
            } else {
                out.push_str(&format!("{head}\n"));
            }
            for row in rows {
                out.push_str(&format!("{:NAME$}{row}\n", ""));
            }
        }
    }
    out
}

/// The text of `docs/settings.md`.
pub fn markdown() -> String {
    let mut out = String::from(
        "# Settings

This file is made from the list in `src/settings.rs`. A test fails when it is out of date. To make it again, run `UPDATE_SETTINGS_DOC=1 cargo test settings_doc`.

Each setting is a line `ORGTHREAD_<NAME>=<value>`. It comes from the environment, or from the file `.env` beside the program. A name that the environment holds wins over `.env`. Write a value with no quotes, as quotes count as part of it. A line that starts with `#` is a comment.

A wrong value stops every command with code 2, wrong use, and the message names the setting. For example, `ORGTHREAD_UPLOAD_MB=abc` gives `ORGTHREAD_UPLOAD_MB needs a whole number of MB, not abc`. Only `orgthread config`, `--help` and `--version` still run.

## Change the settings

`orgthread config` opens a screen in the terminal, with the settings in groups. It works only in a real terminal, as `orgthread watch` does. No command for a script and no MCP tool changes a setting, so no agent can widen its own limits through orgthread.

| Key | What it does |
|---|---|
| `j`, `k` or the arrows | Choose a setting |
| Enter | Change the value. Enter again keeps it, Esc goes back. An empty value is the default |
| `d` | Back to the default: the line goes from `.env` |
| `s` | Save |
| `?` | Help |
| `q` | Quit. With a change not saved, it asks first |

- A secret shows only as `secret: set` or `secret: not set`. Enter replaces it, with hidden typing. The old value is never shown, never logged and never listed.
- A setting that the environment holds shows as `from the environment`, with the value in effect, and cannot be changed on the screen.
- A line that names no setting of the list, such as a typo `ORGTHREAD_OWNR`, shows at the bottom as `not used`. Such lines and the comments stay in place.
- `s` checks every value first: a wrong one saves nothing. If `.env` changed on the disk since the screen read it, it saves nothing either. Then it keeps the old file as `.env.before-config-<date>-<time>`, writes the new `.env` in one step, a temporary file and then a rename, and sets its rights to 600, so only the owner can read it. A new setting goes at the end. The log gets one line `configured` per changed setting, by `<owner> (config)`, with the name only, never the value.
- The screen shows the restart command, and does not run it. On a server: `systemctl --user restart orgthread.service`. On a Mac, each command reads `.env` again, and the local MCP server of the desktop app needs a restart of the app.
- The way back is the copy `.env.before-config-<date>-<time>`, by hand. The copies stay until you remove them.

`orgthread config --check` lists every setting with its value and where it comes from, and secrets as `set` or `not set`. It changes nothing and works without a terminal. It ends with code 2 when a value is wrong, else 0.

## The list

The doors: `local folder` is the door on a Mac, which reads a folder on the disk. `Dropbox` is the door on a server, with `ORGTHREAD_STORAGE=dropbox`. `both` is both.
",
    );
    for group in Group::ALL {
        out.push_str(&format!(
            "\n### {}\n\n| Setting | What it does | A right value | Default | Doors |\n|---|---|---|---|---|\n",
            group.title()
        ));
        for s in ALL.iter().filter(|s| s.group == group) {
            out.push_str(&format!(
                "| `{}` | {} | {} | {} | {} |\n",
                s.name,
                s.help.replace('|', "\\|"),
                s.kind.rule(),
                s.default,
                s.doors.words()
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A lookup over a fixed map, in place of the environment.
    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        move |name: &str| map.get(name).cloned()
    }

    #[test]
    fn the_list_has_each_setting_once_with_a_prefix_and_a_help_line() {
        assert_eq!(ALL.len(), 27);
        for (i, s) in ALL.iter().enumerate() {
            assert!(s.name.starts_with(PREFIX), "{}", s.name);
            assert!(!s.help.is_empty() && !s.default.is_empty(), "{}", s.name);
            assert!(
                ALL[i + 1..].iter().all(|o| o.name != s.name),
                "{} twice",
                s.name
            );
        }
        // The screen shows the groups in their order, each setting once.
        let mut groups: Vec<Group> = ALL.iter().map(|s| s.group).collect();
        groups.dedup();
        assert_eq!(groups, Group::ALL.to_vec());
    }

    #[test]
    fn four_settings_are_secrets_and_all_sit_in_the_group_secrets() {
        let secrets: Vec<&str> = ALL.iter().filter(|s| s.secret).map(|s| s.name).collect();
        assert_eq!(
            secrets,
            [
                DROPBOX_TOKEN,
                DROPBOX_REFRESH,
                DROPBOX_APP_SECRET,
                LOGIN_HASH
            ]
        );
        for s in &ALL {
            assert_eq!(s.secret, s.group == Group::Secrets, "{}", s.name);
        }
    }

    /// Every name in the code of `src/` is in the list, and no file outside
    /// this one reads a setting from the environment.
    #[test]
    fn only_this_file_reads_the_environment() {
        let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for entry in fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap().to_string();
            if name == "settings.rs" || path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let text = fs::read_to_string(&path).unwrap();
            // The tests may use invented names, such as a typo.
            let text = text.split("#[cfg(test)]").next().unwrap_or_default();
            for read in ["env::var(", "env::var_os(", "env::vars(", "env::vars_os("] {
                assert!(!text.contains(read), "{name} has {read}");
            }
            // Each name in the code is in the list, apart from the build value
            // ORGTHREAD_COMMIT and the names of the tests of the live Dropbox.
            for (at, _) in text.match_indices(PREFIX) {
                let word: String = text[at..]
                    .chars()
                    .take_while(|c| c.is_ascii_uppercase() || *c == '_')
                    .collect();
                if word == PREFIX || word == "ORGTHREAD_COMMIT" || word == "ORGTHREAD_NAME" {
                    continue;
                }
                assert!(find(&word).is_some(), "{name}: {word} is not in the list");
            }
        }
    }

    // The old code of 0.2.21, each reader as it was, with `std::env::var` and
    // `var_os` turned into the lookup `get`. Each test compares the new reader
    // with it, value by value: so each door reads each setting as before.

    mod before {
        use super::*;

        pub fn storage(get: Get) -> bool {
            text_from(get, "ORGTHREAD_STORAGE").is_some_and(|v| v == "dropbox")
        }
        pub fn lock_dir(get: Get, folder: &Path) -> PathBuf {
            match get("ORGTHREAD_LOCK_DIR") {
                Some(dir) => folder.join(dir),
                None => folder.join(".locks"),
            }
        }
        pub fn state(get: Get, name: &str, folder: &Path, default: &str) -> PathBuf {
            let state = folder.join("state");
            match get(name) {
                Some(path) => PathBuf::from(path),
                None => state.join(default),
            }
        }
        pub fn var(get: Get, name: &str) -> String {
            text_from(get, name).unwrap_or_default()
        }
        pub fn limit(get: Get, name: &str, default_mb: u64) -> Result<u64, String> {
            match text_from(get, name) {
                Some(value) if !value.trim().is_empty() => value
                    .trim()
                    .parse::<u64>()
                    .ok()
                    .and_then(|mb| mb.checked_mul(1_000_000))
                    .ok_or_else(|| format!("{name} needs a whole number of MB, not {value}")),
                _ => Ok(default_mb * 1_000_000),
            }
        }
        pub fn who_of(owner: Option<&str>) -> Result<String, String> {
            let owner = owner.map(str::trim).filter(|o| !o.is_empty());
            let owner = owner.unwrap_or("owner");
            if owner.chars().any(char::is_control) {
                return Err("ORGTHREAD_OWNER must be a name on one line".to_string());
            }
            Ok(format!("{owner} (watch)"))
        }
        pub fn listen(get: Get) -> bool {
            text_from(get, "ORGTHREAD_LISTEN_ANYWHERE").is_some_and(|v| v == "yes")
        }
        pub fn url(get: Get, name: &str, default: &str) -> String {
            text_from(get, name).unwrap_or(default.to_string())
        }
        pub fn token(get: Get) -> Option<String> {
            text_from(get, "ORGTHREAD_DROPBOX_TOKEN").filter(|t| !t.is_empty())
        }
        pub fn secrets(get: Get) -> Vec<String> {
            [
                "ORGTHREAD_DROPBOX_TOKEN",
                "ORGTHREAD_DROPBOX_REFRESH",
                "ORGTHREAD_DROPBOX_APP_KEY",
                "ORGTHREAD_DROPBOX_APP_SECRET",
            ]
            .into_iter()
            .filter_map(|name| text_from(get, name))
            .collect()
        }
    }

    /// The values each setting is tried with: not there, empty, spaces, a
    /// usual value, and odd ones.
    const VALUES: [Option<&str>; 9] = [
        None,
        Some(""),
        Some("  "),
        Some("dropbox"),
        Some("yes"),
        Some("20"),
        Some(" 7 "),
        Some("notes/drafts,private"),
        Some("/srv/an/invented/path"),
    ];

    fn with(name: &str, value: Option<&str>) -> impl Fn(&str) -> Option<OsString> + use<> {
        let pairs: Vec<(&str, &str)> = value.map(|v| (name, v)).into_iter().collect();
        lookup(&pairs)
    }

    #[test]
    fn storage_reads_as_before() {
        for value in VALUES.into_iter().chain([Some("Dropbox"), Some("local")]) {
            let get = with(STORAGE, value);
            assert_eq!(dropbox_from(&get), before::storage(&get), "{value:?}");
        }
    }

    #[test]
    fn the_folders_of_the_program_read_as_before() {
        let folder = Path::new("/invented/program");
        for value in VALUES {
            let get = with(LOCK_DIR, value);
            assert_eq!(
                lock_dir_from(&get, folder),
                before::lock_dir(&get, folder),
                "{value:?}"
            );
            for (name, default) in [
                (LOG, "writes.log"),
                (READS, "reads"),
                (VERSIONS, "versions"),
            ] {
                let get = with(name, value);
                assert_eq!(
                    state_from(&get, name, folder, default),
                    before::state(&get, name, folder, default),
                    "{name} {value:?}"
                );
            }
            let get = with(ROOT, value);
            assert_eq!(path_from(&get, ROOT), get(ROOT).map(PathBuf::from));
            let get = with(LOGINS, value);
            assert_eq!(path_from(&get, LOGINS), get(LOGINS).map(PathBuf::from));
        }
    }

    #[test]
    fn the_limits_read_as_before() {
        let lists = [
            ALLOW,
            DENY,
            READ_ONLY,
            BINARY_ALLOW,
            DELETE_ALLOW,
            MOVE_ALLOW,
            FIXED_FOLDERS,
            PUBLIC_URL,
            LOGIN_HASH,
        ];
        for name in lists {
            for value in VALUES {
                let get = with(name, value);
                assert_eq!(
                    list_from(&get, name),
                    before::var(&get, name),
                    "{name} {value:?}"
                );
            }
        }
    }

    #[test]
    fn the_sizes_read_as_before() {
        for (name, default) in [(DOWNLOAD_MB, 250), (UPLOAD_MB, 20), (SEARCH_MB, 25)] {
            let odd = [Some("abc"), Some("-1"), Some("1.5"), Some("99999999999999")];
            for value in VALUES.into_iter().chain(odd) {
                let get = with(name, value);
                assert_eq!(
                    megabytes_from(&get, name),
                    before::limit(&get, name, default),
                    "{name} {value:?}"
                );
            }
        }
    }

    #[test]
    fn the_owner_reads_as_before() {
        for value in VALUES.into_iter().chain([Some("Ana"), Some(" Ana\n")]) {
            let new = name_of(value).map(|n| format!("{n} (watch)"));
            assert_eq!(new, before::who_of(value), "{value:?}");
        }
    }

    #[test]
    fn the_server_and_dropbox_settings_read_as_before() {
        for value in VALUES.into_iter().chain([Some("no"), Some("YES")]) {
            let get = with(LISTEN_ANYWHERE, value);
            assert_eq!(listen_anywhere_from(&get), before::listen(&get));
            for (name, default) in [
                (DROPBOX_API, "https://api.dropboxapi.com"),
                (DROPBOX_CONTENT, "https://content.dropboxapi.com"),
            ] {
                let get = with(name, value);
                assert_eq!(
                    address_from(&get, name),
                    before::url(&get, name, default),
                    "{name} {value:?}"
                );
            }
            let get = with(DROPBOX_TOKEN, value);
            assert_eq!(dropbox_token_from(&get), before::token(&get));
            for name in REFRESH {
                let get = with(name, value);
                assert_eq!(list_from(&get, name), before::var(&get, name));
                assert_eq!(secret_values_from(&get), before::secrets(&get));
            }
        }
    }

    #[test]
    fn a_value_that_is_not_utf8_reads_as_before() {
        use std::os::unix::ffi::OsStrExt;
        let odd = OsStr::from_bytes(b"a\xffb").to_owned();
        for s in &ALL {
            let value = odd.clone();
            let name = s.name;
            let get = move |n: &str| (n == name).then(|| value.clone());
            assert_eq!(list_from(&get, name), before::var(&get, name));
        }
        let get = |_: &str| Some(odd.clone());
        assert_eq!(dropbox_from(&get), before::storage(&get));
        assert_eq!(
            megabytes_from(&get, UPLOAD_MB),
            before::limit(&get, UPLOAD_MB, 20)
        );
        assert_eq!(
            state_from(&get, LOG, Path::new("/p"), "writes.log"),
            before::state(&get, LOG, Path::new("/p"), "writes.log")
        );
    }

    #[test]
    fn the_defaults_of_the_sizes_come_from_the_list() {
        let none = |_: &str| None;
        assert_eq!(megabytes_from(&none, DOWNLOAD_MB), Ok(250_000_000));
        assert_eq!(megabytes_from(&none, UPLOAD_MB), Ok(20_000_000));
        assert_eq!(megabytes_from(&none, SEARCH_MB), Ok(25_000_000));
    }

    fn checked(name: &str, value: &str) -> Result<(), String> {
        check(find(name).unwrap(), OsStr::new(value))
    }

    #[test]
    fn a_right_value_passes_the_check() {
        let right = [
            (ROOT, "/Users/you/notes"),
            (LOCK_DIR, ".locks"),
            (UPLOAD_MB, "20"),
            (UPLOAD_MB, ""),
            (UPLOAD_MB, " 7 "),
            (OWNER, "Ana"),
            (OWNER, ""),
            (ALLOW, "notes/drafts, private"),
            (STORAGE, "dropbox"),
            (STORAGE, "local"),
            (STORAGE, ""),
            (LISTEN_ANYWHERE, "yes"),
            (LISTEN_ANYWHERE, "no"),
            (
                PUBLIC_URL,
                "https://notes.example.com/a-long-invented-secret",
            ),
            (PUBLIC_URL, ""),
            (DROPBOX_API, "http://127.0.0.1:9"),
            (LOGIN_HASH, &"a1".repeat(32)),
            (LOGIN_HASH, ""),
            (DROPBOX_TOKEN, "an-invented-token"),
        ];
        for (name, value) in right {
            assert_eq!(checked(name, value), Ok(()), "{name}={value}");
        }
    }

    #[test]
    fn a_wrong_value_fails_the_check_and_names_the_setting() {
        let wrong = [
            (
                UPLOAD_MB,
                "abc",
                "ORGTHREAD_UPLOAD_MB needs a whole number of MB, not abc",
            ),
            (
                SEARCH_MB,
                "-1",
                "ORGTHREAD_SEARCH_MB needs a whole number of MB, not -1",
            ),
            (ROOT, "", "ORGTHREAD_ROOT needs a path, and it is empty"),
            (
                STORAGE,
                "Dropbox",
                "ORGTHREAD_STORAGE needs dropbox or local, not Dropbox",
            ),
            (
                LISTEN_ANYWHERE,
                "true",
                "ORGTHREAD_LISTEN_ANYWHERE needs yes or no, not true",
            ),
            (
                PUBLIC_URL,
                "http://example.com",
                "ORGTHREAD_PUBLIC_URL needs an address",
            ),
            (DROPBOX_API, "", "ORGTHREAD_DROPBOX_API needs an address"),
            (
                LOGIN_HASH,
                "short",
                "ORGTHREAD_LOGIN_HASH needs the 64 signs",
            ),
            (OWNER, "Ana\tB", "ORGTHREAD_OWNER must be on one line"),
            (DENY, "a\nb", "ORGTHREAD_DENY must be on one line"),
        ];
        for (name, value, start) in wrong {
            let reason = checked(name, value).unwrap_err();
            assert!(reason.starts_with(start), "{name}={value}: {reason}");
        }
    }

    #[test]
    fn a_wrong_secret_never_shows_its_value() {
        let reason = checked(LOGIN_HASH, "invented-secret-hash").unwrap_err();
        assert!(!reason.contains("invented"), "{reason}");
        let reason = checked(DROPBOX_TOKEN, "invented\nsecret").unwrap_err();
        assert!(!reason.contains("invented"), "{reason}");
        let reason = checked(PUBLIC_URL, "http://example.com/invented-path").unwrap_err();
        assert!(!reason.contains("invented"), "{reason}");
    }

    #[test]
    fn the_problems_are_the_wrong_values_in_effect() {
        let get = lookup(&[(UPLOAD_MB, "abc"), (OWNER, "Ana"), ("ORGTHREAD_OWNR", "x")]);
        assert_eq!(
            problems_from(&get),
            ["ORGTHREAD_UPLOAD_MB needs a whole number of MB, not abc"]
        );
        assert!(problems_from(&|_| None).is_empty());
    }

    #[test]
    fn a_line_of_env_reads_as_the_loader_reads_it() {
        assert_eq!(
            parse_line("ORGTHREAD_OWNER=Ana"),
            Some(("ORGTHREAD_OWNER", "Ana"))
        );
        assert_eq!(
            parse_line("  ORGTHREAD_OWNER=Ana  "),
            Some(("ORGTHREAD_OWNER", "Ana"))
        );
        assert_eq!(parse_line("ORGTHREAD_A=b=c"), Some(("ORGTHREAD_A", "b=c")));
        assert_eq!(parse_line("ORGTHREAD_A="), Some(("ORGTHREAD_A", "")));
        assert_eq!(parse_line("# ORGTHREAD_OWNER=Ana"), None);
        assert_eq!(parse_line("export ORGTHREAD_OWNER=Ana"), None);
        assert_eq!(parse_line("PATH=/bin"), None);
        assert_eq!(parse_line("ORGTHREAD_OWNER"), None);
    }

    #[test]
    fn the_help_names_every_setting_with_its_help_and_default() {
        let help = help();
        let flat = help.split_whitespace().collect::<Vec<_>>().join(" ");
        for s in &ALL {
            assert!(help.contains(&format!("  {}", s.name)), "{}", s.name);
            let default = format!("(default: {})", s.default);
            assert!(flat.contains(&default), "{}", s.name);
        }
        for line in help.lines() {
            assert!(line.chars().count() <= 100, "{line}");
        }
        assert!(help.contains("orgthread config --check"));
    }

    /// docs/settings.md is made from the list. To make it again:
    /// `UPDATE_SETTINGS_DOC=1 cargo test settings_doc`.
    #[test]
    fn settings_doc_is_up_to_date() {
        let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/settings.md");
        let made = markdown();
        if std::env::var_os("UPDATE_SETTINGS_DOC").is_some() {
            fs::write(&file, &made).unwrap();
        }
        let found = fs::read_to_string(&file).unwrap_or_default();
        assert!(
            found == made,
            "docs/settings.md is out of date: run UPDATE_SETTINGS_DOC=1 cargo test settings_doc"
        );
        for s in &ALL {
            assert!(made.contains(&format!("`{}`", s.name)), "{}", s.name);
        }
    }
}
