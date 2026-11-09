//! The diff on Dropbox: the two versions of a write, from the Dropbox history.
//!
//! On Enter, a thread lists the versions of the file, finds the two of the write
//! by their fingerprint, downloads both at the same time, and checks each against
//! its fingerprint. The texts stay in memory only, never on the disk, in a cache
//! of at most 25 MB that is empty when watch quits.

use crate::dropbox::{self, Dropbox};
use crate::fingerprint::fingerprint;
use crate::storage::{Revision, Storage};
use crate::watch::{self, Entry};
use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::time::Duration;

/// The most text the cache holds, in bytes.
pub const CACHE_LIMIT: usize = 25_000_000;
/// The largest version that gets a diff, in bytes.
pub const MAX_SIZE: u64 = 1_000_000;
/// How long the screen waits for Dropbox.
pub const WAIT: Duration = Duration::from_secs(20);
/// The most versions that Dropbox lists for one file.
pub const LIST_LIMIT: usize = 100;

pub const NOT_TEXT: &str = "Not a text file. There is no diff.";
pub const TOO_BIG: &str =
    "This version is larger than 1 MB. The diff shows only text files up to 1 MB.";
pub const MISMATCH: &str =
    "A version from the Dropbox history does not match its fingerprint. There is no diff.";
pub const TIMEOUT: &str = "Dropbox did not answer in 20 seconds. Press Enter to try again.";
const KEPT_FOR: &str =
    "Dropbox keeps old versions for a limited time, and lists only the 100 newest of a file.";

/// The old versions of a file. Dropbox has them, a local folder does not.
pub trait History: Send + Sync {
    /// The versions still kept, newest first.
    fn revisions(&self, path: &str) -> io::Result<Vec<Revision>>;
    /// The bytes of one version. `None` when it is gone.
    fn read_revision(&self, rev: &str) -> io::Result<Option<Vec<u8>>>;
}

impl History for Dropbox {
    fn revisions(&self, path: &str) -> io::Result<Vec<Revision>> {
        Storage::revisions(self, path, LIST_LIMIT)
    }
    fn read_revision(&self, rev: &str) -> io::Result<Option<Vec<u8>>> {
        Storage::read_revision(self, rev)
    }
}

/// What one Enter asks for: the two fingerprints of a log line. An empty one
/// stands for no file, so its side is empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub path: String,
    pub old: String,
    pub new: String,
    /// A refusal with `changed`: the old side is what was expected, the new side
    /// what was found.
    pub refused: bool,
}

impl Job {
    /// The job for a log line, or the reason there is no diff, before any call.
    pub fn of(entry: &Entry) -> Result<Job, String> {
        let refused = entry.outcome == "changed";
        if !refused && !watch::is_written(&entry.outcome) {
            return Err(format!(
                "{} There is nothing to compare.",
                watch::why(&entry.outcome)
            ));
        }
        if crate::transfer::named_binary(&entry.path) {
            return Err(NOT_TEXT.to_string());
        }
        if entry.size.parse::<u64>().is_ok_and(|size| size > MAX_SIZE) {
            return Err(TOO_BIG.to_string());
        }
        if entry.before.is_empty() && entry.after.is_empty() {
            return Err("This line names no version. There is nothing to compare.".to_string());
        }
        Ok(Job {
            path: entry.path.clone(),
            old: entry.before.clone(),
            new: entry.after.clone(),
            refused,
        })
    }

    /// The fingerprints to fetch, each once.
    fn prints(&self) -> Vec<&str> {
        let mut prints = Vec::new();
        for print in [self.old.as_str(), self.new.as_str()] {
            if !print.is_empty() && !prints.contains(&print) {
                prints.push(print);
            }
        }
        prints
    }

    /// Which side is gone, in plain words.
    pub fn gone(&self, old: bool, new: bool) -> String {
        let what = match (self.refused, old, new) {
            (false, true, true) => "Both versions of this write are gone",
            (false, true, false) => "The version before this write is gone",
            (false, false, _) => "The version after this write is gone",
            (true, true, true) => "Both the expected and the found version are gone",
            (true, true, false) => "The version this chat expected is gone",
            (true, false, _) => "The version that was found is gone",
        };
        format!("{what} from the Dropbox history. {KEPT_FOR}")
    }

    /// What the screen says while it waits.
    pub fn headline(&self) -> &'static str {
        if self.prints().len() > 1 {
            "Getting both versions from the Dropbox history"
        } else {
            "Getting the version from the Dropbox history"
        }
    }
}

/// The versions downloaded so far, in memory only, and the `rev` of each
/// fingerprint per file. When it is full, the version looked at longest ago goes.
pub struct Cache {
    texts: HashMap<String, (String, u64)>,
    bytes: usize,
    limit: usize,
    tick: u64,
    revs: HashMap<String, HashMap<String, Revision>>,
}

impl Cache {
    pub fn new(limit: usize) -> Cache {
        Cache {
            texts: HashMap::new(),
            bytes: 0,
            limit,
            tick: 0,
            revs: HashMap::new(),
        }
    }

    /// The text of a fingerprint, which counts as a look.
    pub fn text(&mut self, print: &str) -> Option<String> {
        self.tick += 1;
        let tick = self.tick;
        self.texts.get_mut(print).map(|(text, used)| {
            *used = tick;
            text.clone()
        })
    }

    pub fn put(&mut self, print: &str, text: String) {
        if text.len() > self.limit {
            return;
        }
        if let Some((old, _)) = self.texts.remove(print) {
            self.bytes -= old.len();
        }
        while self.bytes + text.len() > self.limit {
            let Some(oldest) = self
                .texts
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(print, _)| print.clone())
            else {
                break;
            };
            if let Some((old, _)) = self.texts.remove(&oldest) {
                self.bytes -= old.len();
            }
        }
        self.tick += 1;
        self.bytes += text.len();
        self.texts.insert(print.to_string(), (text, self.tick));
    }

    /// The bytes of text held now.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn holds(&self, print: &str) -> bool {
        self.texts.contains_key(print)
    }

    /// Dropbox does not count capitals in a path, so neither does the map.
    fn key(path: &str) -> String {
        path.to_lowercase()
    }

    pub fn rev(&self, path: &str, print: &str) -> Option<Revision> {
        self.revs.get(&Self::key(path))?.get(print).cloned()
    }

    /// Notes the `rev` of each fingerprint in a list. A `rev` never changes.
    pub fn learn(&mut self, path: &str, list: &[Revision]) {
        let map = self.revs.entry(Self::key(path)).or_default();
        for revision in list {
            map.entry(revision.hash.clone())
                .or_insert_with(|| revision.clone());
        }
    }
}

fn hold(cache: &Mutex<Cache>) -> MutexGuard<'_, Cache> {
    cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The old and the new text of a job.
pub type Texts = (String, String);

/// What the fetch tells the screen as it goes.
#[derive(Debug)]
pub enum Step {
    /// A step that has begun.
    Doing(String),
    /// The step before is done, in these words.
    Did(String),
    Done(Result<Texts, String>),
}

/// An error in short words, with the value of each secret setting taken out,
/// so that no token can reach the screen.
pub fn clean(text: &str) -> String {
    let secrets: Vec<String> = std::iter::once(dropbox::TOKEN)
        .chain(dropbox::REFRESH)
        .filter_map(|name| std::env::var(name).ok())
        .collect();
    clean_of(text, &secrets)
}

fn clean_of(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_string();
    for secret in secrets.iter().filter(|s| s.len() >= 4) {
        out = out.replace(secret.as_str(), "(hidden)");
    }
    out.chars().take(300).collect()
}

fn texts_of(job: &Job, found: &HashMap<String, String>) -> Texts {
    let side = |print: &str| {
        if print.is_empty() {
            String::new()
        } else {
            found.get(print).cloned().unwrap_or_default()
        }
    };
    (side(&job.old), side(&job.new))
}

/// Both texts from memory, when the cache has them: then no call is needed.
pub fn from_memory(job: &Job, cache: &Mutex<Cache>) -> Option<Texts> {
    let mut cache = hold(cache);
    let mut found = HashMap::new();
    for print in job.prints() {
        found.insert(print.to_string(), cache.text(print)?);
    }
    Some(texts_of(job, &found))
}

fn versions(count: usize) -> String {
    if count == 1 {
        "1 version".to_string()
    } else {
        format!("{count} versions")
    }
}

/// Gets both texts of a job: from memory, else from the history, and keeps what
/// it downloaded in the cache, also when nobody waits for it any more.
pub fn fetch(
    job: &Job,
    history: &dyn History,
    cache: &Mutex<Cache>,
    tell: &dyn Fn(Step),
) -> Result<Texts, String> {
    let prints = job.prints();
    let mut found: HashMap<String, String> = HashMap::new();
    {
        let mut c = hold(cache);
        for print in &prints {
            if let Some(text) = c.text(print) {
                found.insert(print.to_string(), text);
            }
        }
    }
    if !found.is_empty() {
        tell(Step::Did(format!("{} from memory", versions(found.len()))));
    }
    let missing: Vec<&str> = prints
        .iter()
        .copied()
        .filter(|p| !found.contains_key(*p))
        .collect();
    if missing.is_empty() {
        return Ok(texts_of(job, &found));
    }

    let known = |c: &Cache| -> Vec<Option<Revision>> {
        missing.iter().map(|p| c.rev(&job.path, p)).collect()
    };
    let mut revs = known(&hold(cache));
    if revs.iter().any(Option::is_none) {
        tell(Step::Doing("Looking in the list of revisions".to_string()));
        let list = history.revisions(&job.path).map_err(|e| {
            format!(
                "Dropbox could not list the versions: {}",
                clean(&e.to_string())
            )
        })?;
        let mut c = hold(cache);
        c.learn(&job.path, &list);
        revs = known(&c);
        let listed = revs.iter().filter(|r| r.is_some()).count();
        tell(Step::Did(format!(
            "Found {} in the list of revisions",
            versions(listed)
        )));
    }
    let is_gone = |print: &str, revs: &[Option<Revision>]| {
        missing
            .iter()
            .zip(revs)
            .any(|(p, r)| *p == print && r.is_none())
    };
    let old_gone = is_gone(&job.old, &revs);
    let new_gone = is_gone(&job.new, &revs);
    if old_gone || new_gone {
        return Err(job.gone(old_gone, new_gone));
    }
    let revs: Vec<Revision> = revs.into_iter().flatten().collect();
    if revs.iter().any(|r| r.size > MAX_SIZE) {
        return Err(TOO_BIG.to_string());
    }

    tell(Step::Doing(if revs.len() > 1 {
        "Downloading before and after, at the same time".to_string()
    } else {
        "Downloading the version".to_string()
    }));
    let results: Vec<io::Result<Option<Vec<u8>>>> = std::thread::scope(|scope| {
        let handles: Vec<_> = revs
            .iter()
            .map(|r| scope.spawn(|| history.read_revision(&r.rev)))
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err(io::Error::other("the download stopped")))
            })
            .collect()
    });

    let mut gone = Vec::new();
    let mut problem = None;
    for (print, result) in missing.iter().zip(results) {
        match result {
            Ok(Some(bytes)) => {
                if fingerprint(&bytes) != *print {
                    problem.get_or_insert(MISMATCH.to_string());
                    continue;
                }
                match String::from_utf8(bytes) {
                    Ok(text) if !text.contains('\0') => {
                        hold(cache).put(print, text.clone());
                        found.insert(print.to_string(), text);
                    }
                    _ => {
                        problem.get_or_insert(NOT_TEXT.to_string());
                    }
                }
            }
            Ok(None) => gone.push(*print),
            Err(e) => {
                problem.get_or_insert(format!(
                    "Dropbox could not give a version: {}",
                    clean(&e.to_string())
                ));
            }
        }
    }
    if let Some(problem) = problem {
        return Err(problem);
    }
    if !gone.is_empty() {
        return Err(job.gone(
            gone.contains(&job.old.as_str()),
            gone.contains(&job.new.as_str()),
        ));
    }
    tell(Step::Did(format!("Downloaded {}", versions(missing.len()))));
    Ok(texts_of(job, &found))
}

/// Opens a fresh link to the history for each fetch, so that a token never
/// goes stale while the screen stays open for hours.
pub type Open = Arc<dyn Fn() -> Result<Arc<dyn History>, String> + Send + Sync>;

/// The diff source of watch in Dropbox mode.
pub struct Remote {
    open: Open,
    cache: Arc<Mutex<Cache>>,
}

impl Remote {
    pub fn new(open: Open) -> Remote {
        Remote {
            open,
            cache: Arc::new(Mutex::new(Cache::new(CACHE_LIMIT))),
        }
    }

    /// Dropbox, with the settings of this program. No call happens before Enter.
    pub fn dropbox(lock_dir: PathBuf) -> Remote {
        Remote::new(Arc::new(move || {
            Dropbox::new(&lock_dir).map(|d| Arc::new(d) as Arc<dyn History>)
        }))
    }

    pub fn from_memory(&self, job: &Job) -> Option<Texts> {
        from_memory(job, &self.cache)
    }

    /// Starts the fetch in a thread of its own. Its steps and its answer come
    /// through the channel. Whoever stops listening, the cache still gets it.
    pub fn start(&self, job: Job) -> mpsc::Receiver<Step> {
        let (send, receive) = mpsc::channel();
        let open = Arc::clone(&self.open);
        let cache = Arc::clone(&self.cache);
        std::thread::spawn(move || {
            let tell = |step: Step| {
                let _ = send.send(step);
            };
            let result = match open() {
                Ok(history) => fetch(&job, history.as_ref(), &cache, &tell),
                Err(e) => Err(format!("Could not reach Dropbox: {}", clean(&e))),
            };
            tell(Step::Done(result));
        });
        receive
    }

    #[cfg(test)]
    pub fn cache(&self) -> &Mutex<Cache> {
        &self.cache
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A history in memory, with invented texts, that counts its calls.
    #[derive(Default)]
    pub struct Fake {
        pub revisions: Vec<Revision>,
        pub bytes: HashMap<String, Vec<u8>>,
        pub lists: AtomicUsize,
        pub downloads: AtomicUsize,
        /// The downloads wait for this before they answer, when set.
        pub gate: Option<Mutex<mpsc::Receiver<()>>>,
    }

    impl Fake {
        /// A file with these texts, newest last.
        pub fn with(texts: &[&str]) -> Fake {
            let mut fake = Fake::default();
            for (i, text) in texts.iter().enumerate().rev() {
                fake.add(&format!("r{i}"), text.as_bytes());
            }
            fake
        }

        pub fn add(&mut self, rev: &str, bytes: &[u8]) {
            self.revisions.push(Revision {
                rev: rev.to_string(),
                hash: fingerprint(bytes),
                size: bytes.len() as u64,
                modified: None,
            });
            self.bytes.insert(rev.to_string(), bytes.to_vec());
        }
    }

    impl History for Fake {
        fn revisions(&self, _path: &str) -> io::Result<Vec<Revision>> {
            self.lists.fetch_add(1, Ordering::SeqCst);
            Ok(self.revisions.clone())
        }
        fn read_revision(&self, rev: &str) -> io::Result<Option<Vec<u8>>> {
            if let Some(gate) = &self.gate {
                let _ = gate.lock().unwrap().recv();
            }
            self.downloads.fetch_add(1, Ordering::SeqCst);
            Ok(self.bytes.get(rev).cloned())
        }
    }

    pub fn entry(outcome: &str, before: &str, after: &str) -> Entry {
        Entry {
            time: "2026-11-09 08:00:00Z".into(),
            outcome: outcome.into(),
            path: "org/notes/plan.md".into(),
            who: "test".into(),
            before: before.into(),
            after: after.into(),
            size: String::new(),
        }
    }

    fn job(before: &str, after: &str) -> Job {
        Job::of(&entry("written", before, after)).unwrap()
    }

    const OLD: &str = "- buy apples\n- call Ana\n";
    const NEW: &str = "- buy apples\n- call Ana\n- water the plants\n";

    fn run(job: &Job, fake: &Fake, cache: &Mutex<Cache>) -> Result<Texts, String> {
        fetch(job, fake, cache, &|_| {})
    }

    #[test]
    fn the_fingerprints_map_to_revs_from_a_list_of_revisions() {
        let fake = Fake::with(&[OLD, NEW]);
        let cache = Mutex::new(Cache::new(CACHE_LIMIT));
        let job = job(&fingerprint(OLD.as_bytes()), &fingerprint(NEW.as_bytes()));
        assert_eq!(run(&job, &fake, &cache).unwrap(), (OLD.into(), NEW.into()));
        let c = hold(&cache);
        assert_eq!(c.rev("org/notes/plan.md", &job.old).unwrap().rev, "r0");
        assert_eq!(c.rev("ORG/notes/plan.md", &job.new).unwrap().rev, "r1");
        assert_eq!(fake.lists.load(Ordering::SeqCst), 1);
        assert_eq!(fake.downloads.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn the_same_write_again_needs_no_call_and_the_next_needs_one_download() {
        let third = "- buy apples\n- water the plants\n";
        let fake = Fake::with(&[OLD, NEW, third]);
        let cache = Mutex::new(Cache::new(CACHE_LIMIT));
        let first = job(&fingerprint(OLD.as_bytes()), &fingerprint(NEW.as_bytes()));
        run(&first, &fake, &cache).unwrap();
        assert_eq!(from_memory(&first, &cache), Some((OLD.into(), NEW.into())));
        run(&first, &fake, &cache).unwrap();
        assert_eq!(fake.lists.load(Ordering::SeqCst), 1);
        assert_eq!(fake.downloads.load(Ordering::SeqCst), 2);
        // The "after" of one write is the "before" of the next.
        let next = job(&fingerprint(NEW.as_bytes()), &fingerprint(third.as_bytes()));
        assert_eq!(run(&next, &fake, &cache).unwrap().1, third);
        assert_eq!(fake.downloads.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn a_new_file_has_an_empty_old_side() {
        let fake = Fake::with(&[NEW]);
        let cache = Mutex::new(Cache::new(CACHE_LIMIT));
        let job = job("", &fingerprint(NEW.as_bytes()));
        assert_eq!(
            run(&job, &fake, &cache).unwrap(),
            (String::new(), NEW.into())
        );
        assert_eq!(fake.downloads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_gone_side_is_named_and_shows_no_text() {
        let gone = fingerprint(b"an old text that Dropbox no longer keeps\n");
        let fake = Fake::with(&[NEW]);
        let cache = Mutex::new(Cache::new(CACHE_LIMIT));
        let new = fingerprint(NEW.as_bytes());

        let before = run(&job(&gone, &new), &fake, &cache).unwrap_err();
        assert!(
            before.starts_with("The version before this write is gone"),
            "{before}"
        );
        assert!(before.contains("limited time"), "{before}");

        let after = run(&job(&new, &gone), &fake, &cache).unwrap_err();
        assert!(
            after.starts_with("The version after this write is gone"),
            "{after}"
        );

        let other = fingerprint(b"another lost text\n");
        let both = run(&job(&gone, &other), &fake, &cache).unwrap_err();
        assert!(
            both.starts_with("Both versions of this write are gone"),
            "{both}"
        );
        assert_eq!(
            fake.downloads.load(Ordering::SeqCst),
            0,
            "no download for a gone side"
        );
    }

    #[test]
    fn a_wrong_fingerprint_after_the_download_shows_no_diff() {
        let mut fake = Fake::with(&[OLD]);
        // The list names NEW, but the download gives other bytes.
        fake.add("r9", NEW.as_bytes());
        fake.bytes
            .insert("r9".into(), b"- something else\n".to_vec());
        let cache = Mutex::new(Cache::new(CACHE_LIMIT));
        let job = job(&fingerprint(OLD.as_bytes()), &fingerprint(NEW.as_bytes()));
        assert_eq!(run(&job, &fake, &cache).unwrap_err(), MISMATCH);
        assert!(
            !hold(&cache).holds(&job.new),
            "a wrong version is never kept"
        );
    }

    #[test]
    fn a_binary_version_and_a_large_one_get_no_diff() {
        let mut fake = Fake::default();
        fake.add("r0", b"\x89PNG\r\n\x1a\n\0\0");
        let cache = Mutex::new(Cache::new(CACHE_LIMIT));
        let binary = job("", &fingerprint(b"\x89PNG\r\n\x1a\n\0\0"));
        assert_eq!(run(&binary, &fake, &cache).unwrap_err(), NOT_TEXT);

        let big = "a line of an invented long note\n".repeat(40_000);
        let mut fake = Fake::default();
        fake.add("r0", big.as_bytes());
        let large = job("", &fingerprint(big.as_bytes()));
        assert_eq!(run(&large, &fake, &cache).unwrap_err(), TOO_BIG);
        assert_eq!(
            fake.downloads.load(Ordering::SeqCst),
            0,
            "the size is checked first"
        );
    }

    #[test]
    fn a_picture_or_a_large_write_is_refused_before_any_call() {
        let mut picture = entry("written", "", "abc");
        picture.path = "org/photos/garden.jpg".into();
        assert_eq!(Job::of(&picture).unwrap_err(), NOT_TEXT);
        let mut large = entry("written", "", "abc");
        large.size = "1500000".into();
        assert_eq!(Job::of(&large).unwrap_err(), TOO_BIG);
        let locked = entry("locked", "", "");
        assert!(Job::of(&locked).unwrap_err().contains("nothing to compare"));
    }

    #[test]
    fn a_refused_line_compares_expected_with_found() {
        let fake = Fake::with(&[OLD, NEW]);
        let cache = Mutex::new(Cache::new(CACHE_LIMIT));
        let line = entry(
            "changed",
            &fingerprint(OLD.as_bytes()),
            &fingerprint(NEW.as_bytes()),
        );
        let job = Job::of(&line).unwrap();
        assert!(job.refused);
        assert_eq!(run(&job, &fake, &cache).unwrap(), (OLD.into(), NEW.into()));
        let gone = job.gone(true, false);
        assert!(
            gone.starts_with("The version this chat expected is gone"),
            "{gone}"
        );
    }

    #[test]
    fn the_cache_keeps_its_limit_and_drops_the_least_recently_used() {
        let mut cache = Cache::new(30);
        cache.put("a", "x".repeat(10));
        cache.put("b", "y".repeat(10));
        cache.put("c", "z".repeat(10));
        assert_eq!(cache.bytes(), 30);
        // A look at "a" makes "b" the oldest.
        assert!(cache.text("a").is_some());
        cache.put("d", "w".repeat(10));
        assert!(cache.holds("a") && !cache.holds("b"));
        assert!(cache.holds("c") && cache.holds("d"));
        assert_eq!(cache.bytes(), 30);
        cache.put("e", "v".repeat(25));
        assert!(cache.bytes() <= 30);
        assert!(cache.holds("e"));
        cache.put("f", "u".repeat(31));
        assert!(!cache.holds("f"), "a text over the limit is not kept");
    }

    #[test]
    fn a_late_result_still_goes_into_the_cache() {
        let (open_gate, gate) = mpsc::channel();
        let mut fake = Fake::with(&[OLD, NEW]);
        fake.gate = Some(Mutex::new(gate));
        let fake: Arc<dyn History> = Arc::new(fake);
        let remote = Remote::new(Arc::new(move || Ok(Arc::clone(&fake))));
        let job = job(&fingerprint(OLD.as_bytes()), &fingerprint(NEW.as_bytes()));
        let steps = remote.start(job.clone());
        drop(steps);
        open_gate.send(()).unwrap();
        open_gate.send(()).unwrap();
        for _ in 0..200 {
            if remote.from_memory(&job).is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the late result never reached the cache");
    }

    #[test]
    fn an_error_shows_no_secret() {
        let secrets = ["sl.invented-token-123".to_string()];
        let text = clean_of(
            "curl failed: header sl.invented-token-123 refused",
            &secrets,
        );
        assert!(!text.contains("invented-token"), "{text}");
        assert!(text.contains("(hidden)"));
    }
}
