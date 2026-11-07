//! The one small interface that every kind of storage implements.
//!
//! Today there is one kind: a local folder. A Dropbox version can slot in
//! later: `write_if_unchanged` maps to an upload with a rev check, and the locks
//! stay files in the same lock folder.

use crate::fingerprint::fingerprint;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// What the caller believes about the file before the write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expect {
    /// The file must not exist.
    Absent,
    /// The file must exist and have this fingerprint.
    Fingerprint(String),
}

#[derive(Debug)]
pub enum WriteError {
    /// The file is not what the caller expected. Nothing was written.
    Changed {
        found: Option<String>,
    },
    Io(io::Error),
}

impl From<io::Error> for WriteError {
    fn from(e: io::Error) -> Self {
        WriteError::Io(e)
    }
}

pub trait Storage {
    /// The bytes of a file, or `None` when it does not exist.
    fn read(&self, path: &str) -> io::Result<Option<Vec<u8>>>;
    /// Writes the whole file, as one step, only if it is still as expected.
    fn write_if_unchanged(
        &self,
        path: &str,
        expect: &Expect,
        bytes: &[u8],
    ) -> Result<(), WriteError>;
    /// Puts a file back after a failed check, whatever it holds now.
    fn restore(&self, path: &str, old: Option<&[u8]>) -> io::Result<()>;
    /// Keeps bytes that a restore would destroy. Returns where they are.
    fn rescue(&self, path: &str, bytes: &[u8]) -> io::Result<String>;

    /// Creates the lock. False means that the lock exists already.
    fn lock_create(&self, name: &str, line: &str) -> io::Result<bool>;
    fn lock_read(&self, name: &str) -> io::Result<Option<String>>;
    fn lock_age(&self, name: &str) -> io::Result<Option<Duration>>;
    /// Removes a lock older than `max_age`. Returns its line if this call won.
    fn lock_take_stale(&self, name: &str, max_age: Duration) -> io::Result<Option<String>>;
    fn lock_remove(&self, name: &str) -> io::Result<()>;
}

/// Files in a local folder, for example the Dropbox mirror on a Mac.
pub struct LocalFolder {
    root: PathBuf,
    locks: PathBuf,
}

impl LocalFolder {
    /// A relative `lock_dir` starts at `root`. It must exist already: a missing
    /// lock folder means a wrong root, and a new one would protect nothing.
    pub fn new(root: impl Into<PathBuf>, lock_dir: impl AsRef<Path>) -> Self {
        let root = root.into();
        let locks = root.join(lock_dir);
        LocalFolder { root, locks }
    }

    fn full(&self, path: &str) -> PathBuf {
        self.root.join(path)
    }

    fn lock_path(&self, name: &str) -> PathBuf {
        self.locks.join(name)
    }

    /// The temporary file sits in the lock folder: the same disk, so the move is
    /// one step, and no editor or sync app sees it next to the real file.
    /// The process number keeps two writers apart, even when one of them lost its
    /// lock without knowing it.
    fn write_temp(&self, path: &str, bytes: &[u8], like: Option<&Path>) -> io::Result<PathBuf> {
        let temp = self.lock_path(&format!(
            "{}.{}.tmp",
            path.replace('/', "--"),
            std::process::id()
        ));
        let result = (|| {
            let mut file = File::create(&temp)?;
            file.write_all(bytes)?;
            if let Some(like) = like {
                file.set_permissions(fs::metadata(like)?.permissions())?;
            }
            file.sync_all()
        })();
        match result {
            Ok(()) => Ok(temp),
            Err(e) => {
                let _ = fs::remove_file(&temp);
                Err(e)
            }
        }
    }

    fn sync_parent(target: &Path) {
        if let Some(parent) = target.parent()
            && let Ok(dir) = File::open(parent)
        {
            let _ = dir.sync_all();
        }
    }

    fn found(&self, path: &str) -> io::Result<Option<String>> {
        Ok(self.read(path)?.map(|b| fingerprint(&b)))
    }
}

impl Storage for LocalFolder {
    fn read(&self, path: &str) -> io::Result<Option<Vec<u8>>> {
        match fs::read(self.full(path)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn write_if_unchanged(
        &self,
        path: &str,
        expect: &Expect,
        bytes: &[u8],
    ) -> Result<(), WriteError> {
        let target = self.full(path);
        match expect {
            Expect::Absent => {
                let temp = self.write_temp(path, bytes, None)?;
                // A hard link never replaces what is there: if a file or a
                // dangling link appeared meanwhile, this fails and changes nothing.
                let linked = fs::hard_link(&temp, &target);
                let _ = fs::remove_file(&temp);
                match linked {
                    Ok(()) => {}
                    Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                        return Err(WriteError::Changed {
                            found: self.found(path)?,
                        });
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            Expect::Fingerprint(want) => {
                let temp = self.write_temp(path, bytes, Some(&target))?;
                // The last look right before the move (step 3 of the written
                // procedure): editors of people take no lock.
                let found = match self.found(path) {
                    Ok(found) => found,
                    Err(e) => {
                        let _ = fs::remove_file(&temp);
                        return Err(e.into());
                    }
                };
                if found.as_deref() != Some(want) {
                    let _ = fs::remove_file(&temp);
                    return Err(WriteError::Changed { found });
                }
                if let Err(e) = fs::rename(&temp, &target) {
                    let _ = fs::remove_file(&temp);
                    return Err(e.into());
                }
            }
        }
        Self::sync_parent(&target);
        Ok(())
    }

    fn restore(&self, path: &str, old: Option<&[u8]>) -> io::Result<()> {
        let target = self.full(path);
        match old {
            Some(bytes) => {
                let like = target.exists().then_some(target.as_path());
                let temp = self.write_temp(path, bytes, like)?;
                if let Err(e) = fs::rename(&temp, &target) {
                    let _ = fs::remove_file(&temp);
                    return Err(e);
                }
            }
            None => match fs::remove_file(&target) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            },
        }
        Self::sync_parent(&target);
        Ok(())
    }

    fn rescue(&self, path: &str, bytes: &[u8]) -> io::Result<String> {
        let name = format!("{}.{}.rescue", path.replace('/', "--"), std::process::id());
        let target = self.lock_path(&name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(target.display().to_string())
    }

    fn lock_create(&self, name: &str, line: &str) -> io::Result<bool> {
        if !self.locks.is_dir() {
            return Err(io::Error::new(
                ErrorKind::NotFound,
                format!("the lock folder {} does not exist", self.locks.display()),
            ));
        }
        let path = self.lock_path(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                let written = file.write_all(format!("{line}\n").as_bytes());
                if let Err(e) = written.and_then(|()| file.sync_all()) {
                    let _ = fs::remove_file(&path);
                    return Err(e);
                }
                Ok(true)
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn lock_read(&self, name: &str) -> io::Result<Option<String>> {
        match fs::read(self.lock_path(name)) {
            Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).trim_end().to_string())),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn lock_age(&self, name: &str) -> io::Result<Option<Duration>> {
        match fs::metadata(self.lock_path(name)) {
            // A time in the future, from another device's clock, counts as new.
            Ok(meta) => Ok(Some(meta.modified()?.elapsed().unwrap_or_default())),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn lock_take_stale(&self, name: &str, max_age: Duration) -> io::Result<Option<String>> {
        let path = self.lock_path(name);
        let aside = self.lock_path(&format!("{name}.stale.{}", std::process::id()));
        // A move has one winner. The loser finds nothing and stays refused.
        match fs::rename(&path, &aside) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        }
        let age = fs::metadata(&aside)?
            .modified()?
            .elapsed()
            .unwrap_or_default();
        if age <= max_age {
            // Between our look and our move another writer took over and made a
            // fresh lock. Hand it back, without replacing anything.
            // If a third writer made a lock meanwhile, that lock stays.
            let line = fs::read(&aside)?;
            let back = OpenOptions::new().write(true).create_new(true).open(&path);
            match back {
                Ok(mut file) => file.write_all(&line)?,
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
            fs::remove_file(&aside)?;
            return Ok(None);
        }
        let line = String::from_utf8_lossy(&fs::read(&aside)?)
            .trim_end()
            .to_string();
        fs::remove_file(&aside)?;
        Ok(Some(line))
    }

    fn lock_remove(&self, name: &str) -> io::Result<()> {
        match fs::remove_file(self.lock_path(name)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}
