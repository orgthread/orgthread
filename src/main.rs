use orgthread::dropbox::Dropbox;
use orgthread::fingerprint::fingerprint;
use orgthread::log;
use orgthread::login::Login;
use orgthread::reads::Reads;
use orgthread::screen;
use orgthread::serve;
use orgthread::stop;
use orgthread::storage::{Expect, LocalFolder, Storage};
use orgthread::versions::Versions;
use orgthread::write::{self, Outcome, exit};
use std::env;
use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
orgthread reads and writes a plain text file the same way every time.

For an agent, two commands:

  orgthread read <file> --who <name>             prints the exact text
  orgthread read <file> --who <name> --to <copy> saves an exact copy instead
  orgthread write <file> --who <name>            < the whole new text

Give the same name each time. To change a file, read it first. A new file needs
no read. orgthread remembers what each name read, and it refuses a write when
the file changed since. The agent never handles anything else.

For a chat with no shell:

  orgthread serve                                the same read and write, as MCP tools

For a person:

  orgthread watch                                the read-only screen; Enter shows a diff, Esc goes back
  orgthread watch --once                         the same screen, as plain text
  orgthread fingerprint <file>                   the content hash Dropbox reports
  orgthread serve [--http <address>]             the MCP tools, for a chat with no shell

Options of write: --size <bytes> refuses text of any other length, so a cut-off
pipe cannot replace a whole file.

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

Exit codes:
  0  done                    6  missing: this name read it, but it is gone
  1  failed, file unchanged  7  check failed, old state put back
  2  wrong use, or not text  8  written, but the log line failed
  3  busy: another writer   9  check failed, old state NOT put back
  4  changed: read it again  10 written, but its clean-up did not finish
  5  exists: read it first, or for --to, the copy exists
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
            println!("orgthread {}", env!("CARGO_PKG_VERSION"));
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

fn parse_read(args: &[String]) -> Result<ReadArgs, String> {
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
                    return Err("read takes one file".into());
                }
            }
        }
    }
    Ok(ReadArgs {
        file: file.ok_or("read needs a file")?,
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

fn parse_write(args: &[String]) -> Result<WriteArgs, String> {
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
                    return Err("write takes one file".into());
                }
            }
        }
    }
    Ok(WriteArgs {
        file: file.ok_or("write needs a file")?,
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

/// The exact text, and a note of what this name read, from one single read.
fn cmd_read(args: &[String]) -> Result<i32, String> {
    let ReadArgs { file, who, to } = parse_read(args)?;
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
            let copy = Path::new(to);
            let parent = match copy.parent() {
                Some(p) if !p.as_os_str().is_empty() => p,
                _ => Path::new("."),
            };
            let parent = fs::canonicalize(parent)
                .map_err(|e| format!("the folder of {to} does not exist: {e}"))?;
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
            let mut made = match created {
                Ok(made) => made,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    say(format!("refused: {to} exists already. Name a new copy."));
                    return Ok(exit::EXISTS);
                }
                Err(e) => {
                    say(format!("could not make {to}: {e}"));
                    return Ok(exit::FAILED);
                }
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
    let result = match args {
        [] => {
            if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
                return Err("watch needs a terminal. For plain text, use watch --once".into());
            }
            screen::run(locks, &settings.log, &versions)
        }
        [flag] if flag == "--once" => screen::once(locks, &settings.log, 110, 30).map(|text| {
            let _ = io::stdout().write_all(text.as_bytes());
        }),
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
            serve::run_http(address, &Login::new(&base, &hash), &exe)
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
    let args = parse_write(args)?;
    let settings = Settings::load()?;
    let reads = Reads::new(&settings.reads);
    let path = settings.path_of(&args.file)?;
    settings.limits(&path, true)?;

    let mut text = Vec::new();
    io::stdin()
        .read_to_end(&mut text)
        .map_err(|e| format!("could not read the new text: {e}"))?;
    write::check_text(&text)?;
    if let Some(size) = args.size
        && size != text.len()
    {
        return Err(format!(
            "the new text has {} bytes, not the {size} of --size, so it may be cut off",
            text.len()
        ));
    }

    // What this name read decides what must hold. An unread file must be new.
    let expect = match reads.recall(&args.who, &path) {
        Ok(Some(print)) => Expect::Fingerprint(print),
        Ok(None) => Expect::Absent,
        Err(e) => {
            say(format!("could not look up what {} read: {e}", args.who));
            return Ok(exit::FAILED);
        }
    };

    let now = log::utc_now();
    let storage = settings.storage()?;
    let lock_line = format!("orgthread {} {now} pid {}", args.who, std::process::id());
    let stop = match stop::arm() {
        Ok(stop) => stop,
        Err(e) => {
            say(format!(
                "could not prepare for a stop, so nothing is written: {e}"
            ));
            return Ok(exit::FAILED);
        }
    };
    let report =
        write::write_unless_stopped(storage.as_ref(), &path, &expect, &text, &lock_line, &stop);

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
    let line = log::line(&[
        &now,
        report.outcome.word(),
        &path,
        &args.who,
        &before,
        &after,
        &size,
    ]);

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
            ..
        } => {
            // Only for a person's diff, so a failure here does not fail the write.
            // In Dropbox mode, on a server, no text may stay on the disk.
            if !settings.dropbox {
                let versions = Versions::new(&settings.versions);
                versions.tidy();
                let kept = match old_text {
                    Some(old) => versions.save(old),
                    None => Ok(()),
                };
                if let Err(e) = kept.and_then(|()| versions.save(&text)) {
                    say(format!("could not keep a copy for the diff screen: {e}"));
                }
            }
            // A next write of this agent needs no new read: it knows this text.
            if let Err(e) = reads.remember(&args.who, &path, new) {
                say(format!(
                    "could not note this write, so read before the next one: {e}"
                ));
            }
            let _ = writeln!(io::stdout(), "written: {path}, {size} bytes");
        }
        Outcome::Locked { holder, age } => say(format!(
            "refused, nothing written. {path} is busy since {} s with another writer: {holder}. Try again in a moment.",
            age.as_secs()
        )),
        Outcome::Changed { .. } => say(format!(
            "refused, nothing written. {path} changed since you read it. Read it again and build the change from that text."
        )),
        Outcome::Exists { .. } => say(format!(
            "refused, nothing written. {path} exists, and {} has not read it. Read it first with: orgthread read <file> --who <same name>",
            args.who
        )),
        Outcome::Missing { .. } => say(format!(
            "refused, nothing written. {path} is gone since you read it."
        )),
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
    Ok(report.exit_code(logged.is_ok()))
}

/// A closed error output must not stop the command.
fn say(message: String) {
    let _ = writeln!(io::stderr(), "orgthread: {message}");
}
