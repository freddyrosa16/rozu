//! Project memory: `ROZU.md` and `AGENTS.md` in the workspace root plus notes pinned with
//! `/pin` (`.rozu/pinned.md`), sent as one system message after the system prompt.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use rozu_tools::PROTECTED_DIR;

pub const MEMORY_FILES: [&str; 2] = ["ROZU.md", "AGENTS.md"];
pub const PINNED_FILE: &str = "pinned.md";
/// Per file; the rest is cut so one huge file can't fill the context window.
pub const MAX_MEMORY_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectMemory {
    /// `(label, text)` in prompt order.
    pub sources: Vec<(String, String)>,
}

impl ProjectMemory {
    pub fn load(root: &Path) -> io::Result<Self> {
        let mut sources = Vec::new();
        let pinned = pinned_path(root);
        let files = MEMORY_FILES
            .iter()
            .map(|name| (name.to_string(), root.join(name)))
            .chain([(format!("{PROTECTED_DIR}/{PINNED_FILE}"), pinned)]);
        for (label, path) in files {
            match fs::read_to_string(&path) {
                Ok(text) if !text.trim().is_empty() => sources.push((label, cap(text))),
                Ok(_) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
        }
        Ok(Self { sources })
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// The system message text, or `None` without memory.
    pub fn prompt(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut text = String::from(
            "Project memory (pinned by the user; follow it, and it stays in context even when \
             older turns are summarized):",
        );
        for (label, body) in &self.sources {
            text.push_str(&format!("\n\n<{label}>\n{}\n</{label}>", body.trim_end()));
        }
        Some(text)
    }
}

/// Appends `note` to the pinned notes, returning the file.
pub fn pin(root: &Path, note: &str) -> io::Result<PathBuf> {
    let path = pinned_path(root);
    fs::create_dir_all(path.parent().expect("pinned file has a parent"))?;
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(file, "- {}", note.trim())?;
    Ok(path)
}

fn pinned_path(root: &Path) -> PathBuf {
    root.join(PROTECTED_DIR).join(PINNED_FILE)
}

fn cap(mut text: String) -> String {
    if text.len() > MAX_MEMORY_BYTES {
        let mut end = MAX_MEMORY_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n[… cut: the file is over 32 KB]");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_files_in_order_and_pins() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(ProjectMemory::load(dir.path()).unwrap().prompt(), None);

        fs::write(dir.path().join("AGENTS.md"), "use tabs\n").unwrap();
        fs::write(dir.path().join("ROZU.md"), "be brief").unwrap();
        pin(dir.path(), " run cargo test \n").unwrap();
        pin(dir.path(), "no unwrap").unwrap();

        let memory = ProjectMemory::load(dir.path()).unwrap();
        let labels: Vec<_> = memory.sources.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, ["ROZU.md", "AGENTS.md", ".rozu/pinned.md"]);
        let prompt = memory.prompt().unwrap();
        assert!(
            prompt.contains("<ROZU.md>\nbe brief\n</ROZU.md>"),
            "{prompt}"
        );
        assert!(prompt.contains("- run cargo test\n- no unwrap"), "{prompt}");
    }

    #[test]
    fn huge_files_are_cut() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("ROZU.md"), "é".repeat(MAX_MEMORY_BYTES)).unwrap();
        let memory = ProjectMemory::load(dir.path()).unwrap();
        let text = &memory.sources[0].1;
        assert!(text.len() < MAX_MEMORY_BYTES + 100);
        assert!(text.ends_with("over 32 KB]"));
    }
}
