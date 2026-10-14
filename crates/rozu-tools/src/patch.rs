//! Applying edits to file contents: exact search/replace edits or a single-file unified diff.
//!
//! Both are all-or-nothing: if any edit or hunk doesn't match, nothing is written and the error
//! says what didn't match and how to fix the call.

use std::fmt::Write as _;

/// One search/replace edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchReplace {
    pub old: String,
    pub new: String,
    /// Replace every occurrence instead of requiring exactly one.
    pub replace_all: bool,
}

/// Why a patch didn't apply. The message is written for the model.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PatchError(pub String);

/// Line endings of the original file, restored after editing.
fn split_crlf(text: &str) -> (String, bool) {
    if text.contains("\r\n") {
        (text.replace("\r\n", "\n"), true)
    } else {
        (text.to_owned(), false)
    }
}

fn restore_crlf(text: String, crlf: bool) -> String {
    if crlf {
        text.replace('\n', "\r\n")
    } else {
        text
    }
}

/// 1-based line number of byte `offset`.
fn line_of(text: &str, offset: usize) -> usize {
    text[..offset].matches('\n').count() + 1
}

/// Applies `edits` in order. Each `old` must occur exactly once (unless `replace_all`).
pub fn apply_search_replace(original: &str, edits: &[SearchReplace]) -> Result<String, PatchError> {
    let (mut text, crlf) = split_crlf(original);
    for (index, edit) in edits.iter().enumerate() {
        let n = index + 1;
        let old = edit.old.replace("\r\n", "\n");
        let new = edit.new.replace("\r\n", "\n");
        if old.is_empty() {
            return Err(PatchError(format!(
                "edit {n}: `old` is empty; to create or replace a whole file use write_file"
            )));
        }
        let positions: Vec<usize> = text.match_indices(&old).map(|(i, _)| i).collect();
        match positions.len() {
            0 => {
                let mut msg = format!(
                    "edit {n}: `old` text not found. It must match the file exactly, including \
                     whitespace and indentation; re-read the file with read_file and copy the \
                     lines verbatim."
                );
                if let Some(line) = fuzzy_line(&text, &old) {
                    let _ = write!(
                        msg,
                        " A match ignoring whitespace starts at line {line}: check indentation."
                    );
                }
                return Err(PatchError(msg));
            }
            1 => text.replace_range(positions[0]..positions[0] + old.len(), &new),
            _ if edit.replace_all => text = text.replace(&old, &new),
            count => {
                let lines: Vec<String> = positions
                    .iter()
                    .take(5)
                    .map(|&p| line_of(&text, p).to_string())
                    .collect();
                return Err(PatchError(format!(
                    "edit {n}: `old` text matches {count} times (lines {}); include more \
                     surrounding lines to make it unique, or set replace_all",
                    lines.join(", ")
                )));
            }
        }
    }
    Ok(restore_crlf(text, crlf))
}

/// First line where `needle`'s lines match `text`'s lines with whitespace trimmed.
fn fuzzy_line(text: &str, needle: &str) -> Option<usize> {
    let hay: Vec<&str> = text.lines().map(str::trim).collect();
    let pat: Vec<&str> = needle.lines().map(str::trim).collect();
    if pat.is_empty() || pat.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - pat.len())
        .find(|&i| hay[i..i + pat.len()] == pat[..])
        .map(|i| i + 1)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Hunk {
    header: String,
    /// 1-based start line in the original file (0 for an empty file).
    old_start: usize,
    old: Vec<String>,
    new: Vec<String>,
}

fn parse_hunk_start(header: &str) -> Option<usize> {
    // "@@ -12,5 +12,6 @@ optional context"
    let rest = header.strip_prefix("@@")?.trim_start();
    let old = rest.strip_prefix('-')?;
    let end = old.find([',', ' ']).unwrap_or(old.len());
    old[..end].parse().ok()
}

fn parse_unified(diff: &str) -> Result<Vec<Hunk>, PatchError> {
    let diff = diff.replace("\r\n", "\n");
    let mut hunks: Vec<Hunk> = Vec::new();
    for line in diff.lines() {
        if line.starts_with("@@") {
            let old_start = parse_hunk_start(line).ok_or_else(|| {
                PatchError(format!(
                    "malformed hunk header `{line}`; expected `@@ -start,count +start,count @@`"
                ))
            })?;
            hunks.push(Hunk {
                header: line.to_owned(),
                old_start,
                old: Vec::new(),
                new: Vec::new(),
            });
            continue;
        }
        let Some(hunk) = hunks.last_mut() else {
            // File headers before the first hunk.
            if line.starts_with("---")
                || line.starts_with("+++")
                || line.starts_with("diff ")
                || line.starts_with("index ")
                || line.trim().is_empty()
            {
                continue;
            }
            return Err(PatchError(format!(
                "unexpected line before the first `@@` hunk header: `{line}`"
            )));
        };
        if line.starts_with("diff --git") {
            return Err(PatchError(
                "the diff touches more than one file; call apply_patch once per file".into(),
            ));
        }
        if let Some(rest) = line.strip_prefix('+') {
            hunk.new.push(rest.to_owned());
        } else if let Some(rest) = line.strip_prefix('-') {
            hunk.old.push(rest.to_owned());
        } else if let Some(rest) = line.strip_prefix(' ') {
            hunk.old.push(rest.to_owned());
            hunk.new.push(rest.to_owned());
        } else if line.is_empty() {
            // Some generators drop the space on blank context lines.
            hunk.old.push(String::new());
            hunk.new.push(String::new());
        } else if line.starts_with('\\') {
            // "\ No newline at end of file"
        } else {
            return Err(PatchError(format!(
                "unexpected line in hunk `{}`: `{line}`; hunk lines must start with ' ', '-' or '+'",
                hunk.header
            )));
        }
    }
    if hunks.is_empty() {
        return Err(PatchError(
            "no hunks found; a unified diff needs `@@ -start,count +start,count @@` headers".into(),
        ));
    }
    Ok(hunks)
}

/// Applies a single-file unified diff. Hunks are located by their context lines, preferring the
/// position in the header; a hunk whose context isn't in the file is rejected.
pub fn apply_unified_diff(original: &str, diff: &str) -> Result<String, PatchError> {
    let (text, crlf) = split_crlf(original);
    let trailing_newline = text.is_empty() || text.ends_with('\n');
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let hunks = parse_unified(diff)?;

    let mut search_from = 0usize; // hunks apply in order
    let mut offset: isize = 0; // line shift from earlier hunks
    for (index, hunk) in hunks.iter().enumerate() {
        let n = index + 1;
        let expected = (hunk.old_start.saturating_sub(1) as isize + offset).max(0) as usize;
        let at = if hunk.old.is_empty() {
            // Pure insertion: trust the header.
            Some(expected.min(lines.len()))
        } else {
            find_block(&lines, &hunk.old, search_from, expected)
        };
        let Some(at) = at else {
            return Err(PatchError(mismatch_message(n, hunk, &lines, expected)));
        };
        lines.splice(at..at + hunk.old.len(), hunk.new.iter().cloned());
        search_from = at + hunk.new.len();
        offset += hunk.new.len() as isize - hunk.old.len() as isize;
    }

    let mut out = lines.join("\n");
    if trailing_newline && !lines.is_empty() {
        out.push('\n');
    }
    Ok(restore_crlf(out, crlf))
}

/// Position of `block` in `lines` at or after `from`, closest to `expected`.
fn find_block(lines: &[String], block: &[String], from: usize, expected: usize) -> Option<usize> {
    if block.len() > lines.len() {
        return None;
    }
    let matches_at = |i: usize| lines[i..i + block.len()] == *block;
    if expected >= from && expected + block.len() <= lines.len() && matches_at(expected) {
        return Some(expected);
    }
    (from..=lines.len() - block.len())
        .filter(|&i| matches_at(i))
        .min_by_key(|&i| i.abs_diff(expected))
}

fn mismatch_message(n: usize, hunk: &Hunk, lines: &[String], expected: usize) -> String {
    let mut msg = format!(
        "hunk {n} (`{}`) does not apply: its context/removed lines were not found in the file.",
        hunk.header
    );
    // Show the first differing line at the expected position.
    for (k, want) in hunk.old.iter().enumerate() {
        let line_no = expected + k;
        match lines.get(line_no) {
            Some(found) if found == want => continue,
            Some(found) => {
                let _ = write!(
                    msg,
                    " At line {}: expected `{want}`, found `{found}`.",
                    line_no + 1
                );
                break;
            }
            None => {
                let _ = write!(
                    msg,
                    " Expected `{want}` at line {}, past the end of the file ({} lines).",
                    line_no + 1,
                    lines.len()
                );
                break;
            }
        }
    }
    msg.push_str(
        " Re-read the file with read_file and regenerate the hunk, or use `edits` \
         (exact search/replace) instead.",
    );
    msg
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn edit(old: &str, new: &str) -> SearchReplace {
        SearchReplace {
            old: old.into(),
            new: new.into(),
            replace_all: false,
        }
    }

    #[test]
    fn search_replace_applies_in_order() {
        let out = apply_search_replace(
            "fn a() {}\nfn b() {}\n",
            &[
                edit("fn a()", "fn alpha()"),
                edit("alpha() {}", "alpha() { 1 }"),
            ],
        )
        .unwrap();
        assert_eq!(out, "fn alpha() { 1 }\nfn b() {}\n");
    }

    #[test]
    fn search_replace_rejects_missing_and_ambiguous() {
        for (text, edit, want) in [
            (
                "x = 1\n",
                edit("y = 1", "y = 2"),
                "edit 1: `old` text not found",
            ),
            (
                "    a\n    b\n",
                edit("a\nb", "c"),
                "ignoring whitespace starts at line 1",
            ),
            ("a\nb\na\n", edit("a", "c"), "matches 2 times (lines 1, 3)"),
            ("a", edit("", "c"), "empty"),
        ] {
            let err = apply_search_replace(text, &[edit]).unwrap_err();
            assert!(err.0.contains(want), "{err}");
        }
        let err = apply_search_replace("    x = 1\n", &[edit("x  =  1", "")]).unwrap_err();
        assert!(!err.0.contains("ignoring whitespace"), "{err}");

        let mut all = edit("a", "c");
        all.replace_all = true;
        assert_eq!(
            apply_search_replace("a\nb\na\n", &[all]).unwrap(),
            "c\nb\nc\n"
        );
    }

    #[test]
    fn search_replace_keeps_crlf() {
        let out = apply_search_replace("a\r\nb\r\n", &[edit("a\nb", "c\nd")]).unwrap();
        assert_eq!(out, "c\r\nd\r\n");
    }

    const FILE: &str = "one\ntwo\nthree\nfour\nfive\nsix\n";

    #[test]
    fn unified_diff_applies() {
        let diff = "--- a/f.txt\n+++ b/f.txt\n@@ -2,3 +2,3 @@\n two\n-three\n+THREE\n four\n@@ -5,2 +5,3 @@\n five\n six\n+seven\n";
        assert_eq!(
            apply_unified_diff(FILE, diff).unwrap(),
            "one\ntwo\nTHREE\nfour\nfive\nsix\nseven\n"
        );
    }

    #[test]
    fn unified_diff_tolerates_wrong_line_numbers() {
        let diff = "@@ -40,2 +40,2 @@\n four\n-five\n+5\n";
        assert_eq!(
            apply_unified_diff(FILE, diff).unwrap(),
            "one\ntwo\nthree\nfour\n5\nsix\n"
        );
    }

    #[test]
    fn unified_diff_rejects_context_mismatch() {
        let diff = "@@ -2,2 +2,2 @@\n two\n-tree\n+3\n";
        let err = apply_unified_diff(FILE, diff).unwrap_err();
        assert!(err.0.contains("hunk 1"), "{err}");
        assert!(
            err.0.contains("At line 3: expected `tree`, found `three`"),
            "{err}"
        );
        assert!(err.0.contains("read_file"), "{err}");
    }

    #[test]
    fn unified_diff_rejects_garbage_and_multi_file() {
        assert!(apply_unified_diff(FILE, "just text").is_err());
        assert!(apply_unified_diff(FILE, "").is_err());
        let multi = "@@ -1 +1 @@\n-one\n+1\ndiff --git a/x b/x\n";
        assert!(
            apply_unified_diff(FILE, multi)
                .unwrap_err()
                .0
                .contains("more than one file")
        );
        assert!(apply_unified_diff(FILE, "@@ bogus @@\n").is_err());
    }

    #[test]
    fn unified_diff_creates_content_in_empty_file() {
        let diff = "--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1,2 @@\n+hello\n+world\n";
        assert_eq!(apply_unified_diff("", diff).unwrap(), "hello\nworld\n");
    }

    #[test]
    fn unified_diff_keeps_missing_trailing_newline() {
        let diff = "@@ -1,2 +1,2 @@\n a\n-b\n+c\n\\ No newline at end of file\n";
        assert_eq!(apply_unified_diff("a\nb", diff).unwrap(), "a\nc");
    }

    proptest! {
        #[test]
        fn replace_then_reverse_restores(
            lines in proptest::collection::vec("[a-z]{1,8}", 1..20),
            pick in any::<proptest::sample::Index>(),
        ) {
            let text = lines.join("\n") + "\n";
            let target = pick.get(&lines).clone();
            let unique = text.matches(&target).count() == 1;
            let forward = apply_search_replace(&text, &[edit(&target, "REPLACED-LINE")]);
            if unique {
                let changed = forward.unwrap();
                let back = apply_search_replace(&changed, &[edit("REPLACED-LINE", &target)]).unwrap();
                prop_assert_eq!(back, text);
            } else {
                prop_assert!(forward.is_err());
            }
        }
    }
}
