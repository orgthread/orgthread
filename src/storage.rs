//! The one small interface that every kind of storage implements.
//!
//! Today there is one kind: a local folder. A Dropbox version can slot in
//! later: `write_if_unchanged` maps to an upload with a rev check, and the locks
//! stay files in the same lock folder.

use crate::fingerprint::{Fingerprinter, fingerprint};
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

/// What a remove did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Removed {
    /// The size in bytes of the file that went.
    pub size: u64,
    /// The version that went, where the storage names one, for a restore.
    pub rev: Option<String>,
}

#[derive(Debug)]
pub enum RemoveError {
    /// The file is not what the caller expected, or is gone. Nothing was removed.
    Changed {
        found: Option<String>,
    },
    /// The file went, but the storage reports other bytes than expected.
    /// `reason` says what is known, and where the way back is.
    Unexpected {
        reason: String,
    },
    Io(io::Error),
}

impl From<io::Error> for RemoveError {
    fn from(e: io::Error) -> Self {
        RemoveError::Io(e)
    }
}

/// What a copy made. The two fingerprints are equal when every byte arrived.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Copied {
    /// The fingerprint of the source, as the copy found it.
    pub source: String,
    /// The fingerprint of the new file, as the storage reports it or it reads back.
    pub target: String,
    /// The size in bytes of the new file.
    pub size: u64,
    /// The version of the new file, where the storage names one, for a remove.
    pub rev: Option<String>,
    /// The folders the copy made for the new name, from the root, the outer one
    /// first. Empty when the folder of the new name existed.
    pub folders: Vec<String>,
}

#[derive(Debug)]
pub enum CopyError {
    /// A file or folder is at the target already. Nothing was copied.
    Exists,
    /// There is no source. Nothing was copied.
    Missing,
    /// The source is a folder, or no plain file. Nothing was copied.
    Folder,
    /// The source is not the version the caller gave. Nothing was copied.
    Changed {
        found: String,
    },
    /// Something was made at the target, but it is not a file.
    Unexpected {
        reason: String,
    },
    Io(io::Error),
    /// The copy or the move failed after the step that makes the new folders.
    /// `folders` were missing before that step, the outer one first. Some or
    /// all of them may exist now, and they stay.
    After {
        error: Box<CopyError>,
        folders: Vec<String>,
    },
}

impl CopyError {
    /// This error, with the folders that were missing before the step that
    /// makes them. With no folders, the error stays as it is.
    pub fn after(self, folders: &[String]) -> CopyError {
        if folders.is_empty() || matches!(self, CopyError::After { .. }) {
            return self;
        }
        CopyError::After {
            error: Box::new(self),
            folders: folders.to_vec(),
        }
    }

    /// The folders of an `After`, or none.
    pub fn folders(&self) -> Vec<String> {
        match self {
            CopyError::After { folders, .. } => folders.clone(),
            _ => Vec::new(),
        }
    }
}

/// The folders of `folders` that exist now. After a failure, these are the new
/// folders that stay, and each gets its log line.
pub fn still_there(storage: &dyn Storage, folders: Vec<String>) -> Vec<String> {
    folders
        .into_iter()
        .filter(|folder| storage.exists(folder).unwrap_or(false))
        .collect()
}

impl From<io::Error> for CopyError {
    fn from(e: io::Error) -> Self {
        CopyError::Io(e)
    }
}

/// What a move did. A move gives the same refusals as a copy, so it shares
/// `CopyError`: `Unexpected` then says where the file may be now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Moved {
    /// The fingerprint of the file before the move, as the check found it.
    pub source: String,
    /// The fingerprint of the file at the new name, after the move.
    pub target: String,
    /// The size in bytes of the file at the new name.
    pub size: u64,
    /// The folders the move made for the new name, from the root, the outer one
    /// first. Empty when the folder of the new name existed.
    pub folders: Vec<String>,
}

/// The folders of `path` that do not exist yet, the outer one first. `exists`
/// looks at one folder, named from the root. The look stops at the first folder
/// that exists, so a path in a known folder costs one look.
pub fn new_folders(
    path: &str,
    mut exists: impl FnMut(&str) -> io::Result<bool>,
) -> io::Result<Vec<String>> {
    let path = path.trim_start_matches('/');
    let mut missing = Vec::new();
    let mut folder = path;
    while let Some((parent, _)) = folder.rsplit_once('/') {
        if parent.is_empty() || exists(parent)? {
            break;
        }
        missing.push(parent.to_string());
        folder = parent;
    }
    missing.reverse();
    Ok(missing)
}

/// One entry of a folder, as a list shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub folder: bool,
    /// The size in bytes. A folder has none.
    pub size: Option<u64>,
    /// Seconds since 1970-01-01 UTC. A folder has none, as Dropbox gives none.
    pub modified: Option<i64>,
    /// The fingerprint, when the storage gives it with no extra work. Dropbox
    /// does. A local folder does not, as it would have to read each file.
    pub hash: Option<String>,
}

/// What a path holds, one level down.
#[derive(Debug, PartialEq, Eq)]
pub enum Listing {
    /// A folder and its entries, in no order. `complete` is false when more
    /// entries exist than were fetched.
    Folder {
        entries: Vec<Entry>,
        complete: bool,
    },
    /// The path names a file.
    File(Entry),
    Missing,
}

/// One old or current version of a file, as the history of the storage lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Revision {
    /// The name of this version in the storage. It never changes.
    pub rev: String,
    /// The fingerprint of its bytes.
    pub hash: String,
    pub size: u64,
    /// Seconds since 1970-01-01 UTC, when the storage gives it.
    pub modified: Option<i64>,
}

/// The error of a storage that keeps no history of a file.
pub fn not_kept() -> io::Error {
    io::Error::new(ErrorKind::Unsupported, "this storage keeps no old versions")
}

/// The kind of storage in plain words, for the screen and the tool `version`. It is
/// fixed text, so no path, address or key can reach a screen or a chat through it.
pub fn kind(dropbox: bool) -> &'static str {
    if dropbox { "Dropbox" } else { "local folder" }
}

/// How a walk of a folder for a search began.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Walked {
    /// The folder was there, and the walk went through it.
    Done,
    /// Nothing is at the path.
    Missing,
    /// The path names a file, and a search needs a folder.
    NotFolder,
}

/// The test of a walk: a path from the walked folder, and true for a folder.
/// False leaves the path out. For a folder, the walk may also leave out all it holds.
pub type Keep<'a> = &'a dyn Fn(&str, bool) -> bool;

/// True when `keep` takes `path` and each folder around it, inside the walked
/// folder. A walk that cannot leave a folder out, such as a zip, asks this.
pub fn kept(path: &str, folder: bool, keep: Keep) -> bool {
    let mut end = 0;
    while let Some(i) = path[end..].find('/') {
        end += i;
        if !keep(&path[..end], true) {
            return false;
        }
        end += 1;
    }
    keep(path, folder)
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

    /// The entries of a folder, one level down. `path` is from the root, and
    /// empty for the root itself. It may stop after a little more than `max`
    /// entries, and then says the folder is not complete.
    fn list(&self, path: &str, max: usize) -> io::Result<Listing> {
        let _ = (path, max);
        Err(io::Error::new(
            ErrorKind::Unsupported,
            "this storage cannot list a folder",
        ))
    }

    /// The bytes of a file and the version the storage gives them, where it
    /// names one. Dropbox does, in the same reply, so a delete can later name it.
    fn read_rev(&self, path: &str) -> io::Result<Option<(Vec<u8>, Option<String>)>> {
        Ok(self.read(path)?.map(|bytes| (bytes, None)))
    }

    /// Removes one file, only if it still has the fingerprint of `expect`.
    /// `rev` is the version noted with that fingerprint, when the storage gives
    /// one: then the storage itself checks it at the moment it removes.
    /// A folder is never removed. `Expect::Absent` is wrong use.
    fn remove_if(
        &self,
        path: &str,
        expect: &Expect,
        rev: Option<&str>,
    ) -> Result<Removed, RemoveError> {
        let _ = (path, expect, rev);
        Err(RemoveError::Io(io::Error::new(
            ErrorKind::Unsupported,
            "this storage cannot remove a file",
        )))
    }

    /// Copies one file to a new name. It never replaces anything at `to`, and it
    /// makes the missing parent folders of `to`. With `expect`, the source must
    /// have that fingerprint, or nothing is copied. The caller compares the two
    /// fingerprints of the answer, and removes the copy if they differ.
    fn copy(&self, from: &str, to: &str, expect: Option<&str>) -> Result<Copied, CopyError> {
        let _ = (from, to, expect);
        Err(CopyError::Io(io::Error::new(
            ErrorKind::Unsupported,
            "this storage cannot copy a file",
        )))
    }

    /// Moves one file to a new name, also a new name in the same folder. It never
    /// replaces anything at `to`, and it makes the missing parent folders of `to`.
    /// With `expect`, the source must have that fingerprint, or nothing moves.
    /// A folder is never moved.
    fn move_file(&self, from: &str, to: &str, expect: Option<&str>) -> Result<Moved, CopyError> {
        let _ = (from, to, expect);
        Err(CopyError::Io(io::Error::new(
            ErrorKind::Unsupported,
            "this storage cannot move a file",
        )))
    }

    /// True when a file or a folder is at `path`, named from the root. The
    /// write, the upload, the copy and the move use it with `new_folders`.
    fn exists(&self, path: &str) -> io::Result<bool> {
        Ok(self.list(path, 1)? != Listing::Missing)
    }

    /// The size of a file in bytes, or `None` when it does not exist.
    fn size(&self, path: &str) -> io::Result<Option<u64>> {
        Ok(self.read(path)?.map(|bytes| bytes.len() as u64))
    }

    /// The versions of a file that the storage still keeps, newest first, at
    /// most `limit`. An empty list for a file with no history. A storage with no
    /// history at all, such as a local folder, answers "not kept".
    fn revisions(&self, path: &str, limit: usize) -> io::Result<Vec<Revision>> {
        let _ = (path, limit);
        Err(not_kept())
    }

    /// The bytes of one version, by its `rev`. `None` when it is gone.
    fn read_revision(&self, rev: &str) -> io::Result<Option<Vec<u8>>> {
        let _ = rev;
        Err(not_kept())
    }

    /// Copies the bytes of a file to `out`, in parts where the storage can, so
    /// that a large file never sits whole in memory. False when it does not exist.
    fn read_to(&self, path: &str, out: &mut dyn Write) -> io::Result<bool> {
        match self.read(path)? {
            Some(bytes) => out.write_all(&bytes).map(|()| true),
            None => Ok(false),
        }
    }

    /// Each file and folder under `folder`, at any depth, that `keep` takes. The
    /// path given to `found` is from `folder`, and true marks a folder. `folder`
    /// is from the root, and empty for the root itself.
    fn names(
        &self,
        folder: &str,
        keep: Keep,
        found: &mut dyn FnMut(&str, bool),
    ) -> io::Result<Walked> {
        let _ = (folder, keep, found);
        Err(io::Error::new(
            ErrorKind::Unsupported,
            "this storage cannot search a folder",
        ))
    }

    /// The bytes of each file under `folder`, at any depth, that `keep` takes, one
    /// file at a time. The path given to `found` is from `folder`. `limit` is the
    /// most bytes to hold: a storage that fetches the whole folder at once refuses
    /// a folder above it, with the error kind `FileTooLarge`. A storage that reads
    /// one file at a time skips a file above it. Nothing goes to a disk.
    fn texts(
        &self,
        folder: &str,
        limit: u64,
        keep: Keep,
        found: &mut dyn FnMut(&str, &[u8]),
    ) -> io::Result<Walked> {
        let _ = (folder, limit, keep, found);
        Err(io::Error::new(
            ErrorKind::Unsupported,
            "this storage cannot search a folder",
        ))
    }
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

    /// The fingerprint and the size of a file, read in parts.
    fn print_of(file: &Path) -> io::Result<(String, u64)> {
        let mut input = File::open(file)?;
        let mut print = Fingerprinter::default();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = io::Read::read(&mut input, &mut buf)?;
            if n == 0 {
                break;
            }
            print.update(&buf[..n]);
        }
        let size = print.size();
        Ok((print.finish(), size))
    }

    fn found(&self, path: &str) -> io::Result<Option<String>> {
        Ok(self.read(path)?.map(|b| fingerprint(&b)))
    }
}

/// Seconds since 1970-01-01 UTC, also for a time before it.
fn seconds(time: std::time::SystemTime) -> Option<i64> {
    match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).ok(),
        Err(e) => i64::try_from(e.duration().as_secs()).ok().map(|s| -s),
    }
}

impl LocalFolder {
    /// The entry for a path, following links. `None` for a link that leads
    /// nowhere or out of the root, and for a name that is not valid UTF-8.
    fn entry(&self, full: &Path, name: &std::ffi::OsStr) -> Option<Entry> {
        let name = name.to_str()?.to_string();
        if fs::symlink_metadata(full).ok()?.file_type().is_symlink() {
            let real = fs::canonicalize(full).ok()?;
            let root = fs::canonicalize(&self.root).ok()?;
            if !real.starts_with(root) {
                return None;
            }
        }
        let meta = fs::metadata(full).ok()?;
        let folder = meta.is_dir();
        Some(Entry {
            name,
            folder,
            size: (!folder).then_some(meta.len()),
            modified: if folder {
                None
            } else {
                meta.modified().ok().and_then(seconds)
            },
            hash: None,
        })
    }
}

impl LocalFolder {
    /// The start of a walk: whether `folder` is a folder.
    fn walk_start(&self, folder: &str) -> io::Result<Walked> {
        match fs::metadata(self.full(folder)) {
            Ok(meta) if meta.is_dir() => Ok(Walked::Done),
            Ok(_) => Ok(Walked::NotFolder),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(Walked::Missing),
            Err(e) => Err(e),
        }
    }

    /// Each entry under `folder` that `keep` takes, in the order of the names,
    /// with its full path on the disk. A folder that `keep` refuses is not
    /// entered. A link is left out, so a walk never leaves the root or runs in
    /// a loop. A name that is not valid UTF-8 is left out too.
    fn walk(
        &self,
        folder: &str,
        keep: Keep,
        found: &mut dyn FnMut(&str, &Path, bool) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut todo = vec![String::new()];
        while let Some(inside) = todo.pop() {
            let dir = self.full(folder).join(&inside);
            let mut items = Vec::new();
            for item in fs::read_dir(&dir)? {
                let item = item?;
                let Some(name) = item.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                let kind = item.file_type()?;
                if kind.is_symlink() {
                    continue;
                }
                items.push((name, kind.is_dir()));
            }
            items.sort();
            // The folders go on the stack in reverse, so the first name comes first.
            for (name, is_dir) in items.iter().rev() {
                let path = if inside.is_empty() {
                    name.clone()
                } else {
                    format!("{inside}/{name}")
                };
                if !keep(&path, *is_dir) {
                    continue;
                }
                found(&path, &dir.join(name), *is_dir)?;
                if *is_dir {
                    todo.push(path);
                }
            }
        }
        Ok(())
    }
}

impl Storage for LocalFolder {
    fn exists(&self, path: &str) -> io::Result<bool> {
        match fs::symlink_metadata(self.full(path)) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn size(&self, path: &str) -> io::Result<Option<u64>> {
        match fs::metadata(self.full(path)) {
            Ok(meta) if meta.is_file() => Ok(Some(meta.len())),
            Ok(_) => Err(io::Error::new(
                ErrorKind::InvalidInput,
                format!("{path} is not a file"),
            )),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn read_to(&self, path: &str, out: &mut dyn Write) -> io::Result<bool> {
        let mut file = match File::open(self.full(path)) {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        io::copy(&mut file, out)?;
        Ok(true)
    }

    fn names(
        &self,
        folder: &str,
        keep: Keep,
        found: &mut dyn FnMut(&str, bool),
    ) -> io::Result<Walked> {
        let start = self.walk_start(folder)?;
        if start == Walked::Done {
            self.walk(folder, keep, &mut |path, _, is_dir| {
                found(path, is_dir);
                Ok(())
            })?;
        }
        Ok(start)
    }

    /// Each file is read at the time of the search, and gone again after `found`.
    fn texts(
        &self,
        folder: &str,
        limit: u64,
        keep: Keep,
        found: &mut dyn FnMut(&str, &[u8]),
    ) -> io::Result<Walked> {
        let start = self.walk_start(folder)?;
        if start == Walked::Done {
            self.walk(folder, keep, &mut |path, full, is_dir| {
                if is_dir {
                    return Ok(());
                }
                match fs::metadata(full) {
                    Ok(meta) if meta.len() > limit => return Ok(()),
                    Ok(_) => {}
                    Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
                    Err(e) => return Err(e),
                }
                match fs::read(full) {
                    Ok(bytes) => found(path, &bytes),
                    // Gone since the folder was read: nothing to search.
                    Err(e) if e.kind() == ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
                Ok(())
            })?;
        }
        Ok(start)
    }

    fn list(&self, path: &str, _max: usize) -> io::Result<Listing> {
        let full = self.full(path);
        let meta = match fs::metadata(&full) {
            Ok(meta) => meta,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Listing::Missing),
            Err(e) => return Err(e),
        };
        if !meta.is_dir() {
            let name = full.file_name().unwrap_or_default();
            return match self.entry(&full, name) {
                Some(entry) => Ok(Listing::File(entry)),
                None => Ok(Listing::Missing),
            };
        }
        let mut entries = Vec::new();
        for item in fs::read_dir(&full)? {
            let item = item?;
            if let Some(entry) = self.entry(&item.path(), &item.file_name()) {
                entries.push(entry);
            }
        }
        Ok(Listing::Folder {
            entries,
            complete: true,
        })
    }

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
                // A new file may need new folders. They come right before the
                // link, as for a copy, and stay if the link fails.
                if let Some(parent) = target.parent()
                    && let Err(e) = fs::create_dir_all(parent)
                {
                    let _ = fs::remove_file(&temp);
                    return Err(e.into());
                }
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

    /// The file first moves into the lock folder, in one step. From there no
    /// editor can change it, so the check that follows is the final one. If it
    /// differs, it goes back, unless a new file took its place meanwhile.
    fn remove_if(
        &self,
        path: &str,
        expect: &Expect,
        _rev: Option<&str>,
    ) -> Result<Removed, RemoveError> {
        let Expect::Fingerprint(want) = expect else {
            return Err(RemoveError::Io(io::Error::new(
                ErrorKind::InvalidInput,
                "a remove needs the fingerprint of the file",
            )));
        };
        let target = self.full(path);
        match fs::symlink_metadata(&target) {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => {
                return Err(RemoveError::Io(io::Error::new(
                    ErrorKind::InvalidInput,
                    format!("{path} is not a file, and only a file is removed"),
                )));
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Err(RemoveError::Changed { found: None });
            }
            Err(e) => return Err(e.into()),
        }
        let aside = self.lock_path(&format!(
            "{}.{}.removing",
            path.replace('/', "--"),
            std::process::id()
        ));
        match fs::rename(&target, &aside) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Err(RemoveError::Changed { found: None });
            }
            Err(e) => return Err(e.into()),
        }
        let bytes = match fs::read(&aside) {
            Ok(bytes) => bytes,
            Err(e) => {
                let back = fs::hard_link(&aside, &target).and_then(|()| fs::remove_file(&aside));
                return Err(match back {
                    Ok(()) => e.into(),
                    Err(_) => RemoveError::Unexpected {
                        reason: format!(
                            "the file could not be read after it was moved aside, and it is kept at {}",
                            aside.display()
                        ),
                    },
                });
            }
        };
        let found = fingerprint(&bytes);
        if found != *want {
            // A hard link never replaces a file that appeared meanwhile.
            return match fs::hard_link(&aside, &target) {
                Ok(()) => {
                    let _ = fs::remove_file(&aside);
                    Self::sync_parent(&target);
                    Err(RemoveError::Changed { found: Some(found) })
                }
                Err(_) => Err(RemoveError::Unexpected {
                    reason: format!(
                        "the file changed, and a new file took its place before it could go back. The changed one is kept at {}",
                        aside.display()
                    ),
                }),
            };
        }
        fs::remove_file(&aside)?;
        Self::sync_parent(&target);
        Ok(Removed {
            size: bytes.len() as u64,
            rev: None,
        })
    }

    /// The source goes into a temporary file in the lock folder, with its
    /// fingerprint made on the way. A hard link then puts it in place: it never
    /// replaces a file, so a target that appeared meanwhile stays as it is.
    /// The new file is read back for its fingerprint.
    fn copy(&self, from: &str, to: &str, expect: Option<&str>) -> Result<Copied, CopyError> {
        let source = self.full(from);
        let target = self.full(to);
        match fs::metadata(&source) {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => return Err(CopyError::Folder),
            Err(e) if e.kind() == ErrorKind::NotFound => return Err(CopyError::Missing),
            Err(e) => return Err(e.into()),
        }
        // A first look, so a clear refusal costs no copy of the bytes.
        if fs::symlink_metadata(&target).is_ok() {
            return Err(CopyError::Exists);
        }
        let temp = self.lock_path(&format!(
            "{}.{}.copy",
            to.replace('/', "--"),
            std::process::id()
        ));
        let mut folders = Vec::new();
        let made = (|| -> Result<Copied, CopyError> {
            let mut input = match File::open(&source) {
                Ok(file) => file,
                Err(e) if e.kind() == ErrorKind::NotFound => return Err(CopyError::Missing),
                Err(e) => return Err(e.into()),
            };
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            let mut print = Fingerprinter::default();
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = io::Read::read(&mut input, &mut buf)?;
                if n == 0 {
                    break;
                }
                file.write_all(&buf[..n])?;
                print.update(&buf[..n]);
            }
            file.set_permissions(input.metadata()?.permissions())?;
            file.sync_all()?;
            let source_print = print.finish();
            if let Some(want) = expect
                && want != source_print
            {
                return Err(CopyError::Changed {
                    found: source_print,
                });
            }
            folders = new_folders(to, |folder| self.exists(folder))?;
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            match fs::hard_link(&temp, &target) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::AlreadyExists => return Err(CopyError::Exists),
                Err(e) => return Err(e.into()),
            }
            Self::sync_parent(&target);
            let (target_print, size) = Self::print_of(&target)?;
            Ok(Copied {
                source: source_print,
                target: target_print,
                size,
                rev: None,
                folders: folders.clone(),
            })
        })();
        let _ = fs::remove_file(&temp);
        made.map_err(|e| e.after(&folders))
    }

    /// A hard link puts the file at the new name, and then the old name goes.
    /// The link never replaces a file, so a file that appeared at the new name
    /// meanwhile stays as it is. Only std is used: there is no rename that
    /// refuses a target in std. If the old name cannot go, the new link goes
    /// again, so the file is never left at two names without a word.
    fn move_file(&self, from: &str, to: &str, expect: Option<&str>) -> Result<Moved, CopyError> {
        let source = self.full(from);
        let target = self.full(to);
        match fs::symlink_metadata(&source) {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => return Err(CopyError::Folder),
            Err(e) if e.kind() == ErrorKind::NotFound => return Err(CopyError::Missing),
            Err(e) => return Err(e.into()),
        }
        if fs::symlink_metadata(&target).is_ok() {
            return Err(CopyError::Exists);
        }
        let (print, _) = match Self::print_of(&source) {
            Ok(found) => found,
            Err(e) if e.kind() == ErrorKind::NotFound => return Err(CopyError::Missing),
            Err(e) => return Err(e.into()),
        };
        if let Some(want) = expect
            && want != print
        {
            return Err(CopyError::Changed { found: print });
        }
        let folders = new_folders(to, |folder| self.exists(folder))?;
        let moved = (|| -> Result<Moved, CopyError> {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            match fs::hard_link(&source, &target) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::AlreadyExists => return Err(CopyError::Exists),
                Err(e) if e.kind() == ErrorKind::NotFound => return Err(CopyError::Missing),
                Err(e) => return Err(e.into()),
            }
            Self::sync_parent(&target);
            if let Err(e) = fs::remove_file(&source) {
                return match fs::remove_file(&target) {
                    Ok(()) => {
                        Self::sync_parent(&target);
                        Err(CopyError::Io(io::Error::other(format!(
                            "the old name could not go ({e}), so the new name is removed again, and nothing moved"
                        ))))
                    }
                    Err(again) => Err(CopyError::Unexpected {
                        reason: format!(
                            "the file is at both names now: the old name could not go ({e}), and the new name could not go again ({again})"
                        ),
                    }),
                };
            }
            Self::sync_parent(&source);
            match Self::print_of(&target) {
                Ok((after, size)) => Ok(Moved {
                    source: print,
                    target: after,
                    size,
                    folders: folders.clone(),
                }),
                Err(e) => Err(CopyError::Unexpected {
                    reason: format!(
                        "the file moved, but it could not be read back at the new name: {e}"
                    ),
                }),
            }
        })();
        moved.map_err(|e| e.after(&folders))
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
                format!("orgthread's folder {} does not exist", self.locks.display()),
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
