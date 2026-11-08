//! A patch: exact old text replaced by new text, plus lines added at the end.
//!
//! The exact text is the check. An edit that does not fit is refused, and then
//! nothing of the patch is written. The steps here only build the new text; the
//! write procedure writes it as it writes any other text.

use serde_json::Value;

/// One change: `old` becomes `new`. `line` picks between equal matches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub old: String,
    pub new: String,
    pub line: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Patch {
    pub edits: Vec<Edit>,
    pub append: Option<String>,
}

/// The new text, and notes for the answer.
#[derive(Debug, PartialEq, Eq)]
pub struct Applied {
    pub text: String,
    pub notes: Vec<String>,
}

impl Patch {
    /// Reads the JSON of a patch. A refusal says what is wrong, as wrong use.
    pub fn parse(bytes: &[u8]) -> Result<Patch, String> {
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|e| format!("the patch is not valid JSON: {e}"))?;
        let top = value
            .as_object()
            .ok_or("the patch must be one JSON object, with edits, append or both")?;
        for key in top.keys() {
            if key != "edits" && key != "append" {
                return Err(format!(
                    "the patch has an unknown field {key}. Its fields are edits and append"
                ));
            }
        }
        let mut edits = Vec::new();
        match top.get("edits") {
            None | Some(Value::Null) => {}
            Some(Value::Array(list)) => {
                for (i, item) in list.iter().enumerate() {
                    edits.push(Edit::parse(i + 1, item)?);
                }
            }
            Some(_) => return Err("edits must be a list of {old, new}".into()),
        }
        let append = match top.get("append") {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) if text.is_empty() => {
                return Err("append is empty. Leave it out, or give the lines to add".into());
            }
            Some(Value::String(text)) => Some(text.clone()),
            Some(_) => return Err("append must be text".into()),
        };
        if edits.is_empty() && append.is_none() {
            return Err("the patch needs at least one edit or an append".into());
        }
        Ok(Patch { edits, append })
    }
}

impl Edit {
    fn parse(n: usize, item: &Value) -> Result<Edit, String> {
        let fields = item
            .as_object()
            .ok_or_else(|| format!("edit {n} must be an object with old and new"))?;
        for key in fields.keys() {
            if key != "old" && key != "new" && key != "line" {
                return Err(format!(
                    "edit {n} has an unknown field {key}. Its fields are old, new and line"
                ));
            }
        }
        let text = |key: &str| -> Result<String, String> {
            fields
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("edit {n} needs {key}, as text"))
        };
        let old = text("old")?;
        if old.is_empty() {
            return Err(format!(
                "the old text of edit {n} is empty. To add lines at the end, use append"
            ));
        }
        let new = text("new")?;
        let line = match fields.get("line") {
            None | Some(Value::Null) => None,
            Some(v) => match v.as_u64() {
                Some(l) if l >= 1 => Some(l as usize),
                _ => return Err(format!("the line of edit {n} must be a number from 1")),
            },
        };
        Ok(Edit { old, new, line })
    }
}

/// The byte offset where each line starts.
fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' && i + 1 < text.len() {
            starts.push(i + 1);
        }
    }
    starts
}

/// The line, from 1, that holds the byte at `offset`.
fn line_of(starts: &[usize], offset: usize) -> usize {
    match starts.binary_search(&offset) {
        Ok(i) => i + 1,
        Err(i) => i,
    }
}

/// Where `part` occurs as whole lines: it starts at the start of a line, and it
/// ends after a newline or at the end of the text.
fn whole_line_matches(text: &str, starts: &[usize], part: &str) -> Vec<usize> {
    starts
        .iter()
        .copied()
        .filter(|&s| {
            let end = s + part.len();
            text[s..].starts_with(part) && (end == text.len() || text.as_bytes()[end - 1] == b'\n')
        })
        .collect()
}

/// "4", "4 and 88", "4, 88 and 140".
fn lines_said(lines: &[usize]) -> String {
    let words: Vec<String> = lines.iter().map(usize::to_string).collect();
    match words.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

/// Where the file already holds the new text around the chosen match of the
/// old text: there the edit has been applied already, by an earlier try.
fn done_at(text: &str, starts: &[usize], edit: &Edit, start: usize) -> Option<usize> {
    if edit.new.is_empty() || !edit.new.contains(edit.old.as_str()) {
        return None;
    }
    let inner = line_starts(&edit.new);
    let offsets = whole_line_matches(&edit.new, &inner, &edit.old);
    whole_line_matches(text, starts, &edit.new)
        .into_iter()
        .find(|&p| offsets.iter().any(|&k| p + k == start))
}

/// Builds the new text. A refusal names the edit and the lines.
pub fn apply(old: &str, patch: &Patch) -> Result<Applied, String> {
    let starts = line_starts(old);
    let mut notes = Vec::new();
    // Where each edit goes: its start, its end, and its number.
    let mut places: Vec<(usize, usize, usize)> = Vec::new();
    for (i, edit) in patch.edits.iter().enumerate() {
        let n = i + 1;
        if edit.old.ends_with('\n') && !edit.new.is_empty() && !edit.new.ends_with('\n') {
            return Err(format!(
                "Edit {n}: the old text ends with a newline, so the new text must end with one too, or be empty. Otherwise two lines join."
            ));
        }
        let matches = whole_line_matches(old, &starts, &edit.old);
        let lines: Vec<usize> = matches.iter().map(|&s| line_of(&starts, s)).collect();
        let start = match (matches.as_slice(), edit.line) {
            ([], _) => {
                let mut said = format!("Edit {n}: the old text is not in the file.");
                let found = if edit.new.is_empty() {
                    Vec::new()
                } else {
                    whole_line_matches(old, &starts, &edit.new)
                };
                let with_newline = format!("{}\n", edit.old);
                if !edit.old.ends_with('\n')
                    && !whole_line_matches(old, &starts, &with_newline).is_empty()
                {
                    said = format!(
                        "Edit {n}: the old text has no newline at its end. Old and new are whole lines with their newlines: add \\n at the end of both."
                    );
                } else if let Some(&p) = found.first() {
                    said.push_str(&format!(
                        " Its new text is at line {}, so this patch may have gone through already.",
                        line_of(&starts, p)
                    ));
                } else {
                    said.push_str(" Read the file again, or check the old text sign by sign.");
                }
                return Err(said);
            }
            ([one], wanted) => {
                if let Some(wanted) = wanted
                    && wanted != lines[0]
                {
                    notes.push(format!("edit {n} found at line {}, not {wanted}", lines[0]));
                }
                *one
            }
            (_, None) => {
                return Err(format!(
                    "Edit {n}: the old text occurs {} times, at lines {}. Add a line before or after it so that it occurs once, or give the line of the one you mean.",
                    matches.len(),
                    lines_said(&lines)
                ));
            }
            (_, Some(wanted)) => match lines.iter().position(|&l| l == wanted) {
                Some(k) => matches[k],
                None => {
                    return Err(format!(
                        "Edit {n}: the old text occurs {} times, at lines {}, but not at line {wanted}. Give one of these lines.",
                        matches.len(),
                        lines_said(&lines)
                    ));
                }
            },
        };
        if let Some(p) = done_at(old, &starts, edit, start) {
            return Err(format!(
                "Edit {n}: the file already holds its new text at line {}, so this patch may have gone through already.",
                line_of(&starts, p)
            ));
        }
        places.push((start, start + edit.old.len(), n));
    }

    places.sort();
    for pair in places.windows(2) {
        let ((_, end, a), (start, _, b)) = (pair[0], pair[1]);
        if start < end {
            let (a, b) = (a.min(b), a.max(b));
            return Err(format!(
                "Edits {a} and {b} touch the same lines. Join them into one edit."
            ));
        }
    }

    let mut text = String::with_capacity(old.len());
    let mut at = 0;
    for &(start, end, n) in &places {
        text.push_str(&old[at..start]);
        text.push_str(&patch.edits[n - 1].new);
        at = end;
    }
    text.push_str(&old[at..]);

    if let Some(append) = &patch.append {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(append);
        if !append.ends_with('\n') {
            text.push('\n');
        }
    }
    Ok(Applied { text, notes })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "\
#+title: Example notes
* Contents
- Build
- Release
* Build
Run the tests:
  make test
Then build:
  make build
* Release
Run the tests:
  make test
Then tag the build:
  git tag v1.0
Done.
";

    fn edit(old: &str, new: &str) -> Edit {
        Edit {
            old: old.into(),
            new: new.into(),
            line: None,
        }
    }

    fn patch(edits: Vec<Edit>) -> Patch {
        Patch {
            edits,
            append: None,
        }
    }

    #[test]
    fn a_missing_last_newline_gets_a_hint() {
        let e = apply(FILE, &patch(vec![edit("Done.", "All done.")])).unwrap_err();
        assert!(e.contains("has no newline at its end"), "{e}");
        assert!(e.contains("add \\n at the end of both"), "{e}");
        // A text that is not there at all keeps the old words.
        let e = apply(FILE, &patch(vec![edit("Gone.", "x")])).unwrap_err();
        assert!(e.contains("check the old text sign by sign"), "{e}");
    }

    #[test]
    fn one_match_is_replaced() {
        let p = patch(vec![edit("Done.\n", "All done.\n")]);
        let a = apply(FILE, &p).unwrap();
        assert!(a.text.ends_with("  git tag v1.0\nAll done.\n"));
        assert!(a.notes.is_empty());
    }

    #[test]
    fn no_match_is_refused() {
        let e = apply(FILE, &patch(vec![edit("Gone.\n", "x\n")])).unwrap_err();
        assert!(
            e.starts_with("Edit 1: the old text is not in the file."),
            "{e}"
        );
    }

    #[test]
    fn a_repeated_line_is_refused_with_its_lines() {
        let e = apply(
            FILE,
            &patch(vec![edit("  make test\n", "  make test-all\n")]),
        )
        .unwrap_err();
        assert!(e.contains("occurs 2 times, at lines 7 and 12"), "{e}");
    }

    #[test]
    fn a_repeated_neighbour_is_still_refused() {
        let p = patch(vec![edit(
            "Run the tests:\n  make test\n",
            "Run the tests:\n  make test-all\n",
        )]);
        let e = apply(FILE, &p).unwrap_err();
        assert!(e.contains("at lines 6 and 11"), "{e}");
    }

    #[test]
    fn a_line_after_that_differs_makes_it_unique() {
        let p = patch(vec![edit(
            "  make test\nThen tag the build:\n",
            "  make test-all\nThen tag the build:\n",
        )]);
        let a = apply(FILE, &p).unwrap();
        assert!(a.text.contains("Run the tests:\n  make test\nThen build:"));
        assert!(a.text.contains("  make test-all\nThen tag the build:"));
    }

    #[test]
    fn a_line_picks_between_equal_matches() {
        let mut e = edit("  make test\n", "  make test-all\n");
        e.line = Some(12);
        let a = apply(FILE, &patch(vec![e])).unwrap();
        assert!(a.text.contains("Then build:"));
        assert_eq!(a.text.matches("  make test\n").count(), 1);
        assert!(a.text.contains("  make test-all\nThen tag the build:"));
    }

    #[test]
    fn a_line_at_no_match_is_refused() {
        let mut e = edit("  make test\n", "  make test-all\n");
        e.line = Some(13);
        let err = apply(FILE, &patch(vec![e])).unwrap_err();
        assert!(
            err.contains("at lines 7 and 12, but not at line 13"),
            "{err}"
        );
    }

    #[test]
    fn a_line_that_differs_for_one_match_is_only_a_note() {
        let mut e = edit("Done.\n", "All done.\n");
        e.line = Some(3);
        let a = apply(FILE, &patch(vec![e])).unwrap();
        assert_eq!(a.notes, vec!["edit 1 found at line 15, not 3".to_string()]);
    }

    #[test]
    fn a_match_counts_only_at_whole_lines() {
        let text = "  make test-all\n";
        let e = apply(text, &patch(vec![edit("  make test", "x")])).unwrap_err();
        assert!(e.contains("not in the file"), "{e}");
        let e = apply(text, &patch(vec![edit("make test-all\n", "x\n")])).unwrap_err();
        assert!(e.contains("not in the file"), "{e}");
    }

    #[test]
    fn the_last_line_without_a_newline_matches() {
        let a = apply("a\nb", &patch(vec![edit("b", "c")])).unwrap();
        assert_eq!(a.text, "a\nc");
    }

    #[test]
    fn a_lost_newline_is_refused() {
        let e = apply(FILE, &patch(vec![edit("Done.\n", "Done.")])).unwrap_err();
        assert!(e.contains("must end with one too"), "{e}");
    }

    #[test]
    fn a_delete_and_an_insert() {
        let p = patch(vec![
            edit("- Release\n", ""),
            edit("Done.\n", "Done.\nThe end.\n"),
        ]);
        let a = apply(FILE, &p).unwrap();
        assert!(a.text.contains("- Build\n* Build\n"));
        assert!(a.text.ends_with("Done.\nThe end.\n"));
    }

    #[test]
    fn overlapping_edits_are_refused() {
        let p = patch(vec![
            edit("Then build:\n  make build\n", "x\n"),
            edit("  make build\n* Release\n", "y\n"),
        ]);
        let e = apply(FILE, &p).unwrap_err();
        assert!(e.contains("Edits 1 and 2 touch the same lines"), "{e}");
    }

    #[test]
    fn edits_next_to_each_other_are_fine() {
        let p = patch(vec![
            edit("Then build:\n", "Then make:\n"),
            edit("  make build\n", "  make all\n"),
        ]);
        let a = apply(FILE, &p).unwrap();
        assert!(a.text.contains("Then make:\n  make all\n"));
    }

    #[test]
    fn one_misfit_writes_none_of_the_edits() {
        let p = patch(vec![edit("Done.\n", "All done.\n"), edit("Gone.\n", "x\n")]);
        let e = apply(FILE, &p).unwrap_err();
        assert!(e.starts_with("Edit 2:"), "{e}");
    }

    #[test]
    fn the_append_comes_after_the_edits_and_ends_the_line() {
        let csv = "date,place,km,notes\n2026-11-07,park,3.8,\n";
        let p = Patch {
            edits: vec![edit("2026-11-07,park,3.8,\n", "2026-11-07,park,4.1,\n")],
            append: Some("2026-11-08,forest,7.5,".into()),
        };
        let a = apply(csv, &p).unwrap();
        assert_eq!(
            a.text,
            "date,place,km,notes\n2026-11-07,park,4.1,\n2026-11-08,forest,7.5,\n"
        );
    }

    #[test]
    fn the_append_adds_a_missing_newline_before_it() {
        let p = Patch {
            edits: vec![],
            append: Some("b\n".into()),
        };
        assert_eq!(apply("a", &p).unwrap().text, "a\nb\n");
        assert_eq!(apply("", &p).unwrap().text, "b\n");
    }

    #[test]
    fn quotes_and_commas_stay_as_they_are() {
        let csv = "date,notes\n2026-11-06,\"windy, cold\"\n";
        let p = patch(vec![edit(
            "2026-11-06,\"windy, cold\"\n",
            "2026-11-06,\"windy, cold, 8 °C\"\n",
        )]);
        let a = apply(csv, &p).unwrap();
        assert_eq!(a.text, "date,notes\n2026-11-06,\"windy, cold, 8 °C\"\n");
    }

    #[test]
    fn the_same_patch_twice_is_refused_the_second_time() {
        let csv = "2026-11-06,river,6.0,\n2026-11-07,park,3.8,\n";
        let p = patch(vec![edit(
            "2026-11-06,river,6.0,\n2026-11-07,park,3.8,\n",
            "2026-11-06,river,6.0,\n2026-11-07,park,4.1,\n2026-11-08,forest,7.5,\n",
        )]);
        let once = apply(csv, &p).unwrap().text;
        let e = apply(&once, &p).unwrap_err();
        assert!(
            e.contains("Its new text is at line 1, so this patch may have gone through already"),
            "{e}"
        );
    }

    #[test]
    fn the_same_patch_twice_is_refused_also_when_the_old_rows_stay() {
        // No old row changes, so the old text is still in the file after the first try.
        let csv = "2026-11-06,river,6.0,\n2026-11-07,park,3.8,\n";
        let p = patch(vec![edit(
            "2026-11-06,river,6.0,\n2026-11-07,park,3.8,\n",
            "2026-11-06,river,6.0,\n2026-11-07,park,3.8,\n2026-11-08,forest,7.5,\n",
        )]);
        let once = apply(csv, &p).unwrap().text;
        let e = apply(&once, &p).unwrap_err();
        assert!(e.contains("already holds its new text at line 1"), "{e}");
    }

    #[test]
    fn the_done_check_looks_only_at_the_chosen_match() {
        // An insert after the second h. The first h is followed by x already.
        let mut e = edit("h\n", "h\nx\n");
        e.line = Some(3);
        let a = apply("h\nx\nh\n", &patch(vec![e])).unwrap();
        assert_eq!(a.text, "h\nx\nh\nx\n");
        // Without line, the chat learns where the matches are.
        let err = apply("h\nx\nh\n", &patch(vec![edit("h\n", "h\nx\n")])).unwrap_err();
        assert!(err.contains("occurs 2 times, at lines 1 and 3"), "{err}");
    }

    #[test]
    fn the_patch_json_is_read() {
        let p = Patch::parse(br#"{"edits":[{"old":"a\n","new":"b\n","line":3}],"append":"c\n"}"#)
            .unwrap();
        assert_eq!(p.edits[0].line, Some(3));
        assert_eq!(p.append.as_deref(), Some("c\n"));
    }

    #[test]
    fn a_wrong_patch_json_is_refused() {
        for (json, said) in [
            (r#"[]"#, "one JSON object"),
            (r#"{}"#, "at least one edit or an append"),
            (r#"{"edits":[]}"#, "at least one edit or an append"),
            (r#"{"edit":[]}"#, "unknown field edit"),
            (r#"{"edits":[{"old":"a"}]}"#, "edit 1 needs new"),
            (r#"{"edits":[{"old":"","new":"b"}]}"#, "is empty"),
            (
                r#"{"edits":[{"old":"a","new":"b","line":0}]}"#,
                "number from 1",
            ),
            (
                r#"{"edits":[{"old":"a","text":"b"}]}"#,
                "unknown field text",
            ),
            (r#"{"append":""}"#, "append is empty"),
            (r#"{"append":3}"#, "append must be text"),
            ("not json", "not valid JSON"),
        ] {
            let e = Patch::parse(json.as_bytes()).unwrap_err();
            assert!(e.contains(said), "{json}: {e}");
        }
    }
}
