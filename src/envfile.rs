//! The file `.env` as its lines, for `orgthread config`. A change touches only
//! the line of its setting: the order, the comments and the lines of unknown
//! names stay as they are. A new setting goes at the end.
//!
//! A save keeps the old file as `.env.before-config-<date>-<time>`, writes the
//! new one in one step, a temporary file and then a rename, sets its rights to
//! 600, and logs one `configured` line per changed name, never a value.

use crate::log;
use crate::settings::{self, Setting};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The log word of a changed setting.
pub const WORD: &str = "configured";

/// The lines of a `.env` file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvFile {
    lines: Vec<String>,
}

/// A name of `.env` that the list does not know, or a second line of a name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unused {
    pub name: String,
    /// True for a second line of a name: the first line wins.
    pub again: bool,
}

impl EnvFile {
    pub fn parse(text: &str) -> EnvFile {
        EnvFile {
            lines: text.lines().map(str::to_string).collect(),
        }
    }

    /// The file at `path`. A missing file is an empty one.
    pub fn read(path: &Path) -> io::Result<EnvFile> {
        match fs::read_to_string(path) {
            Ok(text) => Ok(EnvFile::parse(&text)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(EnvFile::default()),
            Err(e) => Err(e),
        }
    }

    /// The text to write: each line with a newline after it.
    pub fn text(&self) -> String {
        self.lines.iter().map(|l| format!("{l}\n")).collect()
    }

    /// The index of the first line of `name`, the one the loader takes.
    fn first(&self, name: &str) -> Option<usize> {
        self.lines
            .iter()
            .position(|l| settings::parse_line(l).is_some_and(|(n, _)| n == name))
    }

    /// The value of `name`, from its first line, as the loader takes it.
    pub fn value(&self, name: &str) -> Option<&str> {
        let i = self.first(name)?;
        settings::parse_line(&self.lines[i]).map(|(_, value)| value)
    }

    /// Sets `name` to `value`: its first line changes, or a new line goes at
    /// the end.
    pub fn set(&mut self, name: &str, value: &str) {
        let line = format!("{name}={value}");
        match self.first(name) {
            Some(i) => self.lines[i] = line,
            None => self.lines.push(line),
        }
    }

    /// Back to the default: every line of `name` goes, so that no second line
    /// takes its place.
    pub fn remove(&mut self, name: &str) {
        self.lines
            .retain(|l| settings::parse_line(l).is_none_or(|(n, _)| n != name));
    }

    /// The names that the list does not know, and the second lines of a name,
    /// in the order of the file.
    pub fn unused(&self) -> Vec<Unused> {
        let mut seen: Vec<&str> = Vec::new();
        let mut out = Vec::new();
        for line in &self.lines {
            let Some((name, _)) = settings::parse_line(line) else {
                continue;
            };
            let again = seen.contains(&name);
            if again || settings::find(name).is_none() {
                out.push(Unused {
                    name: name.to_string(),
                    again,
                });
            }
            seen.push(name);
        }
        out
    }

    /// The settings of the list whose value differs between `self` and `old`.
    pub fn changed(&self, old: &EnvFile) -> Vec<&'static str> {
        settings::ALL
            .iter()
            .filter(|s| self.value(s.name) != old.value(s.name))
            .map(|s| s.name)
            .collect()
    }

    /// The first wrong value of the file, among the settings that `skip` does
    /// not name, with the reason.
    pub fn problem(&self, skip: &dyn Fn(&str) -> bool) -> Option<String> {
        settings::ALL
            .iter()
            .filter(|s| !skip(s.name))
            .find_map(|s: &Setting| {
                let value = self.value(s.name)?;
                settings::check(s, value.as_ref()).err()
            })
    }
}

/// What a save did.
#[derive(Debug)]
pub struct Saved {
    /// The copy of the old file. None when there was no file before.
    pub copy: Option<PathBuf>,
    /// The names that changed, each with its log line.
    pub changed: Vec<&'static str>,
    /// Each log line that failed, with the reason.
    pub log_failures: Vec<String>,
}

/// Everything a save needs to know besides the two files.
pub struct Place<'a> {
    /// The file `.env`.
    pub file: &'a Path,
    pub log: &'a Path,
    /// `<owner> (config)`.
    pub who: &'a str,
    /// The local date and time for the name of the copy: `2026-11-12-143005`.
    pub stamp: &'a str,
    /// The time of the log lines, in UTC.
    pub now: &'a str,
}

/// The local date and time for the name of a copy.
pub fn stamp_now() -> String {
    jiff::Zoned::now().strftime("%Y-%m-%d-%H%M%S").to_string()
}

/// A new file that only the owner can read and write. It must not exist yet.
fn new_private(path: &Path) -> io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn private(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// The copy `.env.before-config-<stamp>` beside `file`, with a number after it
/// when a copy of the same second is there already. It holds secrets, so only
/// the owner can read it.
fn keep_copy(file: &Path, bytes: &[u8], stamp: &str) -> io::Result<PathBuf> {
    let name = file
        .file_name()
        .map_or(".env".into(), |n| n.to_string_lossy().into_owned());
    for n in 1..100 {
        let more = if n == 1 {
            String::new()
        } else {
            format!("-{n}")
        };
        let copy = file.with_file_name(format!("{name}.before-config-{stamp}{more}"));
        let mut made = match new_private(&copy) {
            Ok(made) => made,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        made.write_all(bytes)?;
        made.sync_all()?;
        return Ok(copy);
    }
    Err(io::Error::other("too many copies of this second"))
}

/// Saves `new` over the file that held `old`. Nothing is written when a value
/// is wrong, or when the file on the disk is no longer `old`: then someone
/// changed it since the screen read it.
pub fn save(
    place: &Place,
    old: &EnvFile,
    new: &EnvFile,
    skip: &dyn Fn(&str) -> bool,
) -> Result<Saved, String> {
    if let Some(reason) = new.problem(skip) {
        return Err(reason);
    }
    // A link to .env is followed, so the real file changes and the link stays.
    let file = fs::canonicalize(place.file).unwrap_or_else(|_| place.file.to_path_buf());
    let before = match fs::read(&file) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("could not read {}: {e}", file.display())),
    };
    let on_disk = EnvFile::parse(&String::from_utf8_lossy(
        before.as_deref().unwrap_or_default(),
    ));
    if on_disk != *old {
        return Err(format!(
            "{} changed on the disk since this screen read it. Quit, and open orgthread config again",
            file.display()
        ));
    }
    let copy = match &before {
        Some(bytes) => Some(
            keep_copy(&file, bytes, place.stamp)
                .map_err(|e| format!("could not keep a copy of {}: {e}", file.display()))?,
        ),
        None => None,
    };
    let temp = file.with_file_name(format!(".env.config-{}.tmp", std::process::id()));
    let _ = fs::remove_file(&temp);
    let written = new_private(&temp)
        .and_then(|mut made| {
            made.write_all(new.text().as_bytes())?;
            made.sync_all()
        })
        .and_then(|()| fs::rename(&temp, &file))
        .and_then(|()| private(&file));
    if let Err(e) = written {
        let _ = fs::remove_file(&temp);
        return Err(format!("could not write {}: {e}", file.display()));
    }
    let changed = new.changed(old);
    let log_failures = changed
        .iter()
        .filter_map(|name| {
            let line = log::line(&[place.now, WORD, name, place.who, "", "", ""]);
            log::append(place.log, &line).err().map(|e| {
                format!(
                    "the log line failed, {}: {e}. The line: {line}",
                    place.log.display()
                )
            })
        })
        .collect();
    Ok(Saved {
        copy,
        changed,
        log_failures,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{OWNER, UPLOAD_MB};

    const TEXT: &str = "# The settings of an invented door\nORGTHREAD_OWNER=Ana\nORGTHREAD_OWNR=typo\n\nORGTHREAD_UPLOAD_MB=20\nORGTHREAD_OWNER=Second\n";

    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Folder {
            let dir = std::env::temp_dir()
                .join(format!("orgthread-envfile-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Folder(fs::canonicalize(dir).unwrap())
        }
        fn env(&self) -> PathBuf {
            self.0.join(".env")
        }
        fn log(&self) -> PathBuf {
            self.0.join("state/writes.log")
        }
        fn files(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(&self.0)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
        fn save(&self, old: &EnvFile, new: &EnvFile) -> Result<Saved, String> {
            let log = self.log();
            let file = self.env();
            let place = Place {
                file: &file,
                log: &log,
                who: "Ana (config)",
                stamp: "2026-11-12-143005",
                now: "2026-11-12 13:30:05Z",
            };
            save(&place, old, new, &|_| false)
        }
    }

    #[test]
    fn the_first_line_of_a_name_wins_as_for_the_loader() {
        let file = EnvFile::parse(TEXT);
        assert_eq!(file.value(OWNER), Some("Ana"));
        assert_eq!(file.value(UPLOAD_MB), Some("20"));
        assert_eq!(file.value("ORGTHREAD_ROOT"), None);
    }

    #[test]
    fn a_change_keeps_the_order_the_comments_and_the_unknown_lines() {
        let mut file = EnvFile::parse(TEXT);
        file.set(UPLOAD_MB, "50");
        file.set("ORGTHREAD_ROOT", "/srv/invented");
        assert_eq!(
            file.text(),
            "# The settings of an invented door\nORGTHREAD_OWNER=Ana\nORGTHREAD_OWNR=typo\n\nORGTHREAD_UPLOAD_MB=50\nORGTHREAD_OWNER=Second\nORGTHREAD_ROOT=/srv/invented\n"
        );
        assert_eq!(
            file.changed(&EnvFile::parse(TEXT)),
            ["ORGTHREAD_ROOT", UPLOAD_MB]
        );
    }

    #[test]
    fn the_default_removes_every_line_of_the_name() {
        let mut file = EnvFile::parse(TEXT);
        file.remove(OWNER);
        assert_eq!(file.value(OWNER), None, "no second line takes its place");
        assert!(file.text().contains("ORGTHREAD_OWNR=typo"));
        assert!(file.text().starts_with("# The settings"));
    }

    #[test]
    fn unknown_names_and_second_lines_are_not_used() {
        let unused = EnvFile::parse(TEXT).unused();
        assert_eq!(
            unused,
            [
                Unused {
                    name: "ORGTHREAD_OWNR".into(),
                    again: false
                },
                Unused {
                    name: OWNER.into(),
                    again: true
                },
            ]
        );
    }

    #[test]
    fn a_save_keeps_a_copy_writes_in_one_step_and_logs_names_only() {
        let f = Folder::new("save");
        fs::write(f.env(), TEXT).unwrap();
        let old = EnvFile::read(&f.env()).unwrap();
        let mut new = old.clone();
        new.set(UPLOAD_MB, "50");
        new.set("ORGTHREAD_DENY", "private");
        let saved = f.save(&old, &new).unwrap();

        let copy = f.0.join(".env.before-config-2026-11-12-143005");
        assert_eq!(saved.copy.as_deref(), Some(copy.as_path()));
        assert_eq!(fs::read_to_string(&copy).unwrap(), TEXT);
        assert_eq!(fs::read_to_string(f.env()).unwrap(), new.text());
        // No temporary file stays.
        assert_eq!(
            f.files(),
            [".env", ".env.before-config-2026-11-12-143005", "state"]
        );
        let log = fs::read_to_string(f.log()).unwrap();
        assert_eq!(
            log,
            "2026-11-12 13:30:05Z\tconfigured\tORGTHREAD_DENY\tAna (config)\t-\t-\t-\n\
             2026-11-12 13:30:05Z\tconfigured\tORGTHREAD_UPLOAD_MB\tAna (config)\t-\t-\t-\n"
        );
        assert!(!log.contains("50") && !log.contains("private"), "{log}");
        assert!(saved.log_failures.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn the_new_file_and_the_copy_have_the_rights_600() {
        use std::os::unix::fs::PermissionsExt;
        let f = Folder::new("rights");
        fs::write(f.env(), "ORGTHREAD_OWNER=Ana\n").unwrap();
        fs::set_permissions(f.env(), fs::Permissions::from_mode(0o644)).unwrap();
        let old = EnvFile::read(&f.env()).unwrap();
        let mut new = old.clone();
        new.set(OWNER, "Bo");
        let saved = f.save(&old, &new).unwrap();
        for path in [f.env(), saved.copy.unwrap()] {
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{}", path.display());
        }
    }

    #[test]
    fn a_second_save_in_the_same_second_keeps_both_copies() {
        let f = Folder::new("twice");
        fs::write(f.env(), "ORGTHREAD_OWNER=Ana\n").unwrap();
        let first = EnvFile::read(&f.env()).unwrap();
        let mut second = first.clone();
        second.set(OWNER, "Bo");
        f.save(&first, &second).unwrap();
        let mut third = second.clone();
        third.set(OWNER, "Cy");
        let saved = f.save(&second, &third).unwrap();
        assert!(
            saved
                .copy
                .unwrap()
                .ends_with(".env.before-config-2026-11-12-143005-2")
        );
        let copy = f.0.join(".env.before-config-2026-11-12-143005");
        assert_eq!(fs::read_to_string(copy).unwrap(), "ORGTHREAD_OWNER=Ana\n");
    }

    #[test]
    fn a_wrong_value_saves_nothing() {
        let f = Folder::new("wrong");
        fs::write(f.env(), TEXT).unwrap();
        let old = EnvFile::read(&f.env()).unwrap();
        let mut new = old.clone();
        new.set(UPLOAD_MB, "abc");
        let reason = f.save(&old, &new).unwrap_err();
        assert!(reason.contains("ORGTHREAD_UPLOAD_MB"), "{reason}");
        assert_eq!(fs::read_to_string(f.env()).unwrap(), TEXT);
        assert_eq!(f.files(), [".env"], "no copy, no log");
    }

    #[test]
    fn a_file_changed_on_the_disk_since_the_screen_read_it_saves_nothing() {
        let f = Folder::new("raced");
        fs::write(f.env(), TEXT).unwrap();
        let old = EnvFile::read(&f.env()).unwrap();
        fs::write(f.env(), "ORGTHREAD_OWNER=Someone else\n").unwrap();
        let mut new = old.clone();
        new.set(UPLOAD_MB, "50");
        let reason = f.save(&old, &new).unwrap_err();
        assert!(reason.contains("changed on the disk"), "{reason}");
        assert_eq!(
            fs::read_to_string(f.env()).unwrap(),
            "ORGTHREAD_OWNER=Someone else\n"
        );
    }

    #[test]
    fn a_first_save_makes_the_file_with_no_copy() {
        let f = Folder::new("first");
        let old = EnvFile::read(&f.env()).unwrap();
        let mut new = old.clone();
        new.set(OWNER, "Ana");
        let saved = f.save(&old, &new).unwrap();
        assert!(saved.copy.is_none());
        assert_eq!(
            fs::read_to_string(f.env()).unwrap(),
            "ORGTHREAD_OWNER=Ana\n"
        );
    }

    #[test]
    fn a_name_the_environment_holds_is_not_checked_in_the_file() {
        let file = EnvFile::parse("ORGTHREAD_UPLOAD_MB=abc\n");
        assert!(file.problem(&|_| false).is_some());
        assert!(file.problem(&|name| name == UPLOAD_MB).is_none());
    }
}
