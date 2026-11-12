//! The answer of `orgthread search`: the lines of the files in one folder that
//! hold a text, or with `--names`, the paths whose name holds it.
//!
//! A content hit looks like `grep -n`, `path:line: text`, so that a model reads
//! it with no help. A name hit is the path alone. The text matches as plain text,
//! with case ignored. The search text and the hits go only to the caller: never
//! to the log, to a message of the log, or to a file, as a search text such as
//! "blood pressure" can be health data.

use crate::list::{Limits, join, join_shown, named};
use crate::storage::{Storage, Walked};
use crate::transfer::named_binary;
use crate::write::{exit, is_under};
use std::io::ErrorKind;

/// The hits of one answer, unless the caller asks for another number.
pub const MAX: usize = 100;
/// The most hits one answer may ask for.
pub const MOST: usize = 1000;
/// A hit line is cut after this many characters, with `…` at the end.
pub const CUT: usize = 300;
/// The setting of the largest zip of a search on a server, in MB.
pub use crate::settings::SEARCH_MB;

/// The largest zip of a content search on a server, in bytes: 25 MB unless the
/// settings say otherwise. A local folder skips a single file above it.
pub fn zip_limit() -> Result<u64, String> {
    crate::settings::megabytes(SEARCH_MB)
}

/// What to look for, and how.
pub struct Query {
    /// The text, on one line. Case does not count.
    pub text: String,
    /// Only the names of files and folders, not the content.
    pub names: bool,
    /// The most hits to show, from 1 to `MOST`.
    pub max: usize,
    /// Also the names that start with a dot, and all they hold.
    pub all: bool,
}

/// A search that ran: the text of the answer, and the number of files with a
/// hit, for the size field of the log. The log never gets the text.
#[derive(Debug)]
pub struct Found {
    pub text: String,
    pub files: usize,
}

/// One hit: a path from the searched folder, and for content the number and the
/// text of the line.
struct Hit {
    path: String,
    folder: bool,
    line: Option<(usize, String)>,
}

/// `Some` with the reason when `folder` holds a closed path of `deny`. A content
/// search on a server fetches the whole folder as one zip, so the closed files
/// would come into memory: such a search is refused. `folder` is from the root,
/// and empty for the root itself.
pub fn holds_closed(folder: &str, deny: &str) -> Option<String> {
    let folder = folder.trim_matches('/');
    deny.split(',')
        .map(str::trim)
        .filter(|rule| !rule.trim_matches('/').is_empty())
        .find(|rule| folder.is_empty() || (is_under(rule, folder) && !is_under(folder, rule)))
        .map(|rule| {
            format!(
                "/{folder} holds the closed path {rule}. A search of the content on a server fetches the whole folder, so it is refused. Search a folder inside it that holds no closed path, or search with names."
            )
        })
}

/// A line cut after `CUT` characters, with `…` at the end.
fn cut(line: &str) -> String {
    match line.char_indices().nth(CUT) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line.to_string(),
    }
}

/// Text that a search reads: valid UTF-8 with no NUL sign, as for a write.
fn as_text(bytes: &[u8]) -> Option<&str> {
    if bytes.contains(&0) {
        return None;
    }
    std::str::from_utf8(bytes).ok()
}

/// The search of the folder `path`, from the root and empty for the root.
/// `shown` is the folder as given: it starts each path of the answer. `limit` is
/// the most bytes the storage may hold for it. The error is an exit code and
/// the reason, and never holds the search text.
pub fn search(
    storage: &dyn Storage,
    shown: &str,
    path: &str,
    query: &Query,
    limits: Limits,
    limit: u64,
) -> Result<Found, (i32, String)> {
    if let Err(reason) = limits(named(path), false) {
        return Err((exit::USAGE, reason));
    }
    let keep = |inside: &str, folder: bool| {
        if !query.all && inside.split('/').any(|part| part.starts_with('.')) {
            return false;
        }
        // A picture or a PDF is never text: a content search need not read it.
        if !query.names && !folder && named_binary(inside) {
            return false;
        }
        limits(&join(path, inside), false).is_ok()
    };
    let text = query.text.to_lowercase();
    let mut hits: Vec<Hit> = Vec::new();
    let mut files = 0;
    let walked = if query.names {
        storage.names(path, &keep, &mut |inside, folder| {
            let name = inside.rsplit('/').next().unwrap_or(inside);
            if name.to_lowercase().contains(&text) {
                files += 1;
                hits.push(Hit {
                    path: inside.to_string(),
                    folder,
                    line: None,
                });
            }
        })
    } else {
        storage.texts(path, limit, &keep, &mut |inside, bytes| {
            let Some(content) = as_text(bytes) else {
                return;
            };
            let before = hits.len();
            for (i, line) in content.lines().enumerate() {
                if line.to_lowercase().contains(&text) {
                    hits.push(Hit {
                        path: inside.to_string(),
                        folder: false,
                        line: Some((i + 1, cut(line))),
                    });
                }
            }
            if hits.len() > before {
                files += 1;
            }
        })
    };
    match walked {
        Ok(Walked::Done) => {}
        Ok(Walked::Missing) => {
            return Err((exit::MISSING, format!("there is no file or folder {shown}")));
        }
        Ok(Walked::NotFolder) => {
            return Err((
                exit::USAGE,
                format!("{shown} is a file, and search needs a folder"),
            ));
        }
        Err(e) if e.kind() == ErrorKind::FileTooLarge => {
            return Err((
                exit::USAGE,
                format!(
                    "{shown} is too large to search: {e}. Search a smaller folder, or raise {SEARCH_MB}."
                ),
            ));
        }
        Err(e) => return Err((exit::FAILED, format!("could not search {shown}: {e}"))),
    }
    // One order on every door. The sort is stable, so the lines of a file stay in order.
    hits.sort_by(|a, b| {
        a.path
            .to_lowercase()
            .cmp(&b.path.to_lowercase())
            .then_with(|| a.path.cmp(&b.path))
    });
    if hits.is_empty() {
        return Ok(Found {
            text: format!("no hits in {shown}\n"),
            files,
        });
    }
    let mut out = String::new();
    for hit in hits.iter().take(query.max) {
        let full = join_shown(shown, &hit.path);
        let mark = if limits(&join(path, &hit.path), true).is_err() {
            "  (read only)"
        } else {
            ""
        };
        match &hit.line {
            Some((n, line)) => out.push_str(&format!("{full}:{n}: {line}{mark}\n")),
            None => {
                let slash = if hit.folder { "/" } else { "" };
                out.push_str(&format!("{full}{slash}{mark}\n"));
            }
        }
    }
    if hits.len() > query.max {
        let left = hits.len() - query.max;
        let word = if left == 1 { "hit" } else { "hits" };
        out.push_str(&format!(
            "{left} more {word} not shown: search a smaller folder or a longer text\n"
        ));
    }
    Ok(Found { text: out, files })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{LocalFolder, kept};
    use crate::write::check_limits;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Invented files in a new test folder.
    fn place() -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let root =
            std::env::temp_dir().join(format!("orgthread-search-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("notes/health")).unwrap();
        fs::create_dir_all(root.join("notes/.obsidian")).unwrap();
        fs::create_dir_all(root.join("notes/secret")).unwrap();
        fs::write(
            root.join("notes/health/log.md"),
            "# Log\nBlood Pressure morning: 120/80\nslept well\nblood pressure evening: 118/79\n",
        )
        .unwrap();
        fs::write(
            root.join("notes/plan.org"),
            "* TODO call the garden centre\n",
        )
        .unwrap();
        fs::write(root.join("notes/.obsidian/app.json"), "blood pressure\n").unwrap();
        fs::write(root.join("notes/secret/a.md"), "blood pressure\n").unwrap();
        root
    }

    fn query(text: &str) -> Query {
        Query {
            text: text.to_string(),
            names: false,
            max: MAX,
            all: false,
        }
    }

    fn run_with(
        root: &PathBuf,
        q: &Query,
        deny: &str,
        read_only: &str,
        limit: u64,
    ) -> Result<Found, (i32, String)> {
        let storage = LocalFolder::new(root, "");
        let limits = |p: &str, w: bool| check_limits(p, w, "", deny, read_only);
        search(&storage, "/notes", "notes", q, &limits, limit)
    }

    fn run(root: &PathBuf, q: &Query) -> String {
        run_with(root, q, "", "", 1_000_000).unwrap().text
    }

    #[test]
    fn case_does_not_count_and_a_hit_is_path_line_text() {
        let root = place();
        let found = run_with(&root, &query("BLOOD pressure"), "", "", 1_000_000).unwrap();
        assert_eq!(
            found.text,
            "/notes/health/log.md:2: Blood Pressure morning: 120/80\n\
             /notes/health/log.md:4: blood pressure evening: 118/79\n\
             /notes/secret/a.md:1: blood pressure\n"
        );
        assert_eq!(found.files, 2, "two files hold a hit");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn names_match_only_the_name_of_files_and_folders() {
        let root = place();
        fs::write(root.join("notes/Health-plan.md"), "nothing here\n").unwrap();
        let mut q = query("HEALTH");
        q.names = true;
        assert_eq!(run(&root, &q), "/notes/health/\n/notes/Health-plan.md\n");
        // The content of log.md does not hold "health", and its name does not either.
        assert_eq!(run(&root, &query("log.md")), "no hits in /notes\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn max_cuts_the_answer_and_the_last_line_says_how_many_are_left_out() {
        let root = place();
        let mut q = query("blood");
        q.max = 1;
        assert_eq!(
            run(&root, &q),
            "/notes/health/log.md:2: Blood Pressure morning: 120/80\n\
             2 more hits not shown: search a smaller folder or a longer text\n"
        );
        q.max = 2;
        assert!(
            run(&root, &q)
                .ends_with("1 more hit not shown: search a smaller folder or a longer text\n")
        );
        q.max = 3;
        assert!(!run(&root, &q).contains("not shown"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_long_line_is_cut_at_300_characters() {
        let root = place();
        let long = format!("{}needle{}", "ä".repeat(298), "b".repeat(100));
        fs::write(root.join("notes/long.md"), format!("{long}\n")).unwrap();
        let text = run(&root, &query("needle"));
        let hit = text.strip_prefix("/notes/long.md:1: ").unwrap().trim_end();
        assert_eq!(hit.chars().count(), 301, "300 characters and the sign …");
        assert_eq!(hit, format!("{}ne…", "ä".repeat(298)));
        // A line of exactly 300 characters stays whole.
        assert_eq!(cut(&"x".repeat(300)), "x".repeat(300));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_closed_path_stays_out() {
        let root = place();
        let text = run_with(&root, &query("blood"), "notes/secret", "", 1_000_000)
            .unwrap()
            .text;
        assert!(!text.contains("secret"), "{text}");
        assert!(text.contains("/notes/health/log.md:2:"), "{text}");
        let mut q = query("secret");
        q.names = true;
        let text = run_with(&root, &q, "/notes/secret", "", 1_000_000)
            .unwrap()
            .text;
        assert_eq!(text, "no hits in /notes\n");
        // The folder itself closed: refused, code 2.
        let (code, _) = run_with(&root, &query("blood"), "notes", "", 1_000_000).unwrap_err();
        assert_eq!(code, exit::USAGE);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_hit_in_a_read_only_path_is_marked() {
        let root = place();
        let text = run_with(&root, &query("evening"), "", "notes/health", 1_000_000)
            .unwrap()
            .text;
        assert_eq!(
            text,
            "/notes/health/log.md:4: blood pressure evening: 118/79  (read only)\n"
        );
        let mut q = query("log");
        q.names = true;
        let text = run_with(&root, &q, "", "notes/health", 1_000_000)
            .unwrap()
            .text;
        assert_eq!(text, "/notes/health/log.md  (read only)\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dot_names_are_skipped_unless_all() {
        let root = place();
        assert!(!run(&root, &query("blood")).contains(".obsidian"));
        let mut q = query("blood");
        q.all = true;
        assert!(run(&root, &q).contains("/notes/.obsidian/app.json:1: blood pressure\n"));
        let mut q = query("app");
        q.names = true;
        assert_eq!(run(&root, &q), "no hits in /notes\n");
        q.all = true;
        assert_eq!(run(&root, &q), "/notes/.obsidian/app.json\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_file_that_is_not_text_is_skipped_for_content_but_found_by_name() {
        let root = place();
        fs::write(root.join("notes/scan.bin"), b"needle\0\x01").unwrap();
        fs::write(root.join("notes/broken.md"), b"needle \xff\xfe").unwrap();
        fs::write(root.join("notes/photo.png"), b"needle").unwrap();
        assert_eq!(run(&root, &query("needle")), "no hits in /notes\n");
        let mut q = query("photo");
        q.names = true;
        assert_eq!(run(&root, &q), "/notes/photo.png\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_local_file_above_the_limit_is_skipped() {
        let root = place();
        let found = run_with(&root, &query("garden"), "", "", 10).unwrap();
        assert_eq!(found.text, "no hits in /notes\n");
        let found = run_with(&root, &query("garden"), "", "", 1000).unwrap();
        assert_eq!(
            found.text,
            "/notes/plan.org:1: * TODO call the garden centre\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn no_hits_is_a_plain_answer_with_no_files() {
        let root = place();
        let found = run_with(&root, &query("nowhere at all"), "", "", 1_000_000).unwrap();
        assert_eq!(found.text, "no hits in /notes\n");
        assert_eq!(found.files, 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_missing_folder_is_6_and_a_file_is_2() {
        let root = place();
        let storage = LocalFolder::new(&root, "");
        let limits = |p: &str, w: bool| check_limits(p, w, "", "", "");
        let q = query("x");
        let missing = search(&storage, "/gone", "gone", &q, &limits, 100).unwrap_err();
        assert_eq!(missing.0, exit::MISSING);
        let file = search(
            &storage,
            "/notes/plan.org",
            "notes/plan.org",
            &q,
            &limits,
            100,
        );
        assert_eq!(file.unwrap_err().0, exit::USAGE);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_folder_that_holds_a_closed_path_is_named() {
        let deny = "/Documents, /org/health/private";
        assert!(holds_closed("", deny).is_some(), "the root holds all");
        assert!(
            holds_closed("org", deny)
                .unwrap()
                .contains("/org/health/private")
        );
        assert!(holds_closed("org/health", deny).is_some());
        assert!(
            holds_closed("ORG/Health", deny).is_some(),
            "case does not count"
        );
        assert!(holds_closed("org/notes", deny).is_none());
        assert!(holds_closed("org/healthy", deny).is_none());
        assert!(holds_closed("", "").is_none());
        assert!(holds_closed("", " , ").is_none());
    }

    #[test]
    fn kept_asks_each_folder_around_a_path() {
        let keep = |p: &str, folder: bool| !(folder && p == "a/.b");
        assert!(kept("a/c/d.md", false, &keep));
        assert!(!kept("a/.b/d.md", false, &keep));
        assert!(!kept("a/.b", true, &keep));
        assert!(kept("d.md", false, &keep));
    }
}
