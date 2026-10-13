//! Finding xAI's Grok Build CLI and asking it about itself.
//!
//! rozu runs the **native** binary directly, never the npm package's Node launcher (`node
//! grok-bootstrap.js`; see zed#64429): `grok` on `PATH` is resolved through its symlinks, and if
//! that lands on a script, the `grok-native` next to it is used.
//!
//! rozu never reads Grok's credential files (`~/.grok/auth.json`). Grok Build has no
//! login-status command (its docs list only `grok login` and `grok logout`), so `rozu auth
//! status` can't tell whether it is signed in; a turn reports "not signed in" when it is not.

use std::ffi::OsStr;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::claude::install::{parse_version, query};
use crate::which::{find_program_in, is_executable};

/// Overrides where rozu looks for the Grok binary (used as given).
pub const GROK_BIN_ENV: &str = "ROZU_GROK_BIN";
/// Homebrew's `bin` on Apple silicon (npm's global prefix there), checked after `PATH`.
pub const HOMEBREW_GROK: &str = "/opt/homebrew/bin/grok";
/// The native binary the npm package installs next to its launcher.
pub const NATIVE_NAME: &str = "grok-native";

/// Removed from Grok's environment: rozu uses only Grok's own (subscription) login, never an
/// API key, and never a chat proxy (which would see the user's bearer token).
pub const ENV_REMOVE: &[&str] = &["XAI_API_KEY", "GROK_CLI_CHAT_PROXY_BASE_URL"];

/// The Grok binary: [`GROK_BIN_ENV`] if set, else `grok` on `PATH` (or `~/.grok/bin/grok`, the
/// standalone installer's, or Homebrew's) resolved to the native executable.
pub fn find_grok_binary() -> Result<PathBuf, String> {
    find_binary_in(
        std::env::var_os(GROK_BIN_ENV).as_deref(),
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("HOME").as_deref().map(Path::new),
    )
}

/// [`find_grok_binary`] with explicit inputs.
pub fn find_binary_in(
    override_path: Option<&OsStr>,
    path_var: Option<&OsStr>,
    home: Option<&Path>,
) -> Result<PathBuf, String> {
    if let Some(path) = override_path.filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let mut fallbacks: Vec<PathBuf> = home
        .map(|home| home.join(".grok/bin/grok"))
        .into_iter()
        .collect();
    fallbacks.push(PathBuf::from(HOMEBREW_GROK));
    let found = find_program_in("grok", None, path_var, &fallbacks).ok_or_else(|| {
        format!(
            "Grok Build isn't installed (looked on PATH, in ~/.grok/bin and /opt/homebrew/bin): \
             install it (`npm install -g @xai-official/grok`), then run `grok login`, or set \
             {GROK_BIN_ENV}"
        )
    })?;
    native_binary(&found)
}

/// The native executable behind `path`: `path` itself (symlinks resolved) if it is one, else a
/// `grok-native` next to the resolved file or next to `path`.
pub fn native_binary(path: &Path) -> Result<PathBuf, String> {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if is_native(&resolved) {
        return Ok(resolved);
    }
    let candidates = [resolved.parent(), path.parent()]
        .into_iter()
        .flatten()
        .map(|dir| dir.join(NATIVE_NAME));
    for candidate in candidates {
        let candidate = std::fs::canonicalize(&candidate).unwrap_or(candidate);
        if is_executable(&candidate) && is_native(&candidate) {
            return Ok(candidate);
        }
    }
    Err(format!(
        "{} is not Grok Build's native binary (a Node launcher?) and no {NATIVE_NAME} is next to \
         it; set {GROK_BIN_ENV} to the native binary (rozu never runs it through node)",
        resolved.display()
    ))
}

/// Whether `path` starts like a Mach-O or ELF executable (not a script).
pub fn is_native(path: &Path) -> bool {
    let mut magic = [0u8; 4];
    let ok = std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .is_ok();
    ok && matches!(
        magic,
        [0x7f, b'E', b'L', b'F']
            | [0xcf, 0xfa, 0xed, 0xfe]
            | [0xce, 0xfa, 0xed, 0xfe]
            | [0xfe, 0xed, 0xfa, 0xcf]
            | [0xfe, 0xed, 0xfa, 0xce]
            | [0xca, 0xfe, 0xba, 0xbe]
    )
}

/// The version `grok --version` reports, e.g. `1.0.52` from `grok 1.0.52 (0920c8169e27)`.
pub async fn grok_version(binary: &Path) -> io::Result<String> {
    let out = query(binary, &["--version"]).await?;
    parse_version(&out)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no version in the output"))
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::*;

    fn write_exec(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn resolves_symlinks_to_the_native_binary() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        // npm layout: bin/grok → lib/node_modules/@xai-official/grok/bin/grok-bootstrap.js,
        // with grok-native next to it.
        let pkg = root.join("lib/node_modules/@xai-official/grok/bin");
        write_exec(
            &pkg.join("grok-bootstrap.js"),
            b"#!/usr/bin/env node\nrequire('./x')\n",
        );
        write_exec(&pkg.join(NATIVE_NAME), b"\xcf\xfa\xed\xfe rest of a Mach-O");
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        symlink(pkg.join("grok-bootstrap.js"), bin.join("grok")).unwrap();
        let path = std::env::join_paths([&bin]).unwrap();
        assert_eq!(
            find_binary_in(None, Some(&path), None),
            Ok(pkg.join(NATIVE_NAME))
        );

        // A symlink straight to the native binary (postinstall swapped it in).
        std::fs::remove_file(bin.join("grok")).unwrap();
        symlink(pkg.join(NATIVE_NAME), bin.join("grok")).unwrap();
        assert_eq!(
            find_binary_in(None, Some(&path), None),
            Ok(pkg.join(NATIVE_NAME))
        );

        // A Node launcher with no native binary next to it is refused.
        std::fs::remove_file(pkg.join(NATIVE_NAME)).unwrap();
        std::fs::remove_file(bin.join("grok")).unwrap();
        symlink(pkg.join("grok-bootstrap.js"), bin.join("grok")).unwrap();
        let err = find_binary_in(None, Some(&path), None).unwrap_err();
        assert!(err.contains(GROK_BIN_ENV) && err.contains("node"), "{err}");

        // The override is used as given.
        assert_eq!(
            find_binary_in(Some(OsStr::new("/x/grok")), Some(&path), None),
            Ok(PathBuf::from("/x/grok"))
        );
    }

    #[test]
    fn falls_back_to_the_standalone_install() {
        let dir = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(dir.path()).unwrap();
        let installed = home.join(".grok/bin/grok");
        write_exec(&installed, b"\x7fELF and more");
        let empty = std::env::join_paths([home.join("nothing")]).unwrap();
        assert_eq!(
            find_binary_in(None, Some(&empty), Some(&home)),
            Ok(installed)
        );
        let missing = tempfile::tempdir().unwrap();
        if !Path::new(HOMEBREW_GROK).exists() {
            let err = find_binary_in(None, Some(&empty), Some(missing.path())).unwrap_err();
            assert!(err.contains("isn't installed"), "{err}");
        }
    }

    #[test]
    fn recognizes_native_executables() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("s");
        write_exec(&script, b"#!/bin/sh\n");
        assert!(!is_native(&script));
        let elf = dir.path().join("e");
        write_exec(&elf, b"\x7fELF");
        assert!(is_native(&elf));
        assert!(!is_native(&dir.path().join("missing")));
    }
}
