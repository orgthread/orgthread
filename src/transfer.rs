//! The one-time links of `download` and `upload`, and their limits.
//!
//! A link works once, for one file and one direction, and must start within 60
//! seconds. It works without a login, like a key under the mat, so it is short
//! and lives in memory only: a restart ends every link. It never goes into the
//! log or into a message.

use crate::write::is_under;
use std::collections::HashMap;
use std::io::{self, Read};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const DOWNLOAD_MB: &str = "ORGTHREAD_DOWNLOAD_MB";
pub const UPLOAD_MB: &str = "ORGTHREAD_UPLOAD_MB";
pub const BINARY_ALLOW: &str = "ORGTHREAD_BINARY_ALLOW";

/// The time a link has to start. Once started, a transfer may take longer.
pub const START_WITHIN: Duration = Duration::from_secs(5 * 60);

/// At most this many links wait at one time, so a loop in a chat cannot fill
/// the memory of the server.
const MAX_WAITING: usize = 1000;

/// A limit in bytes, from a setting in MB. A MB here is 1,000,000 bytes.
fn limit(name: &str, default_mb: u64) -> Result<u64, String> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => value
            .trim()
            .parse::<u64>()
            .ok()
            .and_then(|mb| mb.checked_mul(1_000_000))
            .ok_or_else(|| format!("{name} needs a whole number of MB, not {value}")),
        _ => Ok(default_mb * 1_000_000),
    }
}

/// The largest download, in bytes: 250 MB unless the settings say otherwise.
pub fn download_limit() -> Result<u64, String> {
    limit(DOWNLOAD_MB, 250)
}

/// The largest upload, in bytes: 20 MB unless the settings say otherwise.
pub fn upload_limit() -> Result<u64, String> {
    limit(UPLOAD_MB, 20)
}

/// A limit in the words of a message: `250 MB`.
pub fn megabytes(bytes: u64) -> String {
    format!("{} MB", bytes / 1_000_000)
}

/// True when a file that is not text may go to `path`. With the setting empty,
/// the default, it may go nowhere.
pub fn binary_allowed(path: &str) -> bool {
    std::env::var(BINARY_ALLOW)
        .unwrap_or_default()
        .split(',')
        .any(|rule| is_under(path, rule.trim()))
}

/// True when the name of `path` ends in the extension of a file that is never
/// text, such as `.png` or `.pdf`. Used before the bytes come, so a link is
/// not made for an upload that would be refused.
pub fn named_binary(path: &str) -> bool {
    const NEVER_TEXT: &[&str] = &[
        "png", "jpg", "jpeg", "gif", "webp", "heic", "heif", "tif", "tiff", "bmp", "ico", "pdf",
        "zip", "gz", "tgz", "7z", "rar", "mp3", "m4a", "wav", "mp4", "mov", "epub", "docx", "xlsx",
        "pptx", "sqlite",
    ];
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => {
            NEVER_TEXT.contains(&ext.to_ascii_lowercase().as_str())
        }
        _ => false,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Download,
    Upload,
}

#[derive(Clone, Debug)]
pub struct Link {
    pub direction: Direction,
    /// The path as the chat gave it, for the command.
    pub file: String,
    pub who: String,
    made: Instant,
}

#[derive(Debug)]
pub enum Taken {
    Ready(Link),
    /// The link exists, but its 5 minutes passed. It is gone now.
    Expired,
    /// The link is for the other direction. It stays, unused.
    WrongDirection,
    /// No such link, or it was used already.
    Unknown,
}

#[derive(Default)]
pub struct Links {
    waiting: Mutex<HashMap<String, Link>>,
}

/// 32 random bytes from the system, in hex.
fn secret() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

impl Links {
    /// A new link. Returns its secret, the last part of its address.
    pub fn make(&self, direction: Direction, file: &str, who: &str) -> io::Result<String> {
        let secret = secret()?;
        let mut waiting = self.waiting.lock().unwrap_or_else(|e| e.into_inner());
        waiting.retain(|_, link| link.made.elapsed() <= START_WITHIN);
        if waiting.len() >= MAX_WAITING {
            return Err(io::Error::other(
                "too many links wait already. Use them, or wait a minute",
            ));
        }
        let link = Link {
            direction,
            file: file.to_string(),
            who: who.to_string(),
            made: Instant::now(),
        };
        waiting.insert(secret.clone(), link);
        Ok(secret)
    }

    /// Takes a link for its one use.
    pub fn take(&self, secret: &str, direction: Direction) -> Taken {
        let mut waiting = self.waiting.lock().unwrap_or_else(|e| e.into_inner());
        match waiting.get(secret) {
            None => return Taken::Unknown,
            Some(link) if link.direction != direction => return Taken::WrongDirection,
            Some(_) => {}
        }
        let Some(link) = waiting.remove(secret) else {
            return Taken::Unknown;
        };
        if link.made.elapsed() > START_WITHIN {
            Taken::Expired
        } else {
            Taken::Ready(link)
        }
    }

    /// Makes a link look older, for tests of the 5 minutes.
    #[cfg(test)]
    fn age_all(&self, by: Duration) {
        let mut waiting = self.waiting.lock().unwrap_or_else(|e| e.into_inner());
        for link in waiting.values_mut() {
            if let Some(earlier) = link.made.checked_sub(by) {
                link.made = earlier;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_works_once() {
        let links = Links::default();
        let s = links.make(Direction::Download, "/a.pdf", "chat a").unwrap();
        assert_eq!(s.len(), 64);
        assert!(
            matches!(links.take(&s, Direction::Download), Taken::Ready(l) if l.file == "/a.pdf")
        );
        assert!(matches!(
            links.take(&s, Direction::Download),
            Taken::Unknown
        ));
    }

    #[test]
    fn a_link_for_the_other_direction_stays_unused() {
        let links = Links::default();
        let s = links.make(Direction::Upload, "/a.jpg", "chat a").unwrap();
        assert!(matches!(
            links.take(&s, Direction::Download),
            Taken::WrongDirection
        ));
        assert!(matches!(links.take(&s, Direction::Upload), Taken::Ready(_)));
    }

    #[test]
    fn a_link_still_works_after_61_seconds() {
        let links = Links::default();
        let s = links.make(Direction::Download, "/a.pdf", "chat a").unwrap();
        links.age_all(Duration::from_secs(61));
        assert!(matches!(
            links.take(&s, Direction::Download),
            Taken::Ready(_)
        ));
    }

    #[test]
    fn the_window_is_5_minutes() {
        assert_eq!(START_WITHIN, Duration::from_secs(300));
    }

    #[test]
    fn a_link_ends_after_5_minutes() {
        let links = Links::default();
        let s = links.make(Direction::Download, "/a.pdf", "chat a").unwrap();
        links.age_all(START_WITHIN + Duration::from_secs(1));
        assert!(matches!(
            links.take(&s, Direction::Download),
            Taken::Expired
        ));
        assert!(matches!(
            links.take(&s, Direction::Download),
            Taken::Unknown
        ));
    }

    #[test]
    fn two_links_differ() {
        let links = Links::default();
        let a = links.make(Direction::Download, "/a", "x").unwrap();
        let b = links.make(Direction::Download, "/a", "x").unwrap();
        assert_ne!(a, b);
    }
}
