//! Finding a vendor CLI's binary: an override variable, then `PATH`, then known install
//! locations.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// `override_path` if non-empty (used as given, even if it doesn't exist, so the error names
/// it), else the first executable `name` on `path_var`, else the first executable `fallbacks`.
pub fn find_program_in(
    name: &str,
    override_path: Option<&OsStr>,
    path_var: Option<&OsStr>,
    fallbacks: &[PathBuf],
) -> Option<PathBuf> {
    if let Some(path) = override_path.filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }
    path_var
        .into_iter()
        .flat_map(std::env::split_paths)
        .map(|dir| dir.join(name))
        .chain(fallbacks.iter().cloned())
        .find(|candidate| is_executable(candidate))
}

pub fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}
