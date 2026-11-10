//! The answer of `orgthread list`: one group per folder, one line per entry.
//!
//! The text looks like `ls`, so that a model reads it with no help. The size and
//! the time come first and the name last, so a space in a name does no harm.

use crate::fingerprint::Fingerprinter;
use crate::storage::{Entry, Listing, Storage};
use crate::write::exit;
use jiff::Timestamp;
use jiff::tz::TimeZone;

/// At most this many entries per call, over all groups together.
pub const LIMIT: usize = 1000;

const MORE: &str = "more entries not shown: list a smaller folder or a lower depth\n";

pub struct Options {
    /// How many levels down. `None` goes to the bottom.
    pub depth: Option<usize>,
    /// Also the names that start with a dot.
    pub all: bool,
    /// Also the fingerprint of each file, in a column before the name.
    pub fingerprint: bool,
    /// The time zone of the times.
    pub zone: TimeZone,
}

/// One folder that the caller named.
pub struct Target {
    /// The path as given. It heads the group.
    pub shown: String,
    /// The path from the root, or the exit code and the reason it cannot be listed.
    pub path: Result<String, (i32, String)>,
}

/// The limits of a door: `Err` when the path is closed, or with `true`, when it
/// may not be written.
pub type Limits<'a> = &'a dyn Fn(&str, bool) -> Result<(), String>;

/// The text of the answer and its exit code: the code of the first folder that
/// failed, or 0.
pub fn list(
    storage: &dyn Storage,
    targets: &[Target],
    options: &Options,
    limits: Limits,
) -> (String, i32) {
    let mut run = Run {
        storage,
        options,
        limits,
        out: String::new(),
        groups: 0,
        left: LIMIT,
        cut: false,
        code: 0,
    };
    for target in targets {
        if run.cut {
            break;
        }
        match &target.path {
            Ok(path) => run.top(&target.shown, path),
            Err((code, reason)) => run.problem(&target.shown, *code, reason),
        }
    }
    if run.cut {
        run.out.push_str(MORE);
    }
    (run.out, run.code)
}

struct Run<'a> {
    storage: &'a dyn Storage,
    options: &'a Options,
    limits: Limits<'a>,
    out: String,
    groups: usize,
    left: usize,
    cut: bool,
    code: i32,
}

/// The root has an empty path, but a message needs a name for it.
pub(crate) fn named(path: &str) -> &str {
    if path.is_empty() { "/" } else { path }
}

pub(crate) fn join(path: &str, name: &str) -> String {
    if path.is_empty() {
        name.to_string()
    } else {
        format!("{path}/{name}")
    }
}

pub(crate) fn join_shown(shown: &str, name: &str) -> String {
    if shown.ends_with('/') {
        format!("{shown}{name}")
    } else {
        format!("{shown}/{name}")
    }
}

/// The folder of a file as given, so that the header and the name make its path.
fn parent(shown: &str) -> &str {
    match shown.trim_end_matches('/').rfind('/') {
        Some(0) => "/",
        Some(i) => &shown[..i],
        None => ".",
    }
}

fn time_text(seconds: i64, zone: &TimeZone) -> String {
    match Timestamp::from_second(seconds) {
        Ok(t) => t
            .to_zoned(zone.clone())
            .strftime("%Y-%m-%d %a %H:%M")
            .to_string(),
        Err(_) => "-".to_string(),
    }
}

impl Run<'_> {
    fn group(&mut self, shown: &str) {
        if self.groups > 0 {
            self.out.push('\n');
        }
        self.groups += 1;
        self.out.push_str(shown);
        self.out.push_str(":\n");
    }

    fn fail(&mut self, code: i32) {
        if self.code == 0 {
            self.code = code;
        }
    }

    fn problem(&mut self, shown: &str, code: i32, reason: &str) {
        self.group(shown);
        self.out.push_str(&format!("  {reason}\n"));
        self.fail(code);
    }

    /// The fingerprint of a file. Dropbox gives it with the list. A local file
    /// is read for it, in parts, so a large file never sits whole in memory.
    fn print_of(&mut self, entry: &Entry, path: &str) -> String {
        if entry.folder {
            return "-".to_string();
        }
        if let Some(hash) = &entry.hash {
            return hash.clone();
        }
        let mut print = Fingerprinter::default();
        match self.storage.read_to(path, &mut print) {
            Ok(true) => print.finish(),
            // Gone since the list: nothing to fingerprint.
            Ok(false) => "-".to_string(),
            Err(e) => {
                self.fail(exit::FAILED);
                format!("- (could not read it: {e})")
            }
        }
    }

    fn line(&mut self, entry: &Entry, path: &str) {
        let print = if self.options.fingerprint {
            let print = self.print_of(entry, path);
            format!("{print:<64}  ")
        } else {
            String::new()
        };
        let size = entry.size.map_or("-".to_string(), |s| s.to_string());
        let time = entry
            .modified
            .map_or("-".to_string(), |t| time_text(t, &self.options.zone));
        let slash = if entry.folder { "/" } else { "" };
        let mark = if (self.limits)(named(path), true).is_err() {
            "  (read only)"
        } else {
            ""
        };
        self.out.push_str(&format!(
            "  {size:>9}  {time:<20}  {print}{}{slash}{mark}\n",
            entry.name
        ));
        self.left -= 1;
    }

    fn top(&mut self, shown: &str, path: &str) {
        if let Err(reason) = (self.limits)(named(path), false) {
            return self.problem(shown, exit::USAGE, &reason);
        }
        match self.storage.list(path, self.left) {
            Err(e) => self.problem(shown, exit::FAILED, &format!("could not list {shown}: {e}")),
            Ok(Listing::Missing) => self.problem(
                shown,
                exit::MISSING,
                &format!("there is no file or folder {shown}"),
            ),
            Ok(Listing::File(entry)) => {
                self.group(parent(shown));
                if self.left == 0 {
                    self.cut = true;
                } else {
                    self.line(&entry, path);
                }
            }
            Ok(Listing::Folder { entries, complete }) => {
                self.folder(shown, path, entries, complete, self.options.depth);
            }
        }
    }

    fn folder(
        &mut self,
        shown: &str,
        path: &str,
        entries: Vec<Entry>,
        complete: bool,
        depth: Option<usize>,
    ) {
        self.group(shown);
        let mut seen: Vec<Entry> = entries
            .into_iter()
            .filter(|e| self.options.all || !e.name.starts_with('.'))
            .filter(|e| (self.limits)(&join(path, &e.name), false).is_ok())
            .collect();
        seen.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.name.cmp(&b.name))
        });
        for entry in &seen {
            if self.left == 0 {
                self.cut = true;
                return;
            }
            self.line(entry, &join(path, &entry.name));
        }
        if !complete {
            self.cut = true;
            return;
        }
        if depth.is_some_and(|d| d <= 1) {
            return;
        }
        let deeper = depth.map(|d| d - 1);
        for entry in seen.iter().filter(|e| e.folder) {
            if self.cut {
                return;
            }
            if self.left == 0 {
                self.cut = true;
                return;
            }
            let (sub, sub_shown) = (join(path, &entry.name), join_shown(shown, &entry.name));
            match self.storage.list(&sub, self.left) {
                Ok(Listing::Folder { entries, complete }) => {
                    self.folder(&sub_shown, &sub, entries, complete, deeper);
                }
                // Gone or changed into a file since the line above: nothing to show.
                Ok(_) => {}
                Err(e) => self.problem(
                    &sub_shown,
                    exit::FAILED,
                    &format!("could not list {sub_shown}: {e}"),
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::LocalFolder;
    use crate::write::check_limits;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn place() -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("orgthread-list-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("notes/drafts/old")).unwrap();
        fs::create_dir_all(root.join("notes/.obsidian")).unwrap();
        fs::create_dir_all(root.join("tasks")).unwrap();
        fs::write(root.join("notes/a.md"), "1234").unwrap();
        fs::write(root.join("notes/B.md"), "12").unwrap();
        fs::write(root.join("notes/.DS_Store"), "x").unwrap();
        fs::write(root.join("notes/drafts/c.md"), "c").unwrap();
        fs::write(root.join("notes/drafts/old/d.md"), "d").unwrap();
        fs::write(root.join("tasks/today.md"), "t").unwrap();
        fs::canonicalize(root).unwrap()
    }

    fn options(depth: Option<usize>, all: bool) -> Options {
        Options {
            depth,
            all,
            fingerprint: false,
            zone: TimeZone::UTC,
        }
    }

    fn target(path: &str) -> Target {
        Target {
            shown: format!("/{path}"),
            path: Ok(path.to_string()),
        }
    }

    /// The text with each time put as `T`, so that a test does not hang on the clock.
    fn run(
        root: &PathBuf,
        targets: &[Target],
        o: &Options,
        deny: &str,
        read_only: &str,
    ) -> (String, i32) {
        let storage = LocalFolder::new(root, "");
        let limits = |p: &str, w: bool| check_limits(p, w, "", deny, read_only);
        let (text, code) = list(&storage, targets, o, &limits);
        let plain = text
            .lines()
            .map(|l| match l.find(" 20") {
                Some(i) if l[i + 1..].len() > 20 => format!("{}T{}", &l[..i + 1], &l[i + 21..]),
                _ => l.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n");
        (plain, code)
    }

    #[test]
    fn one_level_sorted_with_dot_names_hidden() {
        let root = place();
        let (text, code) = run(&root, &[target("notes")], &options(Some(1), false), "", "");
        assert_eq!(code, 0);
        assert_eq!(
            text,
            "/notes:\n          4  T  a.md\n          2  T  B.md\n          -  -                     drafts/"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fingerprints_come_before_the_names_of_files() {
        use crate::fingerprint::fingerprint;
        let root = place();
        let o = Options {
            fingerprint: true,
            ..options(Some(1), false)
        };
        let (text, code) = run(&root, &[target("notes")], &o, "", "");
        assert_eq!(code, 0);
        let (a, b) = (fingerprint(b"1234"), fingerprint(b"12"));
        let dash = format!("{:<64}", "-");
        assert_eq!(
            text,
            format!(
                "/notes:\n          4  T  {a}  a.md\n          2  T  {b}  B.md\n          -  -                     {dash}  drafts/"
            )
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_fingerprint_from_the_storage_needs_no_read() {
        struct Given;
        impl Storage for Given {
            fn list(&self, _: &str, _: usize) -> std::io::Result<Listing> {
                Ok(Listing::Folder {
                    entries: vec![Entry {
                        name: "a.md".into(),
                        folder: false,
                        size: Some(4),
                        modified: None,
                        hash: Some("given".into()),
                    }],
                    complete: true,
                })
            }
            fn read(&self, _: &str) -> std::io::Result<Option<Vec<u8>>> {
                panic!("a read, though the list gave the fingerprint")
            }
            fn write_if_unchanged(
                &self,
                _: &str,
                _: &crate::storage::Expect,
                _: &[u8],
            ) -> Result<(), crate::storage::WriteError> {
                unreachable!()
            }
            fn restore(&self, _: &str, _: Option<&[u8]>) -> std::io::Result<()> {
                unreachable!()
            }
            fn rescue(&self, _: &str, _: &[u8]) -> std::io::Result<String> {
                unreachable!()
            }
            fn lock_create(&self, _: &str, _: &str) -> std::io::Result<bool> {
                unreachable!()
            }
            fn lock_read(&self, _: &str) -> std::io::Result<Option<String>> {
                unreachable!()
            }
            fn lock_age(&self, _: &str) -> std::io::Result<Option<std::time::Duration>> {
                unreachable!()
            }
            fn lock_take_stale(
                &self,
                _: &str,
                _: std::time::Duration,
            ) -> std::io::Result<Option<String>> {
                unreachable!()
            }
            fn lock_remove(&self, _: &str) -> std::io::Result<()> {
                unreachable!()
            }
        }
        let o = Options {
            fingerprint: true,
            ..options(Some(1), false)
        };
        let open = |_: &str, _: bool| Ok(());
        let (text, code) = list(&Given, &[target("notes")], &o, &open);
        assert_eq!(code, 0);
        assert_eq!(
            text,
            format!(
                "/notes:\n          4  -                     {:<64}  a.md\n",
                "given"
            )
        );
    }

    #[test]
    fn all_shows_the_dot_names() {
        let root = place();
        let (text, _) = run(&root, &[target("notes")], &options(Some(1), true), "", "");
        assert!(text.contains("  .DS_Store"), "{text}");
        assert!(text.contains("  .obsidian/"), "{text}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_depth_gives_a_group_per_subfolder() {
        let root = place();
        let (two, _) = run(&root, &[target("notes")], &options(Some(2), false), "", "");
        assert!(
            two.contains("\n\n/notes/drafts:\n          1  T  c.md\n"),
            "{two}"
        );
        assert!(!two.contains("/notes/drafts/old:"), "{two}");
        assert!(!two.contains(".obsidian:"), "{two}");
        let (all, _) = run(&root, &[target("notes")], &options(None, false), "", "");
        assert!(
            all.ends_with("/notes/drafts/old:\n          1  T  d.md"),
            "{all}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn several_folders_and_a_missing_one_in_the_middle() {
        let root = place();
        let targets = [target("notes/drafts"), target("gone"), target("tasks")];
        let (text, code) = run(&root, &targets, &options(Some(1), false), "", "");
        assert_eq!(code, 6);
        assert!(
            text.contains("/gone:\n  there is no file or folder /gone\n\n/tasks:\n"),
            "{text}"
        );
        assert!(text.contains("today.md"), "{text}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_first_failure_gives_the_exit_code() {
        let root = place();
        let outside = Target {
            shown: "/elsewhere".into(),
            path: Err((2, "/elsewhere is outside the folder".into())),
        };
        let targets = [outside, target("gone")];
        let (text, code) = run(&root, &targets, &options(Some(1), false), "", "");
        assert_eq!(code, 2);
        assert!(
            text.starts_with("/elsewhere:\n  /elsewhere is outside the folder\n"),
            "{text}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn closed_paths_stay_out_and_read_only_ones_get_a_mark() {
        let root = place();
        let (text, code) = run(
            &root,
            &[target("notes")],
            &options(None, true),
            "notes/drafts",
            "notes/b.md",
        );
        assert_eq!(code, 0);
        assert!(!text.contains("drafts"), "{text}");
        assert!(text.contains("  B.md  (read only)"), "{text}");
        assert!(text.contains("  a.md\n"), "{text}");
        let (text, code) = run(
            &root,
            &[target("notes/drafts")],
            &options(Some(1), false),
            "notes/drafts",
            "",
        );
        assert_eq!(code, 2);
        assert!(text.contains("closed"), "{text}");
        assert!(!text.contains("c.md"), "{text}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_file_shows_under_its_folder() {
        let root = place();
        let (text, code) = run(
            &root,
            &[target("notes/a.md")],
            &options(Some(1), false),
            "",
            "",
        );
        assert_eq!(code, 0);
        assert_eq!(text, "/notes:\n          4  T  a.md");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_root_itself_can_be_listed() {
        let root = place();
        let t = Target {
            shown: "/".into(),
            path: Ok(String::new()),
        };
        let (text, code) = run(&root, &[t], &options(Some(1), false), "", "");
        assert_eq!(code, 0);
        assert_eq!(
            text,
            "/:\n          -  -                     notes/\n          -  -                     tasks/"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_limit_cuts_the_list_and_says_so() {
        let root = place();
        fs::create_dir_all(root.join("many")).unwrap();
        for i in 0..LIMIT + 5 {
            fs::write(root.join(format!("many/{i:04}.md")), "").unwrap();
        }
        let (text, code) = run(
            &root,
            &[target("many"), target("tasks")],
            &options(Some(1), false),
            "",
            "",
        );
        assert_eq!(code, 0);
        assert_eq!(
            text.lines().count(),
            1 + LIMIT + 1,
            "a header, the entries, the last line"
        );
        assert!(text.ends_with(MORE.trim_end()), "{text}");
        assert!(!text.contains("/tasks:"), "{text}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exactly_the_limit_is_no_cut() {
        let root = place();
        fs::create_dir_all(root.join("many")).unwrap();
        for i in 0..LIMIT {
            fs::write(root.join(format!("many/{i:04}.md")), "").unwrap();
        }
        let (text, _) = run(&root, &[target("many")], &options(Some(1), false), "", "");
        assert!(!text.contains("more entries"), "{text}");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_link_out_of_the_root_stays_out() {
        let root = place();
        let outside = root.with_extension("outside");
        fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("tasks/away")).unwrap();
        std::os::unix::fs::symlink(root.join("notes/a.md"), root.join("tasks/near.md")).unwrap();
        let (text, _) = run(&root, &[target("tasks")], &options(Some(1), false), "", "");
        assert!(!text.contains("away"), "{text}");
        assert!(text.contains("near.md"), "{text}");
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn a_time_is_in_the_given_zone_with_the_day_name() {
        let zone = TimeZone::get("Europe/Vienna").unwrap();
        assert_eq!(time_text(1_794_045_600, &zone), "2026-11-07 Sat 11:00");
        assert_eq!(
            time_text(1_794_045_600, &TimeZone::UTC),
            "2026-11-07 Sat 10:00"
        );
    }

    #[test]
    fn a_parent_keeps_the_header_and_the_name_a_path() {
        assert_eq!(parent("/org/notes/a.md"), "/org/notes");
        assert_eq!(parent("/a.md"), "/");
        assert_eq!(parent("a.md"), ".");
        assert_eq!(join_shown("/", "org"), "/org");
        assert_eq!(join_shown("/org", "notes"), "/org/notes");
    }
}
