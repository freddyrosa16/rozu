//! The workspace root that every file tool is confined to.

use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// Why a tool path was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("path is outside the workspace: {0}")]
    Outside(String),
    #[error("no such file or directory: {0}")]
    NotFound(String),
    #[error("cannot access {path}: {message}")]
    Io { path: String, message: String },
}

/// A canonicalized directory that tools may read from. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Workspace {
    root: Arc<PathBuf>,
    #[cfg(unix)]
    pub(crate) directory: Arc<std::fs::File>,
}

impl Workspace {
    /// Opens `root`, which must be an existing directory. Symlinks in `root` itself are resolved.
    pub fn new(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = root.as_ref().canonicalize()?;
        if !root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!("{} is not a directory", root.display()),
            ));
        }
        #[cfg(unix)]
        let directory = Arc::new(crate::workspace_file::open_root(&root)?);
        Ok(Self {
            #[cfg(unix)]
            directory,
            root: Arc::new(root),
        })
    }

    /// The canonical workspace root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolves a tool-supplied path (relative to the root, or absolute) to a canonical path that
    /// exists inside the workspace.
    ///
    /// `..` segments and symlinks are resolved before the check, so neither can escape the root.
    pub fn resolve(&self, path: &str) -> Result<PathBuf, PathError> {
        let requested = Path::new(path);
        let joined = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            self.root.join(requested)
        };
        match joined.canonicalize() {
            Ok(canonical) if canonical.starts_with(&*self.root) => Ok(canonical),
            Ok(_) => Err(PathError::Outside(path.to_owned())),
            // A missing path that would also land outside the root is reported as outside, so
            // probing `../..` doesn't reveal what exists beyond the workspace.
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                if normalize(&joined).starts_with(&*self.root) {
                    Err(PathError::NotFound(path.to_owned()))
                } else {
                    Err(PathError::Outside(path.to_owned()))
                }
            }
            Err(err) => Err(PathError::Io {
                path: path.to_owned(),
                message: err.to_string(),
            }),
        }
    }

    /// Resolves a path a tool wants to write. Like [`Workspace::resolve`], but the file (and any
    /// missing parent directories) may not exist yet. The deepest existing ancestor is
    /// canonicalized, so a symlinked directory can't smuggle the write outside the root, and an
    /// existing file is resolved through its own symlink.
    pub fn resolve_for_write(&self, path: &str) -> Result<PathBuf, PathError> {
        let requested = Path::new(path);
        let joined = normalize(&if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            self.root.join(requested)
        });
        let mut existing = joined.as_path();
        let mut missing = Vec::new();
        let base = loop {
            match existing.canonicalize() {
                Ok(canonical) => break canonical,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    let (Some(parent), Some(name)) = (existing.parent(), existing.file_name())
                    else {
                        return Err(PathError::Outside(path.to_owned()));
                    };
                    missing.push(name.to_owned());
                    existing = parent;
                }
                Err(err) => {
                    return Err(PathError::Io {
                        path: path.to_owned(),
                        message: err.to_string(),
                    });
                }
            }
        };
        let resolved = missing.iter().rev().fold(base, |acc, name| acc.join(name));
        if resolved.starts_with(&*self.root) && resolved != *self.root {
            Ok(resolved)
        } else {
            Err(PathError::Outside(path.to_owned()))
        }
    }

    /// Whether `path` (absolute, or relative to the root) lexically stays inside the workspace.
    /// Doesn't touch the filesystem; used to classify shell arguments.
    pub fn contains_lexically(&self, path: &Path) -> bool {
        let joined = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        };
        normalize(&joined).starts_with(&*self.root)
    }

    /// `path` relative to the root for display (`.` for the root itself).
    pub fn display_relative(&self, path: &Path) -> String {
        match path.strip_prefix(&*self.root) {
            Ok(rel) if rel.as_os_str().is_empty() => ".".to_owned(),
            Ok(rel) => rel.display().to_string(),
            Err(_) => path.display().to_string(),
        }
    }
}

/// Directory (relative to the root) holding user-managed project settings and pinned memory.
/// File tools may not modify it.
pub const PROTECTED_DIR: &str = ".rozu";

impl Workspace {
    /// `<root>/.rozu`, which file tools and sandboxed commands may not write to.
    pub fn protected_dir(&self) -> PathBuf {
        self.root.join(PROTECTED_DIR)
    }

    /// Whether a path names protected settings, including aliases and missing descendants.
    /// The directory name is case-insensitive, since macOS file systems usually are.
    pub fn is_protected(&self, path: &Path) -> bool {
        let joined = self.root.join(path);
        let path = normalize(&joined);
        let named_settings = path
            .strip_prefix(&*self.root)
            .ok()
            .and_then(|rel| rel.components().next())
            .is_some_and(|first| {
                first
                    .as_os_str()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(PROTECTED_DIR)
            });
        named_settings
            || self.protected_dir().canonicalize().is_ok_and(|settings| {
                joined
                    .ancestors()
                    .find_map(|ancestor| ancestor.canonicalize().ok())
                    .is_some_and(|resolved| resolved.starts_with(settings))
            })
    }
}

/// Lexically resolves `.` and `..` without touching the filesystem.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn setup() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(dir.path().join("secret.txt"), "top secret").unwrap();
        let ws = Workspace::new(&root).unwrap();
        (dir, ws)
    }

    fn outside<T>(r: Result<T, PathError>) -> bool {
        matches!(r, Err(PathError::Outside(_)))
    }

    #[test]
    fn resolves_relative_paths_inside() {
        let (_dir, ws) = setup();
        let path = ws.resolve("src/main.rs").unwrap();
        assert_eq!(path, ws.root().join("src/main.rs"));
        assert_eq!(ws.resolve(".").unwrap(), ws.root());
        assert_eq!(ws.resolve("src/../src/main.rs").unwrap(), path);
    }

    #[test]
    fn accepts_absolute_paths_inside() {
        let (_dir, ws) = setup();
        let abs = ws.root().join("src/main.rs");
        assert_eq!(ws.resolve(abs.to_str().unwrap()).unwrap(), abs);
    }

    #[test]
    fn rejects_dotdot_escape() {
        let (_dir, ws) = setup();
        assert_eq!(
            ws.resolve("../secret.txt"),
            Err(PathError::Outside("../secret.txt".into()))
        );
        assert_eq!(
            ws.resolve("src/../../secret.txt"),
            Err(PathError::Outside("src/../../secret.txt".into()))
        );
        // Missing targets outside are still "outside", not "not found".
        assert!(outside(ws.resolve("../../nope")));
    }

    #[test]
    fn rejects_absolute_paths_outside() {
        let (dir, ws) = setup();
        let secret = dir.path().join("secret.txt");
        assert!(outside(ws.resolve(secret.to_str().unwrap())));
        assert!(outside(ws.resolve("/")));
    }

    #[test]
    fn reports_missing_paths_inside() {
        let (_dir, ws) = setup();
        assert_eq!(
            ws.resolve("src/missing.rs"),
            Err(PathError::NotFound("src/missing.rs".into()))
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        let (dir, ws) = setup();
        std::os::unix::fs::symlink(dir.path().join("secret.txt"), ws.root().join("link.txt"))
            .unwrap();
        std::os::unix::fs::symlink(dir.path(), ws.root().join("parent")).unwrap();
        assert!(outside(ws.resolve("link.txt")));
        assert!(outside(ws.resolve("parent/secret.txt")));
    }

    #[cfg(unix)]
    #[test]
    fn allows_symlink_within_workspace() {
        let (_dir, ws) = setup();
        std::os::unix::fs::symlink(ws.root().join("src/main.rs"), ws.root().join("alias.rs"))
            .unwrap();
        assert_eq!(
            ws.resolve("alias.rs").unwrap(),
            ws.root().join("src/main.rs")
        );
    }

    #[test]
    fn resolve_for_write_allows_new_paths_inside() {
        let (_dir, ws) = setup();
        assert_eq!(
            ws.resolve_for_write("src/new/deep.rs").unwrap(),
            ws.root().join("src/new/deep.rs")
        );
        assert_eq!(
            ws.resolve_for_write("src/main.rs").unwrap(),
            ws.root().join("src/main.rs")
        );
        for bad in ["../x.txt", "src/../../x.txt", "/etc/passwd", ".", "/"] {
            assert!(outside(ws.resolve_for_write(bad)), "{bad}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn resolve_for_write_rejects_symlinked_dir_escape() {
        let (dir, ws) = setup();
        std::os::unix::fs::symlink(dir.path(), ws.root().join("parent")).unwrap();
        assert!(outside(ws.resolve_for_write("parent/new.txt")));
        assert!(outside(ws.resolve_for_write("parent/secret.txt")));
    }

    #[cfg(unix)]
    #[test]
    fn protects_missing_files_through_outside_directory_aliases() {
        let (_dir, ws) = setup();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir(outside.path().join("settings")).unwrap();
        fs::create_dir(outside.path().join("other")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("settings"), ws.protected_dir()).unwrap();
        std::os::unix::fs::symlink(outside.path(), ws.root().join("alias")).unwrap();

        for path in [
            PathBuf::from(".rozu/new/pinned.md"),
            PathBuf::from("alias/settings/new/pinned.md"),
            outside.path().join("settings/new/pinned.md"),
            outside
                .path()
                .canonicalize()
                .unwrap()
                .join("settings/new/pinned.md"),
        ] {
            assert!(ws.is_protected(&path), "{}", path.display());
        }
        for path in [
            PathBuf::from("alias/other/new.txt"),
            outside.path().join("other/new.txt"),
            PathBuf::from("src/new.txt"),
        ] {
            assert!(!ws.is_protected(&path), "{}", path.display());
        }
    }

    #[test]
    fn display_is_relative_to_root() {
        let (_dir, ws) = setup();
        assert_eq!(ws.display_relative(ws.root()), ".");
        assert_eq!(
            ws.display_relative(&ws.root().join("src/main.rs")),
            Path::new("src/main.rs").display().to_string()
        );
    }
}
