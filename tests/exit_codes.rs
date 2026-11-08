//! Every exit code of the program has its row in docs/exit-codes.md, and the
//! table has no row for a code that the program does not give.
//!
//! The codes live in one place: the module `exit` in src/write.rs. This test
//! reads that module from the source, so that a new code there fails here until
//! the docs name it.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

fn file(path: &str) -> String {
    let full = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
    fs::read_to_string(&full).unwrap_or_else(|e| panic!("could not read {path}: {e}"))
}

/// The values of `pub const NAME: i32 = N;` in `pub mod exit { ... }`.
fn codes_in_the_code() -> BTreeSet<i32> {
    let source = file("src/write.rs");
    let start = source
        .find("pub mod exit {")
        .expect("src/write.rs has no module exit");
    let body = &source[start..];
    let body = &body[..body.find('}').expect("the module exit has no end")];
    let mut codes = BTreeSet::new();
    for line in body.lines().skip(1).map(str::trim) {
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let value = line
            .strip_prefix("pub const ")
            .and_then(|rest| rest.split_once(": i32 = "))
            .and_then(|(_, value)| value.strip_suffix(';'))
            .unwrap_or_else(|| {
                panic!("a line of the module exit that this test cannot read: {line}")
            });
        let value: i32 = value
            .parse()
            .unwrap_or_else(|_| panic!("not a number in the module exit: {line}"));
        assert!(
            codes.insert(value),
            "the code {value} is in the module exit twice"
        );
    }
    codes
}

/// The first cells of the table under `## The codes`.
fn codes_in_the_docs() -> BTreeSet<i32> {
    let docs = file("docs/exit-codes.md");
    let start = docs
        .find("## The codes")
        .expect("docs/exit-codes.md has no part ## The codes");
    let part = &docs[start + 2..];
    let part = &part[..part.find("\n## ").unwrap_or(part.len())];
    part.lines()
        .filter_map(|line| line.strip_prefix("| "))
        .filter_map(|line| line.split(" |").next())
        .filter_map(|cell| cell.trim().parse().ok())
        .collect()
}

#[test]
fn each_exit_code_has_its_row_in_the_docs() {
    let code = codes_in_the_code();
    assert!(
        code.contains(&0) && code.len() > 10,
        "too few codes found: {code:?}"
    );
    let docs = codes_in_the_docs();
    let missing: Vec<_> = code.difference(&docs).collect();
    assert!(
        missing.is_empty(),
        "docs/exit-codes.md has no row for {missing:?}"
    );
    let extra: Vec<_> = docs.difference(&code).collect();
    assert!(
        extra.is_empty(),
        "docs/exit-codes.md names codes the program does not give: {extra:?}"
    );
}

/// A code written as a bare number elsewhere would escape the check above.
#[test]
fn no_exit_code_is_written_as_a_bare_number() {
    let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for entry in fs::read_dir(&folder).expect("could not read src") {
        let path = entry.expect("could not read src").path();
        let source = fs::read_to_string(&path).expect("could not read a source file");
        for (n, line) in source.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            let bare = line.match_indices("exit code ").any(|(i, found)| {
                line[i + found.len()..].starts_with(|c: char| c.is_ascii_digit())
            });
            assert!(
                !bare,
                "{}:{}: write the code with the module exit, not as a number",
                path.display(),
                n + 1
            );
        }
    }
}
