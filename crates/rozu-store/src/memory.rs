//! Project memory: `ROZU.md` and `AGENTS.md` in the workspace root plus notes pinned with
//! `/pin` (`.rozu/pinned.md`), sent as one system message after the system prompt.

use std::fs::File;
use std::io::{self, Read, Write};
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
        for name in MEMORY_FILES.into_iter().chain([PINNED_FILE]) {
            let label = if name == PINNED_FILE {
                format!("{PROTECTED_DIR}/{PINNED_FILE}")
            } else {
                name.to_owned()
            };
            match memory_file(root, name, false).and_then(read_memory) {
                Ok(text) if !text.trim().is_empty() => sources.push((label, text)),
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
    let note = note.trim();
    let appended = note
        .len()
        .checked_add(3)
        .filter(|&bytes| bytes <= MAX_MEMORY_BYTES)
        .ok_or_else(|| io::Error::other("pinned memory exceeds 32 KB"))?;
    let mut file = memory_file(root, PINNED_FILE, true)?;
    if file
        .metadata()?
        .len()
        .checked_add(appended as u64)
        .is_none_or(|bytes| bytes > MAX_MEMORY_BYTES as u64)
    {
        return Err(io::Error::other("pinned memory exceeds 32 KB"));
    }
    file.write_all(format!("- {note}\n").as_bytes())?;
    Ok(pinned_path(root))
}

fn pinned_path(root: &Path) -> PathBuf {
    root.join(PROTECTED_DIR).join(PINNED_FILE)
}

fn read_memory(file: File) -> io::Result<String> {
    let mut bytes = Vec::new();
    file.take(MAX_MEMORY_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    let cut = bytes.len() > MAX_MEMORY_BYTES;
    if cut {
        bytes.truncate(MAX_MEMORY_BYTES);
    }
    if let Err(error) = std::str::from_utf8(&bytes) {
        if cut && error.error_len().is_none() {
            bytes.truncate(error.valid_up_to());
        } else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, error));
        }
    }
    let mut text = String::from_utf8(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if cut && !text.trim().is_empty() {
        text.push_str("\n[… cut: the file is over 32 KB]");
    }
    Ok(text)
}

/// Open beneath stable directory handles. Never follow a memory-file or `.rozu` symlink.
#[cfg(unix)]
fn memory_file(root: &Path, name: &str, append: bool) -> io::Result<File> {
    use std::ffi::{CStr, CString};
    use std::os::fd::{AsRawFd, FromRawFd};

    fn open_at(parent: &File, name: &CStr, flags: i32) -> io::Result<File> {
        // The parent handle stays alive; openat returns a new, exclusively owned descriptor.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                0o600,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // Successful openat transfers ownership of this descriptor to File.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    let root = fs_open_directory(root)?;
    let directory = if name == PINNED_FILE {
        if append {
            // mkdirat is relative to the same root handle used to open the directory.
            let result = unsafe { libc::mkdirat(root.as_raw_fd(), c".rozu".as_ptr(), 0o700) };
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::AlreadyExists {
                    return Err(error);
                }
            }
        }
        open_at(&root, c".rozu", libc::O_RDONLY | libc::O_DIRECTORY)?
    } else {
        root
    };
    let name =
        CString::new(name).map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let flags = if append {
        libc::O_WRONLY | libc::O_APPEND | libc::O_CREAT
    } else {
        libc::O_RDONLY
    };
    let file = open_at(&directory, &name, flags)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("project memory must be a regular file"));
    }
    Ok(file)
}

#[cfg(unix)]
fn fs_open_directory(root: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(root.canonicalize()?)
}

#[cfg(not(unix))]
fn memory_file(_root: &Path, _name: &str, _append: bool) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "secure project memory is supported on macOS and Linux",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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

    #[test]
    fn sparse_memory_reads_are_bounded_and_cut_on_utf8_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ROZU.md");
        let mut file = File::create(&path).unwrap();
        file.write_all("x".repeat(MAX_MEMORY_BYTES - 1).as_bytes())
            .unwrap();
        file.write_all("🌹".as_bytes()).unwrap();
        file.set_len(1024 * 1024 * 1024).unwrap();
        let memory = ProjectMemory::load(dir.path()).unwrap();
        assert_eq!(
            memory.sources[0].1,
            "x".repeat(MAX_MEMORY_BYTES - 1) + "\n[… cut: the file is over 32 KB]"
        );
        fs::write(path, [0xff]).unwrap();
        assert!(ProjectMemory::load(dir.path()).is_err());
    }

    #[test]
    fn pins_cannot_grow_beyond_the_memory_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = pin(dir.path(), "keep").unwrap();
        assert!(pin(dir.path(), &"x".repeat(MAX_MEMORY_BYTES)).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "- keep\n");
        fs::write(&path, "x".repeat(MAX_MEMORY_BYTES - 1)).unwrap();
        assert!(pin(dir.path(), "more").is_err());
        assert_eq!(
            fs::metadata(path).unwrap().len(),
            (MAX_MEMORY_BYTES - 1) as u64
        );
    }

    #[cfg(unix)]
    #[test]
    fn memory_files_cannot_follow_symlinks_outside_or_inside_the_project() {
        use std::os::unix::fs::symlink;
        for name in ["ROZU.md", "AGENTS.md", ".rozu/pinned.md"] {
            for inside in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let root = dir.path().join("workspace");
                fs::create_dir_all(root.join(".rozu")).unwrap();
                let canary = if inside {
                    root.join("private")
                } else {
                    dir.path().join("private")
                };
                fs::write(&canary, "private").unwrap();
                symlink(&canary, root.join(name)).unwrap();
                assert!(ProjectMemory::load(&root).is_err(), "{name}");
                if name.ends_with("pinned.md") {
                    assert!(pin(&root, "bad").is_err());
                }
                assert_eq!(fs::read_to_string(canary).unwrap(), "private");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn memory_rejects_pinned_directory_aliases() {
        use std::os::unix::fs::symlink;
        for inside in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("workspace");
            fs::create_dir(&root).unwrap();
            let destination = if inside {
                root.join("settings")
            } else {
                dir.path().join("settings")
            };
            fs::create_dir(&destination).unwrap();
            fs::write(destination.join("pinned.md"), "keep").unwrap();
            symlink(&destination, root.join(".rozu")).unwrap();
            assert!(ProjectMemory::load(&root).is_err());
            assert!(pin(&root, "bad").is_err());
            assert_eq!(
                fs::read_to_string(destination.join("pinned.md")).unwrap(),
                "keep"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn memory_rejects_directories_and_fifos_without_blocking() {
        use std::ffi::CString;
        for name in ["ROZU.md", ".rozu/pinned.md"] {
            for fifo in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                fs::create_dir(dir.path().join(".rozu")).unwrap();
                let path = dir.path().join(name);
                if fifo {
                    let path = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
                    // Only creates a FIFO inside this test's private temporary directory.
                    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
                } else {
                    fs::create_dir(path).unwrap();
                }
                assert!(ProjectMemory::load(dir.path()).is_err());
                if name.ends_with("pinned.md") {
                    assert!(pin(dir.path(), "bad").is_err());
                }
            }
        }
    }
}
