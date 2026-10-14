//! Applying edits to file contents: exact search/replace edits or a single-file unified diff.
//!
//! Both are all-or-nothing: if any edit or hunk doesn't match, nothing is written and the error
//! says what didn't match and how to fix the call.

use std::fmt::Write as _;

pub(crate) const MAX_PATCH_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_PATCH_EDITS: usize = 128;
const MAX_LINES: usize = 65_536;
const MAX_MATCHES: usize = 65_536;
const MAX_WORK_BYTES: usize = 64 * MAX_PATCH_BYTES;

fn limit_error() -> PatchError {
    PatchError(
        "patch exceeds resource limits (1 MiB text, 128 edits/hunks, bounded matching work)".into(),
    )
}

fn check_bytes(size: Option<usize>) -> Result<usize, PatchError> {
    size.filter(|&size| size <= MAX_PATCH_BYTES)
        .ok_or_else(limit_error)
}

struct WorkBudget(usize);

impl WorkBudget {
    fn charge(&mut self, bytes: usize) -> Result<(), PatchError> {
        self.0 = self.0.checked_sub(bytes).ok_or_else(limit_error)?;
        Ok(())
    }
}

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

fn restore_crlf(text: String, crlf: bool) -> Result<String, PatchError> {
    if crlf {
        check_bytes(
            text.len()
                .checked_add(text.bytes().filter(|&b| b == b'\n').count()),
        )?;
        Ok(text.replace('\n', "\r\n"))
    } else {
        check_bytes(Some(text.len()))?;
        Ok(text)
    }
}

/// 1-based line number of byte `offset`.
fn line_of(text: &str, offset: usize) -> usize {
    text[..offset].matches('\n').count() + 1
}

/// Applies `edits` in order. Each `old` must occur exactly once (unless `replace_all`).
pub fn apply_search_replace(original: &str, edits: &[SearchReplace]) -> Result<String, PatchError> {
    check_bytes(Some(original.len()))?;
    if edits.len() > MAX_PATCH_EDITS {
        return Err(limit_error());
    }
    check_bytes(edits.iter().try_fold(0usize, |total, edit| {
        total
            .checked_add(edit.old.len())?
            .checked_add(edit.new.len())
    }))?;
    let mut work = WorkBudget(MAX_WORK_BYTES);
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
        work.charge(text.len().saturating_mul(2))?;
        let mut positions = Vec::new();
        let mut count = 0usize;
        for (position, _) in text.match_indices(&old) {
            count += 1;
            if count > MAX_MATCHES {
                return Err(limit_error());
            }
            if positions.len() < 5 {
                positions.push(position);
            }
        }
        if count == 1 || edit.replace_all {
            check_bytes(
                count
                    .checked_mul(old.len())
                    .and_then(|removed| text.len().checked_sub(removed))
                    .and_then(|retained| count.checked_mul(new.len())?.checked_add(retained)),
            )?;
        }
        match count {
            0 => {
                let mut msg = format!(
                    "edit {n}: `old` text not found. It must match the file exactly, including \
                     whitespace and indentation; re-read the file with read_file and copy the \
                     lines verbatim."
                );
                if let Some(line) = fuzzy_line(&text, &old, &mut work)? {
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
                work.charge(text.len().saturating_mul(positions.len()))?;
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
    restore_crlf(text, crlf)
}

/// First line where `needle`'s lines match `text`'s lines with whitespace trimmed.
fn fuzzy_line(
    text: &str,
    needle: &str,
    work: &mut WorkBudget,
) -> Result<Option<usize>, PatchError> {
    if text.lines().count() > MAX_LINES || needle.lines().count() > MAX_LINES {
        return Err(limit_error());
    }
    let hay: Vec<&str> = text.lines().map(str::trim).collect();
    let pat: Vec<&str> = needle.lines().map(str::trim).collect();
    if pat.is_empty() || pat.len() > hay.len() {
        return Ok(None);
    }
    for i in 0..=hay.len() - pat.len() {
        if block_matches(&hay[i..i + pat.len()], &pat, work)? {
            return Ok(Some(i + 1));
        }
    }
    Ok(None)
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
    check_bytes(Some(diff.len()))?;
    if diff.lines().count() > MAX_LINES {
        return Err(limit_error());
    }
    let diff = diff.replace("\r\n", "\n");
    let mut hunks: Vec<Hunk> = Vec::new();
    for line in diff.lines() {
        if line.starts_with("@@") {
            if hunks.len() == MAX_PATCH_EDITS {
                return Err(limit_error());
            }
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
    check_bytes(Some(original.len()))?;
    if original.lines().count() > MAX_LINES {
        return Err(limit_error());
    }
    let (text, crlf) = split_crlf(original);
    let trailing_newline = text.is_empty() || text.ends_with('\n');
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let hunks = parse_unified(diff)?;
    let mut work = WorkBudget(MAX_WORK_BYTES);
    let mut line_bytes = lines.iter().map(|line| line.len() + 1).sum::<usize>();

    let mut search_from = 0usize; // hunks apply in order
    let mut offset: isize = 0; // line shift from earlier hunks
    for (index, hunk) in hunks.iter().enumerate() {
        let n = index + 1;
        let expected = isize::try_from(hunk.old_start.saturating_sub(1))
            .ok()
            .and_then(|start| start.checked_add(offset))
            .ok_or_else(limit_error)?
            .max(0) as usize;
        let at = if hunk.old.is_empty() {
            // Pure insertion: trust the header.
            Some(expected.min(lines.len()))
        } else {
            find_block(&lines, &hunk.old, search_from, expected, &mut work)?
        };
        let Some(at) = at else {
            return Err(PatchError(mismatch_message(n, hunk, &lines, expected)));
        };
        line_bytes = line_bytes
            .checked_sub(hunk.old.iter().map(|line| line.len() + 1).sum::<usize>())
            .and_then(|retained| {
                retained.checked_add(hunk.new.iter().map(|line| line.len() + 1).sum::<usize>())
            })
            .filter(|&bytes| bytes <= MAX_PATCH_BYTES + 1)
            .ok_or_else(limit_error)?;
        let line_count = lines
            .len()
            .checked_sub(hunk.old.len())
            .and_then(|count| count.checked_add(hunk.new.len()))
            .filter(|&count| count <= MAX_LINES)
            .ok_or_else(limit_error)?;
        let output_bytes =
            line_bytes.saturating_sub(usize::from(!trailing_newline && line_count > 0));
        check_bytes(Some(output_bytes))?;
        work.charge(line_bytes)?;
        lines.splice(at..at + hunk.old.len(), hunk.new.iter().cloned());
        search_from = at + hunk.new.len();
        offset = offset
            .checked_add(hunk.new.len() as isize - hunk.old.len() as isize)
            .ok_or_else(limit_error)?;
    }

    let mut out = lines.join("\n");
    if trailing_newline && !lines.is_empty() {
        out.push('\n');
    }
    restore_crlf(out, crlf)
}

/// Position of `block` in `lines` at or after `from`, closest to `expected`.
fn find_block(
    lines: &[String],
    block: &[String],
    from: usize,
    expected: usize,
    work: &mut WorkBudget,
) -> Result<Option<usize>, PatchError> {
    if block.len() > lines.len() {
        return Ok(None);
    }
    if expected >= from
        && expected <= lines.len() - block.len()
        && block_matches(&lines[expected..expected + block.len()], block, work)?
    {
        return Ok(Some(expected));
    }
    let mut closest = None;
    for i in from..=lines.len() - block.len() {
        if block_matches(&lines[i..i + block.len()], block, work)?
            && closest
                .is_none_or(|previous: usize| i.abs_diff(expected) < previous.abs_diff(expected))
        {
            closest = Some(i);
        }
    }
    Ok(closest)
}

fn block_matches<T: AsRef<str>>(
    lines: &[T],
    block: &[T],
    work: &mut WorkBudget,
) -> Result<bool, PatchError> {
    for (line, wanted) in lines.iter().zip(block) {
        let (line, wanted) = (line.as_ref(), wanted.as_ref());
        work.charge(line.len().max(wanted.len()).max(1))?;
        if line != wanted {
            return Ok(false);
        }
    }
    Ok(true)
}

fn mismatch_message(n: usize, hunk: &Hunk, lines: &[String], expected: usize) -> String {
    let mut msg = format!(
        "hunk {n} (`{}`) does not apply: its context/removed lines were not found in the file.",
        hunk.header
    );
    // Show the first differing line at the expected position.
    for (k, want) in hunk.old.iter().enumerate() {
        let line_no = expected.saturating_add(k);
        match lines.get(line_no) {
            Some(found) if found == want => continue,
            Some(found) => {
                let _ = write!(
                    msg,
                    " At line {}: expected `{want}`, found `{found}`.",
                    line_no.saturating_add(1)
                );
                break;
            }
            None => {
                let _ = write!(
                    msg,
                    " Expected `{want}` at line {}, past the end of the file ({} lines).",
                    line_no.saturating_add(1),
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

    #[test]
    fn expanding_replacements_stop_within_resource_limits() {
        let edits = vec![
            SearchReplace {
                old: "a".into(),
                new: "aa".into(),
                replace_all: true
            };
            32
        ];
        assert!(
            apply_search_replace("a", &edits)
                .unwrap_err()
                .0
                .contains("resource limits")
        );
    }

    #[test]
    fn input_edit_and_work_limits_are_enforced() {
        let oversized = "x".repeat(MAX_PATCH_BYTES + 1);
        assert!(apply_search_replace(&oversized, &[edit("x", "y")]).is_err());
        assert!(apply_unified_diff(&oversized, "@@ -1 +1 @@\n-x\n+y\n").is_err());
        assert!(apply_search_replace("a", &[edit("a", &oversized)]).is_err());
        assert!(apply_unified_diff("a", &oversized).is_err());
        assert!(apply_search_replace("a", &vec![edit("a", "a"); MAX_PATCH_EDITS + 1]).is_err());
        assert!(
            apply_unified_diff("", &"@@ -0,0 +1 @@\n+x\n".repeat(MAX_PATCH_EDITS + 1)).is_err()
        );
        let text = format!("{}unique", "x".repeat(MAX_PATCH_BYTES / 2));
        assert!(
            apply_search_replace(&text, &vec![edit("unique", "unique"); MAX_PATCH_EDITS]).is_err()
        );
    }

    #[test]
    fn unicode_and_crlf_survive_both_patch_formats() {
        let original = "café\r\n🌹\r\n";
        let expected = "té\r\n🌷\r\n";
        assert_eq!(
            apply_search_replace(original, &[edit("café\n🌹", "té\n🌷")]).unwrap(),
            expected
        );
        assert_eq!(
            apply_unified_diff(original, "@@ -1,2 +1,2 @@\n-café\n-🌹\n+té\n+🌷\n").unwrap(),
            expected
        );
    }

    #[test]
    fn crlf_restoration_and_extreme_hunk_numbers_are_bounded() {
        let replacement = "x".repeat(MAX_PATCH_BYTES - 16) + &"\n".repeat(10);
        assert!(apply_search_replace("head\r\n", &[edit("head", &replacement)]).is_err());
        let diff = format!("@@ -{},1 +1,1 @@\n-a\n+b\n", usize::MAX);
        assert!(apply_unified_diff("a\n", &diff).is_err());
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
