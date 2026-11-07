use orgthread::fingerprint::{fingerprint, is_fingerprint};
use orgthread::log;
use orgthread::screen;
use orgthread::storage::{Expect, LocalFolder, Storage};
use orgthread::write::{self, Outcome, exit};
use std::env;
use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
orgthread writes a plain text file the same way every time.

  orgthread read <file> --to <copy>       saves an exact copy, prints its fingerprint
  orgthread read <file>                   prints the fingerprint, then the exact text
  orgthread fingerprint <file>            only the fingerprint, for a check
  orgthread watch                         the read-only screen: locks, writes, refusals
  orgthread watch --once                  the same screen once, as plain text
  orgthread write <file> --expect <fingerprint> --who <name>   < new-text
  orgthread write <file> --new --who <name>                    < new-text

  To change a file: read it, build the new text from that copy, and give write
  that fingerprint. Text and fingerprint then come from one single read.

  Add --size <bytes> to refuse new text of any other length, so a cut-off
  pipe cannot replace a whole file.

Settings, from the environment:
  ORGTHREAD_ROOT      the folder all paths live in (default: ~/Library/CloudStorage/Dropbox)
  ORGTHREAD_LOCK_DIR  the lock folder inside it (default: org/.locks)
  ORGTHREAD_LOG       the log file (default: ~/.local/state/orgthread/writes.log)

Exit codes:
  0  written                 6  missing: --expect, but no file there
  1  failed, file unchanged  7  check failed, old state put back
  2  wrong use, or not text  8  written, but the log line failed
  3  locked by a writer      9  check failed, old state NOT put back
  4  changed since you read  10 written, but the lock was not released or not ours
  5  exists: --new, but a file is there
";

fn main() -> ExitCode {
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

fn run(args: &[String]) -> Result<i32, String> {
    match args.first().map(String::as_str) {
        Some("fingerprint") => cmd_fingerprint(&args[1..]),
        Some("read") => cmd_read(&args[1..]),
        Some("write") => cmd_write(&args[1..]),
        Some("watch") => cmd_watch(&args[1..]),
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

struct WriteArgs {
    file: String,
    expect: Expect,
    who: String,
    size: Option<usize>,
}

fn parse_write(args: &[String]) -> Result<WriteArgs, String> {
    let mut file = None;
    let mut expect = None;
    let mut who = None;
    let mut size = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--expect" => {
                let value = rest.next().ok_or("--expect needs a fingerprint")?;
                if !is_fingerprint(value) {
                    return Err(format!(
                        "{value} is not a fingerprint: it must be 64 lower-case hex signs"
                    ));
                }
                if expect.replace(Expect::Fingerprint(value.clone())).is_some() {
                    return Err("give --expect or --new once".into());
                }
            }
            "--new" => {
                if expect.replace(Expect::Absent).is_some() {
                    return Err("give --expect or --new once".into());
                }
            }
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
    let who = who.ok_or("--who is required: the lock says who is writing")?;
    if who.trim().is_empty() || who.chars().any(char::is_control) {
        return Err("--who must be a name on one line".into());
    }
    Ok(WriteArgs {
        file: file.ok_or("write needs a file")?,
        expect: expect.ok_or("write needs --expect <fingerprint> or --new")?,
        who,
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

/// Text and fingerprint from one single read, so that no change can slip in
/// between the two.
fn cmd_read(args: &[String]) -> Result<i32, String> {
    let (file, to) = match args {
        [file] => (file, None),
        [file, flag, to] if flag == "--to" => (file, Some(to)),
        [flag, to, file] if flag == "--to" => (file, Some(to)),
        _ => return Err("read takes one file, and maybe --to <copy>".into()),
    };
    let settings = Settings::load()?;
    let path = path_in_root(file, &settings.root)?;
    if fs::metadata(settings.root.join(&path)).is_ok_and(|m| !m.is_file()) {
        return Err(format!("{file} is not a file"));
    }
    let storage = LocalFolder::new(&settings.root, &settings.lock_dir);
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
    let print = fingerprint(&bytes);
    let mut out = io::stdout();
    let sent = match to {
        None => out
            .write_all(format!("{print}\n").as_bytes())
            .and_then(|()| out.write_all(&bytes)),
        Some(to) => {
            let copy = Path::new(to);
            let parent = match copy.parent() {
                Some(p) if !p.as_os_str().is_empty() => p,
                _ => Path::new("."),
            };
            let parent = fs::canonicalize(parent)
                .map_err(|e| format!("the folder of {to} does not exist: {e}"))?;
            if parent.starts_with(&settings.root) {
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
            made.write_all(&bytes)
                .and_then(|()| made.sync_all())
                .and_then(|()| out.write_all(format!("{print}\n").as_bytes()))
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
    lock_dir: String,
    log: PathBuf,
}

impl Settings {
    fn load() -> Result<Settings, String> {
        let root = match env::var_os("ORGTHREAD_ROOT") {
            Some(root) => PathBuf::from(root),
            None => home()?.join("Library/CloudStorage/Dropbox"),
        };
        let root = fs::canonicalize(&root)
            .map_err(|e| format!("the folder {} does not exist: {e}", root.display()))?;
        let lock_dir = env::var("ORGTHREAD_LOCK_DIR").unwrap_or_else(|_| "org/.locks".to_string());
        let log = match env::var_os("ORGTHREAD_LOG") {
            Some(path) => PathBuf::from(path),
            None => home()?.join(".local/state/orgthread/writes.log"),
        };
        Ok(Settings {
            root,
            lock_dir,
            log,
        })
    }
}

fn cmd_watch(args: &[String]) -> Result<i32, String> {
    let settings = Settings::load()?;
    let locks = settings.root.join(&settings.lock_dir);
    let result = match args {
        [] => {
            if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
                return Err("watch needs a terminal. For plain text, use watch --once".into());
            }
            screen::run(&locks, &settings.log)
        }
        [flag] if flag == "--once" => screen::once(&locks, &settings.log, 110, 30).map(|text| {
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

fn cmd_write(args: &[String]) -> Result<i32, String> {
    let args = parse_write(args)?;
    let Settings {
        root,
        lock_dir,
        log: log_path,
    } = Settings::load()?;
    let path = path_in_root(&args.file, &root)?;
    if fs::metadata(root.join(&path)).is_ok_and(|m| !m.is_file()) {
        return Err(format!("{} is not a file", args.file));
    }

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

    let now = log::utc_now();
    let storage = LocalFolder::new(&root, &lock_dir);
    let lock_line = format!("orgthread {} {now} pid {}", args.who, std::process::id());
    let report = write::write(&storage, &path, &args.expect, &text, &lock_line);

    let (before, after, size) = match &report.outcome {
        Outcome::Written { old, new, size } => (
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
    let logged = log::append(&log_path, &line);
    let saved = |rescued: &Option<String>| match rescued {
        Some(place) => format!(" What the file held instead is kept in {place}."),
        None => String::new(),
    };
    match &report.outcome {
        Outcome::Written { .. } => {
            let _ = writeln!(io::stdout(), "{line}");
        }
        Outcome::Locked { holder, age } => say(format!(
            "refused, nothing written. {path} is locked since {} s by: {holder}. Try again when that writer is done.",
            age.as_secs()
        )),
        Outcome::Changed { expected, found } => say(format!(
            "refused, nothing written. {path} changed since you read it. You expected {expected}, and it is {found}. Read it again and build the change from those bytes."
        )),
        Outcome::Exists { found } => say(format!(
            "refused, nothing written. {path} exists already, with fingerprint {found}. To change it, use --expect."
        )),
        Outcome::Missing { expected } => say(format!(
            "refused, nothing written. There is no file {path}, and you expected {expected}."
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
            "took over a stale lock older than 15 minutes, from: {old}"
        ));
    }
    if let Some(other) = &report.lock_lost {
        say(format!(
            "this write no longer held its lock at the end, so another writer may have been at work. Check {path}. The lock now: {other}"
        ));
    }
    if let Err(e) = &logged {
        say(format!(
            "the log line failed, {}: {e}. The line: {line}",
            log_path.display()
        ));
    }
    if let Some(reason) = &report.lock_left {
        say(format!(
            "the lock was NOT released: {reason}. Delete it: {}",
            root.join(&lock_dir).join(write::lock_name(&path)).display()
        ));
    }
    Ok(report.exit_code(logged.is_ok()))
}

/// A closed error output must not stop the command.
fn say(message: String) {
    let _ = writeln!(io::stderr(), "orgthread: {message}");
}
