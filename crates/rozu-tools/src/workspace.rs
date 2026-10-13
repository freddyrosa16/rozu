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
        Ok(Self {
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

    /// `path` relative to the root for display (`.` for the root itself).
    pub fn display_relative(&self, path: &Path) -> String {
        match path.strip_prefix(&*self.root) {
            Ok(rel) if rel.as_os_str().is_empty() => ".".to_owned(),
            Ok(rel) => rel.display().to_string(),
            Err(_) => path.display().to_string(),
        }
    }
}

/// Lexically resolves `.` and `..` without touching the filesystem.
fn normalize(path: &Path) -> PathBuf {
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
        assert!(matches!(
            ws.resolve("../../nope"),
            Err(PathError::Outside(_))
        ));
    }

    #[test]
    fn rejects_absolute_paths_outside() {
        let (dir, ws) = setup();
        let outside = dir.path().join("secret.txt");
        assert!(matches!(
            ws.resolve(outside.to_str().unwrap()),
            Err(PathError::Outside(_))
        ));
        assert!(matches!(ws.resolve("/"), Err(PathError::Outside(_))));
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
        assert!(matches!(ws.resolve("link.txt"), Err(PathError::Outside(_))));
        assert!(matches!(
            ws.resolve("parent/secret.txt"),
            Err(PathError::Outside(_))
        ));
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
    fn display_is_relative_to_root() {
        let (_dir, ws) = setup();
        assert_eq!(ws.display_relative(ws.root()), ".");
        assert_eq!(
            ws.display_relative(&ws.root().join("src/main.rs")),
            Path::new("src/main.rs").display().to_string()
        );
    }
}
