use jiff::tz::TimeZone;
use orgthread::dropbox::Dropbox;
use orgthread::fingerprint::{Fingerprinter, fingerprint};
use orgthread::list::{self, Options, Target};
use orgthread::log;
use orgthread::login::Login;
use orgthread::patch::{self, Patch};
use orgthread::reads::Reads;
use orgthread::screen;
use orgthread::serve;
use orgthread::stop;
use orgthread::storage::{Expect, LocalFolder, Storage};
use orgthread::transfer;
use orgthread::versions::Versions;
use orgthread::write::{self, Outcome, Refusal, exit};
use std::env;
use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
orgthread reads and writes a plain text file the same way every time.

For an agent:

  orgthread read <file> --who <name>             prints the exact text
  orgthread read <file> --who <name> --to <copy> saves an exact copy instead
  orgthread write <file> --who <name>            < the whole new text
  orgthread patch <file> --who <name>            < only the changes, as JSON
  orgthread list <folder>...                     the size, time and name of each entry
  orgthread download <file> --who <name> --to <copy>
                                                 any file, also not text, as exact bytes
  orgthread upload <file> --who <name>           < any file, with the rules of write

Give the same name each time. To change a file, read it first. A new file needs
no read. orgthread remembers what each name read, and it refuses a write when
the file changed since. The agent never handles anything else.

For a chat with no shell:

  orgthread serve                                the same read, write, patch and list, as MCP tools,
                                                 and version: the build, the door, the storage, the tools
  orgthread serve --http <address>               also download and upload, through one-time links

For a person:

  orgthread watch                                the read-only screen; Enter shows a diff, Esc goes back
  orgthread watch --once                         the same screen, as plain text
  orgthread fingerprint <file>                   the content hash Dropbox reports
  orgthread serve [--http <address>]             the MCP tools, for a chat with no shell

Options of write: --size <bytes> refuses text of any other length, so a cut-off
pipe cannot replace a whole file.

A patch is one JSON object: edits, a list of {old, new}, and append. Each edit
replaces its exact old text, whole lines, which must occur once. If it occurs
more than once, add a line that differs, or give line, the number where it
starts. append adds lines at the end. All edits are applied, or none. The name
must have read the file once, and a change elsewhere in the file needs no new
read. patch takes --size too.

Options of list, also called ls: --depth <n> goes n levels down (default 1), and
--depth all to the bottom. --all also shows the names that start with a dot.
After --, each word is a folder. A list shows at most 1000 entries, leaves out
closed paths, and marks read-only ones. The time is in the zone of TZ.

download without --to sends the bytes to its output, never to a terminal. It
notes what this name read, as read does. upload takes --size too. A text file
may go wherever write may write. A file that is not text may go only to the
folders of ORGTHREAD_BINARY_ALLOW. With --check, either one only checks the
rules and the size, and moves nothing.

Settings, from the environment:
  A file .env beside the program, with lines ORGTHREAD_NAME=value, fills in each
  setting of this list that the environment does not hold.

  ORGTHREAD_ROOT      the folder all paths live in (default: ~/Library/CloudStorage/Dropbox)
  ORGTHREAD_LOCK_DIR  orgthread's own locks, on this machine only (default: .locks beside the
                      program). For a local folder, on the same disk as ORGTHREAD_ROOT
  ORGTHREAD_LOG       the log file (default: state/writes.log beside the program)
  ORGTHREAD_READS     the notes of what was read (default: state/reads beside the program)
  ORGTHREAD_STORAGE   dropbox: use the Dropbox API, with paths from its root, such as /org/a.md.
                      Needs ORGTHREAD_DROPBOX_TOKEN. Keeps no copies.
  ORGTHREAD_ALLOW, ORGTHREAD_DENY, ORGTHREAD_READ_ONLY
                      comma-separated folders or files: what a door may touch,
                      what it may not, and what it may only read
  ORGTHREAD_VERSIONS  a copy of each version written, for the diff screen, for 30 days
                      (default: state/versions beside the program)
  ORGTHREAD_LOGINS    the logins of a door over a restart: hashes of its tokens, no raw token
                      and no text of a note (default: state/logins.json beside the program)
  ORGTHREAD_BINARY_ALLOW
                      comma-separated folders where an upload of a file that is not text may go
                      (default: none)
  ORGTHREAD_DOWNLOAD_MB, ORGTHREAD_UPLOAD_MB
                      the largest download and upload, in MB of 1,000,000 bytes (default: 250, 20)
  ORGTHREAD_LISTEN_ANYWHERE
                      yes: let serve --http listen on an address that others can reach, with
                      no encryption (default: only 127.0.0.1, [::1] and localhost)

Exit codes:
  0  done                    6  missing: this name read it, but it is gone
  1  failed, file unchanged  7  check failed, old state put back
  2  wrong use, or not text  8  written, but the log line failed
  3  busy: another writer   9  check failed, old state NOT put back
  4  changed: read it again  10 written, but its clean-up did not finish
  5  exists: read it first, or for --to, the copy exists
  11 an edit of a patch does not fit: nothing written
Each code and what to do: https://github.com/orgthread/orgthread/blob/main/docs/exit-codes.md
";

fn main() -> ExitCode {
    load_settings_file();
    let args: Vec<String> = env::args().skip(1).collect();
    let code = match run(&args) {
        Ok(code) => code,
        Err(message) => {
            say(message);
            say("run orgthread --help for how to use it".to_string());
            exit::USAGE
        }
    };
    ExitCode::from(code as u8)
}

/// The folder of this program, wherever it was started from.
fn program_folder() -> Result<PathBuf, String> {
    let program = env::current_exe().map_err(|e| format!("could not find this program: {e}"))?;
    fs::canonicalize(&program)
        .ok()
        .and_then(|real| real.parent().map(Path::to_path_buf))
        .ok_or_else(|| format!("could not find the folder of {}", program.display()))
}

/// Each `ORGTHREAD_` line of the `.env` beside the program, unless the
/// environment holds that name already. Any other name stays out.
fn load_settings_file() {
    let Ok(folder) = program_folder() else {
        return;
    };
    let file = folder.join(".env");
    let text = match fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return,
        Err(e) => {
            say(format!("could not read {}: {e}", file.display()));
            return;
        }
    };
    for line in text.lines() {
        let Some((name, value)) = line.trim().split_once('=') else {
            continue;
        };
        if name.starts_with("ORGTHREAD_") && env::var_os(name).is_none() {
            // SAFETY: the first step of main, before any other thread exists.
            unsafe { env::set_var(name, value) };
        }
    }
}

fn run(args: &[String]) -> Result<i32, String> {
    match args.first().map(String::as_str) {
        Some("fingerprint") => cmd_fingerprint(&args[1..]),
        Some("read") => cmd_read(&args[1..]),
        Some("write") => cmd_write(&args[1..]),
        Some("patch") => cmd_patch(&args[1..]),
        Some("list" | "ls") => cmd_list(&args[1..]),
        Some("download") => cmd_download(&args[1..]),
        Some("upload") => cmd_upload(&args[1..]),
        Some("watch") => cmd_watch(&args[1..]),
        Some("serve") => cmd_serve(&args[1..]),
        Some("password-hash") => {
            let mut password = String::new();
            io::stdin()
                .read_line(&mut password)
                .map_err(|e| e.to_string())?;
            println!("{}", orgthread::login::password_hash(password.trim_end()));
            Ok(0)
        }
        Some("--help" | "-h" | "help") => {
            print!("{USAGE}");
            Ok(0)
        }
        Some("--version" | "-V") => {
            println!("orgthread {}", orgthread::VERSION);
            Ok(0)
        }
        Some(other) => Err(format!("there is no command {other}")),
        None => Err("name a command".into()),
    }
}

fn cmd_fingerprint(args: &[String]) -> Result<i32, String> {
    let [file] = args else {
        return Err("fingerprint takes one file".into());
    };
    match fs::read(file) {
        Ok(bytes) => {
            println!("{}", fingerprint(&bytes));
            Ok(0)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            eprintln!("orgthread: there is no file {file}");
            Ok(exit::MISSING)
        }
        Err(e) => {
            eprintln!("orgthread: could not read {file}: {e}");
            Ok(exit::FAILED)
        }
    }
}

struct ReadArgs {
    file: String,
    who: String,
    to: Option<String>,
}

fn parse_read(args: &[String], command: &str) -> Result<ReadArgs, String> {
    let mut file = None;
    let mut who = None;
    let mut to = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--who" => who = Some(rest.next().ok_or("--who needs a name")?.clone()),
            "--to" => to = Some(rest.next().ok_or("--to needs a file")?.clone()),
            other if other.starts_with("--") => return Err(format!("there is no option {other}")),
            other => {
                if file.replace(other.to_string()).is_some() {
                    return Err(format!("{command} takes one file"));
                }
            }
        }
    }
    Ok(ReadArgs {
        file: file.ok_or(format!("{command} needs a file"))?,
        who: checked_who(who)?,
        to,
    })
}

fn checked_who(who: Option<String>) -> Result<String, String> {
    let who = who.ok_or("--who is required: give the same name of this agent each time")?;
    if who.trim().is_empty() || who.chars().any(char::is_control) {
        return Err("--who must be a name on one line".into());
    }
    Ok(who)
}

struct WriteArgs {
    file: String,
    who: String,
    size: Option<usize>,
}

fn parse_write(args: &[String], command: &str) -> Result<WriteArgs, String> {
    let mut file = None;
    let mut who = None;
    let mut size = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--size" => {
                let value = rest.next().ok_or("--size needs a number of bytes")?;
                let bytes = value
                    .parse()
                    .map_err(|_| format!("--size needs a number of bytes, not {value}"))?;
                size = Some(bytes);
            }
            "--who" => who = Some(rest.next().ok_or("--who needs a name")?.clone()),
            other if other.starts_with("--") => return Err(format!("there is no option {other}")),
            other => {
                if file.replace(other.to_string()).is_some() {
                    return Err(format!("{command} takes one file"));
                }
            }
        }
    }
    Ok(WriteArgs {
        file: file.ok_or(format!("{command} needs a file"))?,
        who: checked_who(who)?,
        size,
    })
}

fn home() -> Result<PathBuf, String> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set".to_string())
}

/// The path of the file from the root, with `/` between its parts. It follows
/// links first, so one file has one lock name, whichever way it is reached.
fn path_in_root(file: &str, root: &Path) -> Result<String, String> {
    let given = Path::new(file);
    let real = if fs::symlink_metadata(given).is_ok() {
        fs::canonicalize(given).map_err(|e| format!("could not find {file}: {e}"))?
    } else {
        let name = given
            .file_name()
            .ok_or_else(|| format!("{file} names no file"))?;
        let parent = match given.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        fs::canonicalize(parent)
            .map_err(|e| format!("the folder of {file} does not exist: {e}"))?
            .join(name)
    };
    let inside = real
        .strip_prefix(root)
        .map_err(|_| format!("{file} is outside the folder {}", root.display()))?;
    inside
        .to_str()
        .map(str::to_string)
        .ok_or_else(|| format!("the path of {file} is not valid UTF-8"))
}

struct ListArgs {
    folders: Vec<String>,
    depth: Option<usize>,
    all: bool,
}

fn parse_list(args: &[String]) -> Result<ListArgs, String> {
    let mut folders = Vec::new();
    let mut depth = Some(1);
    let mut all = false;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            // After --, each word is a folder, also one named like an option.
            "--" => folders.extend(rest.by_ref().cloned()),
            "--all" => all = true,
            "--depth" => {
                let value = rest.next().ok_or("--depth needs a number from 1, or all")?;
                depth = match value.as_str() {
                    "all" => None,
                    n => match n.parse::<usize>() {
                        Ok(n) if n >= 1 => Some(n),
                        _ => return Err(format!("--depth needs a number from 1, or all, not {n}")),
                    },
                };
            }
            other if other.starts_with("--") => return Err(format!("there is no option {other}")),
            other => folders.push(other.to_string()),
        }
    }
    if folders.is_empty() {
        return Err("list needs a folder".into());
    }
    Ok(ListArgs {
        folders,
        depth,
        all,
    })
}

/// The files and folders in each folder, one group per folder.
fn cmd_list(args: &[String]) -> Result<i32, String> {
    let ListArgs {
        folders,
        depth,
        all,
    } = parse_list(args)?;
    let settings = Settings::load()?;
    let storage = settings.storage()?;
    let targets: Vec<Target> = folders
        .iter()
        .map(|shown| Target {
            shown: shown.clone(),
            path: settings.folder_of(shown),
        })
        .collect();
    let options = Options {
        depth,
        all,
        zone: TimeZone::system(),
    };
    let limits = |path: &str, writing: bool| settings.limits(path, writing);
    let (text, code) = list::list(storage.as_ref(), &targets, &options, &limits);
    let mut out = io::stdout();
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => Ok(code),
        Err(e) => {
            say(format!("could not hand over the list: {e}"));
            Ok(exit::FAILED)
        }
    }
}

/// The exact text, and a note of what this name read, from one single read.
fn cmd_read(args: &[String]) -> Result<i32, String> {
    let ReadArgs { file, who, to } = parse_read(args, "read")?;
    let (file, to) = (&file, to.as_ref());
    let settings = Settings::load()?;
    let path = settings.path_of(file)?;
    settings.limits(&path, false)?;
    let storage = settings.storage()?;
    let bytes = match storage.read(&path) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            say(format!("there is no file {path}"));
            return Ok(exit::MISSING);
        }
        Err(e) => {
            say(format!("could not read {path}: {e}"));
            return Ok(exit::FAILED);
        }
    };
    if !bytes.is_empty()
        && let Err(reason) = write::check_text(&bytes)
    {
        return Err(format!("{path}: {reason}"));
    }
    let reads = Reads::new(&settings.reads);
    reads.tidy();
    let mut out = io::stdout();
    let sent = match to {
        None => {
            if let Err(e) = reads.remember(&who, &path, &fingerprint(&bytes)) {
                say(format!("could not note this read: {e}"));
                return Ok(exit::FAILED);
            }
            out.write_all(&bytes)
        }
        Some(to) => {
            let mut made = match new_copy(to, &settings)? {
                Ok(made) => made,
                Err(code) => return Ok(code),
            };
            // The note comes last: a failed copy must not count as read.
            made.write_all(&bytes)
                .and_then(|()| made.sync_all())
                .and_then(|()| reads.remember(&who, &path, &fingerprint(&bytes)))
        }
    };
    match sent.and_then(|()| out.flush()) {
        Ok(()) => Ok(0),
        Err(e) => {
            say(format!("could not hand over {path}: {e}"));
            Ok(exit::FAILED)
        }
    }
}

/// A new working copy at `to`, never inside the root, where it would sync to
/// every device. An exit code when it cannot be made.
fn new_copy(to: &str, settings: &Settings) -> Result<Result<fs::File, i32>, String> {
    let copy = Path::new(to);
    let parent = match copy.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let parent =
        fs::canonicalize(parent).map_err(|e| format!("the folder of {to} does not exist: {e}"))?;
    if !settings.dropbox && parent.starts_with(&settings.root) {
        return Err(format!(
            "{to} is inside {}, where a working copy would sync to every device",
            settings.root.display()
        ));
    }
    let created = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(copy);
    match created {
        Ok(made) => Ok(Ok(made)),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            say(format!("refused: {to} exists already. Name a new copy."));
            Ok(Err(exit::EXISTS))
        }
        Err(e) => {
            say(format!("could not make {to}: {e}"));
            Ok(Err(exit::FAILED))
        }
    }
}

struct Settings {
    root: PathBuf,
    locks: PathBuf,
    log: PathBuf,
    reads: PathBuf,
    versions: PathBuf,
    dropbox: bool,
}

impl Settings {
    fn storage(&self) -> Result<Box<dyn Storage>, String> {
        if self.dropbox {
            Ok(Box::new(Dropbox::new(&self.locks)?))
        } else {
            Ok(Box::new(LocalFolder::new(&self.root, &self.locks)))
        }
    }

    /// The path from the root. In Dropbox a file is named from there already.
    fn limits(&self, path: &str, writing: bool) -> Result<(), String> {
        let var = |name: &str| env::var(name).unwrap_or_default();
        write::check_limits(
            path,
            writing,
            &var("ORGTHREAD_ALLOW"),
            &var("ORGTHREAD_DENY"),
            &var("ORGTHREAD_READ_ONLY"),
        )
    }

    fn path_of(&self, file: &str) -> Result<String, String> {
        if self.dropbox {
            let inside = file
                .strip_prefix('/')
                .ok_or_else(|| format!("{file} must start with / in Dropbox"))?;
            let bad = inside.is_empty()
                || inside.ends_with('/')
                || inside
                    .split('/')
                    .any(|p| p.is_empty() || p == "." || p == "..");
            if bad {
                return Err(format!("{file} names no file"));
            }
            return Ok(inside.to_string());
        }
        let path = path_in_root(file, &self.root)?;
        if fs::metadata(self.root.join(&path)).is_ok_and(|m| !m.is_file()) {
            return Err(format!("{file} is not a file"));
        }
        Ok(path)
    }
}

impl Settings {
    /// The path of a folder from the root, empty for the root itself. A path
    /// that names nothing gets exit code 6, and any other problem exit code 2.
    fn folder_of(&self, folder: &str) -> Result<String, (i32, String)> {
        if self.dropbox {
            let inside = folder
                .strip_prefix('/')
                .ok_or_else(|| {
                    (
                        exit::USAGE,
                        format!("{folder} must start with / in Dropbox"),
                    )
                })?
                .trim_end_matches('/');
            let bad = !inside.is_empty()
                && inside
                    .split('/')
                    .any(|p| p.is_empty() || p == "." || p == "..");
            if bad {
                return Err((exit::USAGE, format!("{folder} is not a clean path")));
            }
            return Ok(inside.to_string());
        }
        if fs::symlink_metadata(folder).is_err() {
            return Err((
                exit::MISSING,
                format!("there is no file or folder {folder}"),
            ));
        }
        path_in_root(folder, &self.root).map_err(|reason| (exit::USAGE, reason))
    }

    fn load() -> Result<Settings, String> {
        let dropbox = env::var("ORGTHREAD_STORAGE").is_ok_and(|v| v == "dropbox");
        // Locks, log and notes live beside the program, and nowhere else.
        let folder = program_folder()?;
        let locks = match env::var_os("ORGTHREAD_LOCK_DIR") {
            Some(dir) => folder.join(dir),
            None => folder.join(".locks"),
        };
        // The first run makes it. A failure shows when a write needs it.
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        let _ = builder.create(&locks);
        let state = folder.join("state");
        let root = if dropbox {
            PathBuf::new()
        } else {
            let root = match env::var_os("ORGTHREAD_ROOT") {
                Some(root) => PathBuf::from(root),
                None => home()?.join("Library/CloudStorage/Dropbox"),
            };
            fs::canonicalize(&root).map_err(|e| {
                format!(
                    "the folder {} does not exist: {e}. Set ORGTHREAD_ROOT to your folder, or ORGTHREAD_STORAGE=dropbox to use the Dropbox API, in the environment or in {}",
                    root.display(),
                    folder.join(".env").display()
                )
            })?
        };
        let log = match env::var_os("ORGTHREAD_LOG") {
            Some(path) => PathBuf::from(path),
            None => state.join("writes.log"),
        };
        let reads = match env::var_os("ORGTHREAD_READS") {
            Some(path) => PathBuf::from(path),
            None => state.join("reads"),
        };
        let versions = match env::var_os("ORGTHREAD_VERSIONS") {
            Some(path) => PathBuf::from(path),
            None => state.join("versions"),
        };
        Ok(Settings {
            root,
            locks,
            log,
            reads,
            versions,
            dropbox,
        })
    }
}

fn cmd_watch(args: &[String]) -> Result<i32, String> {
    let settings = Settings::load()?;
    let locks = &settings.locks;
    // In Dropbox mode no text stays on the disk, so there are no copies to compare.
    let versions = if settings.dropbox {
        Versions::none()
    } else {
        Versions::new(&settings.versions)
    };
    let info = screen::Info::new(settings.dropbox, &settings.log);
    let result = match args {
        [] => {
            if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
                return Err("watch needs a terminal. For plain text, use watch --once".into());
            }
            screen::run(locks, &settings.log, &versions, &info)
        }
        [flag] if flag == "--once" => {
            screen::once(locks, &settings.log, &info, 110, 30).map(|text| {
                let _ = io::stdout().write_all(text.as_bytes());
            })
        }
        _ => return Err("watch takes only --once".into()),
    };
    match result {
        Ok(()) => Ok(0),
        Err(e) => {
            say(format!("the screen failed: {e}"));
            Ok(exit::FAILED)
        }
    }
}

/// Where the door keeps its logins over a restart: `state/logins.json` beside the
/// program, unless the settings name another file.
fn logins_file() -> Result<PathBuf, String> {
    if let Some(file) = env::var_os("ORGTHREAD_LOGINS") {
        return Ok(PathBuf::from(file));
    }
    let state = program_folder()?.join("state");
    // The first run makes it. A failure only means that chats log in again.
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    let _ = builder.create(&state);
    Ok(state.join("logins.json"))
}

/// Where the door may listen. It speaks plain HTTP, so only on this machine, unless
/// the settings say otherwise on purpose.
fn listen_addresses(address: &str) -> Result<Vec<std::net::SocketAddr>, String> {
    if let Some(addresses) = serve::local_addresses(address) {
        return Ok(addresses);
    }
    if env::var("ORGTHREAD_LISTEN_ANYWHERE").is_ok_and(|v| v == "yes") {
        return address
            .to_socket_addrs()
            .map(Iterator::collect)
            .map_err(|e| format!("could not use the address {address}: {e}"));
    }
    Err(format!(
        "refused to listen on {address}: this server speaks plain HTTP, so passwords and notes could cross the network unencrypted. Use an address of this machine, such as 127.0.0.1:8080, behind a program that handles HTTPS. To allow this address on purpose, set ORGTHREAD_LISTEN_ANYWHERE=yes"
    ))
}

fn cmd_serve(args: &[String]) -> Result<i32, String> {
    let exe = env::current_exe().map_err(|e| format!("could not find this program: {e}"))?;
    let result = match args {
        [] => serve::run(io::stdin().lock(), io::stdout(), &exe),
        [flag, address] if flag == "--http" => {
            let base = env::var("ORGTHREAD_PUBLIC_URL").unwrap_or_default();
            let hash = env::var("ORGTHREAD_LOGIN_HASH").unwrap_or_default();
            if !base.starts_with("https://") || hash.len() != 64 {
                return Err("--http needs ORGTHREAD_PUBLIC_URL and ORGTHREAD_LOGIN_HASH".into());
            }
            let addresses = listen_addresses(address)?;
            let logins = logins_file()?;
            serve::run_http(&addresses, &Login::with_file(&base, &hash, &logins), &exe)
        }
        _ => return Err("serve takes nothing, or --http <address>".into()),
    };
    match result {
        Ok(()) => Ok(0),
        Err(e) => {
            say(format!("the server stopped: {e}"));
            Ok(exit::FAILED)
        }
    }
}

fn cmd_write(args: &[String]) -> Result<i32, String> {
    let args = parse_write(args, "write")?;
    let settings = Settings::load()?;
    let reads = Reads::new(&settings.reads);
    let path = settings.path_of(&args.file)?;
    settings.limits(&path, true)?;

    let text = read_input("the new text")?;
    write::check_text(&text)?;
    check_size(args.size, &text, "the new text")?;
    let Some(expect) = expected(&reads, &args.who, &path) else {
        return Ok(exit::FAILED);
    };
    write_whole(
        &settings,
        &reads,
        &path,
        &args.who,
        &expect,
        &text,
        Kind::Write,
    )
}

/// What this name read decides what must hold. An unread file must be new.
fn expected(reads: &Reads, who: &str, path: &str) -> Option<Expect> {
    match reads.recall(who, path) {
        Ok(Some(print)) => Some(Expect::Fingerprint(print)),
        Ok(None) => Some(Expect::Absent),
        Err(e) => {
            say(format!("could not look up what {who} read: {e}"));
            None
        }
    }
}

/// The write procedure for the whole new bytes, with its log line and answer.
fn write_whole(
    settings: &Settings,
    reads: &Reads,
    path: &str,
    who: &str,
    expect: &Expect,
    bytes: &[u8],
    kind: Kind,
) -> Result<i32, String> {
    let now = log::utc_now();
    let storage = settings.storage()?;
    let lock_line = format!("orgthread {who} {now} pid {}", std::process::id());
    let Some(stop) = arm_stop() else {
        return Ok(exit::FAILED);
    };
    let report =
        write::write_unless_stopped(storage.as_ref(), path, expect, bytes, &lock_line, &stop);
    Ok(finish(settings, reads, path, who, &now, &report, kind))
}

/// Takes `--check` out of the arguments, and says if it was there.
fn without_check(args: &[String]) -> (Vec<String>, bool) {
    let check = args.iter().any(|a| a == "--check");
    let rest = args.iter().filter(|a| *a != "--check").cloned().collect();
    (rest, check)
}

/// Bytes on their way out, counted and fingerprinted, and cut at the limit.
struct Counted<W: Write> {
    out: W,
    print: Fingerprinter,
    limit: u64,
}

impl<W: Write> Write for Counted<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.print.size() + buf.len() as u64 > self.limit {
            return Err(io::Error::other("the file grew past the limit"));
        }
        let n = self.out.write(buf)?;
        self.print.update(&buf[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// Any file as exact bytes, in parts, so a large file never sits whole in memory.
fn cmd_download(args: &[String]) -> Result<i32, String> {
    let (args, check) = without_check(args);
    let ReadArgs { file, who, to } = parse_read(&args, "download")?;
    let settings = Settings::load()?;
    let path = settings.path_of(&file)?;
    settings.limits(&path, false)?;
    let limit = transfer::download_limit()?;
    let storage = settings.storage()?;
    let size = match storage.size(&path) {
        Ok(Some(size)) => size,
        Ok(None) => {
            say(format!("there is no file {path}"));
            return Ok(exit::MISSING);
        }
        Err(e) => {
            say(format!("could not look at {path}: {e}"));
            return Ok(exit::FAILED);
        }
    };
    if size > limit {
        say(format!(
            "refused: {path} has {size} bytes, more than the limit of {}",
            transfer::megabytes(limit)
        ));
        return Ok(exit::USAGE);
    }
    if check {
        println!("{size}");
        return Ok(0);
    }
    if to.is_none() && io::stdout().is_terminal() {
        return Err("download gives bytes, not text for a terminal: add --to <copy>".into());
    }
    let reads = Reads::new(&settings.reads);
    reads.tidy();
    let out: Box<dyn Write> = match &to {
        None => Box::new(io::stdout().lock()),
        Some(to) => match new_copy(to, &settings)? {
            Ok(made) => Box::new(made),
            Err(code) => return Ok(code),
        },
    };
    let mut counted = Counted {
        out,
        print: Fingerprinter::default(),
        limit,
    };
    let sent = storage
        .read_to(&path, &mut counted)
        .and_then(|found| counted.flush().map(|()| found));
    let failed = |reason: String| {
        if let Some(to) = &to {
            let _ = fs::remove_file(to);
        }
        say(reason);
    };
    match sent {
        Ok(true) => {}
        Ok(false) => {
            failed(format!("there is no file {path}"));
            return Ok(exit::MISSING);
        }
        Err(e) => {
            failed(format!("could not hand over {path}: {e}"));
            return Ok(exit::FAILED);
        }
    }
    let size = counted.print.size().to_string();
    let print = counted.print.finish();
    // The note comes last: a failed copy must not count as read.
    if let Err(e) = reads.remember(&who, &path, &print) {
        say(format!("could not note this download: {e}"));
        return Ok(exit::FAILED);
    }
    let now = log::utc_now();
    let line = log::line(&[&now, "downloaded", &path, &who, "", &print, &size]);
    if let Err(e) = log::append(&settings.log, &line) {
        say(format!(
            "the log line failed, {}: {e}. The line: {line}",
            settings.log.display()
        ));
        return Ok(exit::NOT_LOGGED);
    }
    Ok(0)
}

/// Any file, with the rules of write. A file that is not text may go only to
/// the folders of ORGTHREAD_BINARY_ALLOW.
fn cmd_upload(args: &[String]) -> Result<i32, String> {
    let (args, check) = without_check(args);
    let args = parse_write(&args, "upload")?;
    let settings = Settings::load()?;
    let reads = Reads::new(&settings.reads);
    let path = settings.path_of(&args.file)?;
    settings.limits(&path, true)?;
    let limit = transfer::upload_limit()?;
    let Some(expect) = expected(&reads, &args.who, &path) else {
        return Ok(exit::FAILED);
    };
    if check {
        // Only what can be known before the bytes come: they are checked again then.
        if transfer::named_binary(&path) && !transfer::binary_allowed(&path) {
            say(format!(
                "refused, no link made. {path} has the name of a file that is not text, and such a file may go only to the folders of {}.",
                transfer::BINARY_ALLOW
            ));
            return Ok(exit::USAGE);
        }
        if expect == Expect::Absent {
            match settings.storage()?.size(&path) {
                Ok(None) => {}
                Ok(Some(_)) => {
                    say(format!(
                        "refused: {path} exists, and {} has not read it. Download it first with the same name.",
                        args.who
                    ));
                    return Ok(exit::EXISTS);
                }
                Err(e) => {
                    say(format!("could not look at {path}: {e}"));
                    return Ok(exit::FAILED);
                }
            }
        }
        return Ok(0);
    }
    let mut bytes = Vec::new();
    io::stdin()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("could not read the new file: {e}"))?;
    if bytes.len() as u64 > limit {
        say(format!(
            "refused, nothing written. The new file has more than the limit of {}.",
            transfer::megabytes(limit)
        ));
        return Ok(exit::USAGE);
    }
    check_size(args.size, &bytes, "the new file")?;
    if bytes.is_empty() {
        return Err("the new file is empty, and an empty upload is refused".into());
    }
    let binary = write::check_text(&bytes).is_err();
    if binary && !transfer::binary_allowed(&path) {
        say(format!(
            "refused, nothing written. {path} is not text, and a file that is not text may go only to the folders of {}.",
            transfer::BINARY_ALLOW
        ));
        return Ok(exit::USAGE);
    }
    write_whole(
        &settings,
        &reads,
        &path,
        &args.who,
        &expect,
        &bytes,
        Kind::Upload { binary },
    )
}

fn cmd_patch(args: &[String]) -> Result<i32, String> {
    let args = parse_write(args, "patch")?;
    let settings = Settings::load()?;
    let reads = Reads::new(&settings.reads);
    let path = settings.path_of(&args.file)?;
    settings.limits(&path, true)?;

    let input = read_input("the patch")?;
    check_size(args.size, &input, "the patch")?;
    let patch = Patch::parse(&input)?;

    let now = log::utc_now();
    // The exact text of each edit is the check, but this name must know the file.
    match reads.recall(&args.who, &path) {
        Ok(Some(_)) => {}
        Ok(None) => {
            let line = log::line(&[&now, "not-read", &path, &args.who, "", "", ""]);
            let logged = log::append(&settings.log, &line);
            say(format!(
                "refused, nothing written. {} has not read {path}. Read it first with: orgthread read <file> --who <same name>",
                args.who
            ));
            if let Err(e) = &logged {
                say(format!(
                    "the log line failed, {}: {e}. The line: {line}",
                    settings.log.display()
                ));
            }
            return Ok(exit::EXISTS);
        }
        Err(e) => {
            say(format!("could not look up what {} read: {e}", args.who));
            return Ok(exit::FAILED);
        }
    }

    let storage = settings.storage()?;
    let lock_line = format!("orgthread {} {now} pid {}", args.who, std::process::id());
    let Some(stop) = arm_stop() else {
        return Ok(exit::FAILED);
    };
    let build = |old: &[u8]| -> Result<(Vec<u8>, Vec<String>), Refusal> {
        let old = std::str::from_utf8(old)
            .map_err(|_| Refusal::NotText("the file is not valid UTF-8".into()))?;
        let applied = patch::apply(old, &patch).map_err(Refusal::Misfit)?;
        let text = applied.text.into_bytes();
        write::check_text(&text).map_err(|reason| {
            Refusal::NotText(reason.replace("the new text", "the text after the patch"))
        })?;
        Ok((text, applied.notes))
    };
    let report = write::patch_unless_stopped(storage.as_ref(), &path, &build, &lock_line, &stop);
    let kind = Kind::Patch {
        edits: patch.edits.len(),
        append: patch.append.is_some(),
    };
    Ok(finish(
        &settings, &reads, &path, &args.who, &now, &report, kind,
    ))
}

/// The whole standard input.
fn read_input(what: &str) -> Result<Vec<u8>, String> {
    let mut input = Vec::new();
    io::stdin()
        .read_to_end(&mut input)
        .map_err(|e| format!("could not read {what}: {e}"))?;
    Ok(input)
}

/// With --size, a cut-off input is refused.
fn check_size(size: Option<usize>, input: &[u8], what: &str) -> Result<(), String> {
    match size {
        Some(size) if size != input.len() => Err(format!(
            "{what} has {} bytes, not the {size} of --size, so it may be cut off",
            input.len()
        )),
        _ => Ok(()),
    }
}

fn arm_stop() -> Option<std::sync::Arc<std::sync::atomic::AtomicBool>> {
    match stop::arm() {
        Ok(stop) => Some(stop),
        Err(e) => {
            say(format!(
                "could not prepare for a stop, so nothing is written: {e}"
            ));
            None
        }
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Write,
    Patch {
        edits: usize,
        append: bool,
    },
    /// A file that is not text gets no copy for the diff screen.
    Upload {
        binary: bool,
    },
}

/// The log line, the answer and the exit code of a write or a patch.
fn finish(
    settings: &Settings,
    reads: &Reads,
    path: &str,
    who: &str,
    now: &str,
    report: &write::Report,
    kind: Kind,
) -> i32 {
    let patching = matches!(kind, Kind::Patch { .. });
    let (before, after, size) = match &report.outcome {
        Outcome::Written { old, new, size, .. } => (
            old.clone().unwrap_or_default(),
            new.clone(),
            size.to_string(),
        ),
        // A refusal for a changed file logs what was expected and what was found.
        Outcome::Changed { expected, found } => (expected.clone(), found.clone(), String::new()),
        _ => (String::new(), String::new(), String::new()),
    };
    let word = match (&report.outcome, patching) {
        (Outcome::Written { .. }, true) => "patched",
        (outcome, _) => outcome.word(),
    };
    let line = log::line(&[now, word, path, who, &before, &after, &size]);

    // First the log. A closed output must never hide what happened to the file.
    let logged = log::append(&settings.log, &line);
    let saved = |rescued: &Option<String>| match rescued {
        Some(_) => " What the file held instead is kept for its owner: orgthread watch shows it.",
        None => "",
    };
    match &report.outcome {
        Outcome::Written {
            new,
            size,
            old_text,
            new_text,
            notes,
            ..
        } => {
            // Only for a person's diff, so a failure here does not fail the write.
            // In Dropbox mode, on a server, no text may stay on the disk.
            if !settings.dropbox && !matches!(kind, Kind::Upload { binary: true }) {
                let versions = Versions::new(&settings.versions);
                versions.tidy();
                let kept = match old_text {
                    Some(old) => versions.save(old),
                    None => Ok(()),
                };
                if let Err(e) = kept.and_then(|()| versions.save(new_text)) {
                    say(format!("could not keep a copy for the diff screen: {e}"));
                }
            }
            // A next write of this agent needs no new read: it knows this text.
            if let Err(e) = reads.remember(who, path, new) {
                say(format!(
                    "could not note this write, so read before the next one: {e}"
                ));
            }
            let mut out = io::stdout();
            let _ = match kind {
                Kind::Write | Kind::Upload { .. } => writeln!(out, "written: {path}, {size} bytes"),
                Kind::Patch { edits, append } => {
                    let mut parts = Vec::new();
                    if edits > 0 {
                        parts.push(format!("{edits} edit{}", if edits == 1 { "" } else { "s" }));
                    }
                    if append {
                        parts.push("1 append".to_string());
                    }
                    writeln!(out, "patched: {path}, {}, {size} bytes", parts.join(", "))
                }
            };
            for note in notes {
                let _ = writeln!(out, "note: {note}");
            }
        }
        Outcome::Locked { holder, age } => say(format!(
            "refused, nothing written. {path} is busy since {} s with another writer: {holder}. Try again in a moment.",
            age.as_secs()
        )),
        Outcome::Changed { .. } if patching => say(format!(
            "refused, nothing written. {path} changed during the patch. Send the same patch again."
        )),
        Outcome::Changed { .. } => say(format!(
            "refused, nothing written. {path} changed since you read it. Read it again and build the change from that text."
        )),
        Outcome::Exists { .. } => say(format!(
            "refused, nothing written. {path} exists, and {who} has not read it. Read it first with: orgthread read <file> --who <same name>"
        )),
        Outcome::Missing { .. } if patching => say(format!(
            "refused, nothing written. There is no file {path}. A new file is made with write."
        )),
        Outcome::Missing { .. } => say(format!(
            "refused, nothing written. {path} is gone since you read it."
        )),
        Outcome::Misfit { reason } => say(format!("refused, nothing written. {reason}")),
        Outcome::NotText { reason } => say(format!("refused, nothing written. {path}: {reason}")),
        Outcome::Restored { reason, rescued } => say(format!(
            "the check after the write failed: {reason}. The old state of {path} is back.{}",
            saved(rescued)
        )),
        Outcome::NotRestored { reason, rescued } => say(format!(
            "DAMAGE POSSIBLE. The check after the write failed: {reason}. Look at {path} now. Its old version is in the Dropbox history.{}",
            saved(rescued)
        )),
        Outcome::Failed { reason } => say(format!("failed, {path} is unchanged: {reason}")),
    }
    if let Some(old) = &report.took_over {
        say(format!(
            "a writer that stopped over 15 minutes ago was cleared first: {old}"
        ));
    }
    if let Some(other) = &report.lock_lost {
        say(format!(
            "another writer may have been at work during this write. Check {path}. That writer: {other}"
        ));
    }
    if let Err(e) = &logged {
        say(format!(
            "the log line failed, {}: {e}. The line: {line}",
            settings.log.display()
        ));
    }
    if let Some(reason) = &report.lock_left {
        say(format!(
            "the file is written, but orgthread's clean-up did not finish: {reason}. There is nothing to do: {path} is free again within 15 minutes."
        ));
    }
    report.exit_code(logged.is_ok())
}

/// A closed error output must not stop the command.
fn say(message: String) {
    let _ = writeln!(io::stderr(), "orgthread: {message}");
}
