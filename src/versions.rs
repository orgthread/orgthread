//! A copy of each version that was written, so that a person can see what changed.
//!
//! One file per version, named by its fingerprint, in a folder outside the synced
//! folder that only this user can open. Copies go after 30 days.

use crate::fingerprint::fingerprint;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::PathBuf;
use std::time::Duration;

const KEEP: Duration = Duration::from_secs(30 * 24 * 3600);

pub struct Versions {
    dir: PathBuf,
    keeps: bool,
}

impl Versions {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Versions {
            dir: dir.into(),
            keeps: true,
        }
    }

    /// For a place that keeps no copies, so that no text stays on its disk.
    pub fn none() -> Self {
        Versions {
            dir: PathBuf::new(),
            keeps: false,
        }
    }

    pub fn keeps_copies(&self) -> bool {
        self.keeps
    }

    fn make_dir(&self) -> io::Result<()> {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&self.dir)
    }

    pub fn save(&self, bytes: &[u8]) -> io::Result<()> {
        self.make_dir()?;
        let path = self.dir.join(fingerprint(bytes));
        if path.exists() {
            // A text that comes back, as after a revert, stays 30 days from now.
            if let Ok(file) = fs::File::options().write(true).open(&path) {
                let _ = file.set_modified(std::time::SystemTime::now());
            }
            return Ok(());
        }
        let temp = path.with_extension(format!("{}.tmp", std::process::id()));
        fs::write(&temp, bytes)?;
        fs::rename(&temp, &path).inspect_err(|_| {
            let _ = fs::remove_file(&temp);
        })
    }

    /// Keeps the old and the new text of a change, after the copies older than
    /// 30 days are gone. A write and a revert of watch share this step.
    pub fn keep(&self, old: Option<&[u8]>, new: &[u8]) -> io::Result<()> {
        self.tidy();
        if let Some(old) = old {
            self.save(old)?;
        }
        self.save(new)
    }

    pub fn load(&self, print: &str) -> io::Result<Option<Vec<u8>>> {
        match fs::read(self.dir.join(print)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Removes old copies. A failure here changes nothing for a write.
    pub fn tidy(&self) {
        let Ok(dir) = fs::read_dir(&self.dir) else {
            return;
        };
        for item in dir.flatten() {
            let old = item
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > KEEP);
            if old {
                let _ = fs::remove_file(item.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::time::SystemTime;

    fn place(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("orgthread-versions-{name}-{}", std::process::id()))
    }

    #[test]
    fn a_saved_version_comes_back_by_its_fingerprint() {
        let dir = place("back");
        let v = Versions::new(&dir);
        v.save("Grüße\n".as_bytes()).unwrap();
        v.save("Grüße\n".as_bytes()).unwrap();
        let print = fingerprint("Grüße\n".as_bytes());
        assert_eq!(v.load(&print).unwrap().unwrap(), "Grüße\n".as_bytes());
        assert_eq!(v.load(&fingerprint(b"other")).unwrap(), None);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn only_this_user_can_open_the_folder() {
        use std::os::unix::fs::PermissionsExt;
        let dir = place("mode");
        Versions::new(&dir).save(b"x").unwrap();
        let mode = fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tidy_removes_only_old_copies() {
        let dir = place("tidy");
        let v = Versions::new(&dir);
        v.save(b"old").unwrap();
        v.save(b"new").unwrap();
        let file = File::options()
            .write(true)
            .open(dir.join(fingerprint(b"old")))
            .unwrap();
        file.set_modified(SystemTime::now() - KEEP - Duration::from_secs(60))
            .unwrap();
        v.tidy();
        assert_eq!(v.load(&fingerprint(b"old")).unwrap(), None);
        assert!(v.load(&fingerprint(b"new")).unwrap().is_some());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_text_saved_again_stays_another_30_days() {
        let dir = place("again");
        let v = Versions::new(&dir);
        v.save(b"back").unwrap();
        let file = File::options()
            .write(true)
            .open(dir.join(fingerprint(b"back")))
            .unwrap();
        file.set_modified(SystemTime::now() - KEEP - Duration::from_secs(60))
            .unwrap();
        v.save(b"back").unwrap();
        v.tidy();
        assert!(v.load(&fingerprint(b"back")).unwrap().is_some());
        v.keep(Some(b"back"), b"other").unwrap();
        assert!(v.load(&fingerprint(b"other")).unwrap().is_some());
        fs::remove_dir_all(&dir).unwrap();
    }
}
