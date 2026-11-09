//! What each agent read, so that the agent itself never handles a fingerprint.
//!
//! A read notes the fingerprint under the agent's name and the file's path. A
//! write looks it up. This is one small file per pair, outside the synced folder.

use crate::fingerprint::fingerprint;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::PathBuf;
use std::time::Duration;

/// A note older than this goes. The agent then simply has to read again.
const KEEP: Duration = Duration::from_secs(30 * 24 * 3600);

pub struct Reads {
    dir: PathBuf,
}

impl Reads {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Reads { dir: dir.into() }
    }

    fn note(&self, who: &str, path: &str) -> PathBuf {
        // The name of an agent can hold any sign, so the note's name is a hash.
        self.dir
            .join(fingerprint(format!("{who}\0{path}").as_bytes()))
    }

    pub fn remember(&self, who: &str, path: &str, print: &str) -> io::Result<()> {
        self.remember_rev(who, path, print, None)
    }

    /// The same, with the version the storage gave in the same reply, such as a
    /// Dropbox rev. It sits on a second line of the note.
    pub fn remember_rev(
        &self,
        who: &str,
        path: &str,
        print: &str,
        rev: Option<&str>,
    ) -> io::Result<()> {
        crate::log::private_dir(&self.dir)?;
        let note = self.note(who, path);
        let temp = note.with_extension(format!("{}.tmp", std::process::id()));
        let text = match rev {
            Some(rev) if !rev.is_empty() && !rev.contains('\n') => format!("{print}\n{rev}"),
            _ => print.to_string(),
        };
        fs::write(&temp, text)?;
        fs::rename(&temp, &note).inspect_err(|_| {
            let _ = fs::remove_file(&temp);
        })
    }

    pub fn recall(&self, who: &str, path: &str) -> io::Result<Option<String>> {
        Ok(self.recall_rev(who, path)?.map(|(print, _)| print))
    }

    /// The fingerprint, and the version noted with it, if any.
    pub fn recall_rev(
        &self,
        who: &str,
        path: &str,
    ) -> io::Result<Option<(String, Option<String>)>> {
        match fs::read_to_string(self.note(who, path)) {
            Ok(text) => Ok(Some(match text.split_once('\n') {
                Some((print, rev)) if !rev.is_empty() => (print.to_string(), Some(rev.to_string())),
                Some((print, _)) => (print.to_string(), None),
                None => (text, None),
            })),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Removes old notes. A failure here changes nothing for the agent.
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
        std::env::temp_dir().join(format!("orgthread-reads-{name}-{}", std::process::id()))
    }

    #[cfg(unix)]
    #[test]
    fn a_new_folder_of_notes_is_for_the_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = place("mode");
        let _ = fs::remove_dir_all(&dir);
        Reads::new(&dir)
            .remember("agent", "/a.md", "print")
            .unwrap();
        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_note_is_per_agent_and_per_file() {
        let dir = place("pairs");
        let r = Reads::new(&dir);
        r.remember("chat a", "org/x.md", "one").unwrap();
        r.remember("chat a", "org/y.md", "two").unwrap();
        assert_eq!(
            r.recall("chat a", "org/x.md").unwrap().as_deref(),
            Some("one")
        );
        assert_eq!(
            r.recall("chat a", "org/y.md").unwrap().as_deref(),
            Some("two")
        );
        assert_eq!(r.recall("chat b", "org/x.md").unwrap(), None);
        r.remember("chat a", "org/x.md", "newer").unwrap();
        assert_eq!(
            r.recall("chat a", "org/x.md").unwrap().as_deref(),
            Some("newer")
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_rev_comes_back_with_its_fingerprint_and_a_plain_note_has_none() {
        let dir = place("rev");
        let r = Reads::new(&dir);
        r.remember_rev("chat", "org/x.md", "print", Some("015f00a1"))
            .unwrap();
        assert_eq!(
            r.recall("chat", "org/x.md").unwrap().as_deref(),
            Some("print")
        );
        assert_eq!(
            r.recall_rev("chat", "org/x.md").unwrap(),
            Some(("print".to_string(), Some("015f00a1".to_string())))
        );
        // A write notes a new fingerprint and no rev: the old rev must go.
        r.remember("chat", "org/x.md", "newer").unwrap();
        assert_eq!(
            r.recall_rev("chat", "org/x.md").unwrap(),
            Some(("newer".to_string(), None))
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tidy_removes_only_old_notes() {
        let dir = place("tidy");
        let r = Reads::new(&dir);
        r.remember("old", "org/x.md", "one").unwrap();
        r.remember("new", "org/x.md", "two").unwrap();
        let note = File::options()
            .write(true)
            .open(r.note("old", "org/x.md"))
            .unwrap();
        note.set_modified(SystemTime::now() - KEEP - Duration::from_secs(60))
            .unwrap();
        r.tidy();
        assert_eq!(r.recall("old", "org/x.md").unwrap(), None);
        assert_eq!(r.recall("new", "org/x.md").unwrap().as_deref(), Some("two"));
        fs::remove_dir_all(&dir).unwrap();
    }
}
