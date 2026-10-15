//! Finding the `claude` binary and asking it about itself (`--version`, `auth status`).
//!
//! rozu never looks at Claude Code's credential files; whether the CLI is logged in comes only
//! from `claude auth status`, of whose JSON output only `loggedIn` and `authMethod` are read.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;

pub use crate::probe::parse_version;
use crate::probe::{query, version};
use crate::which::find_program_in;

/// Overrides where rozu looks for the `claude` binary.
pub const CLAUDE_BIN_ENV: &str = "ROZU_CLAUDE_BIN";

/// The `claude` binary: [`CLAUDE_BIN_ENV`] if set, else `claude` on `PATH`, else
/// `~/.local/bin/claude` (the native installer's location).
pub fn find_claude_binary() -> Option<PathBuf> {
    find_binary_in(
        std::env::var_os(CLAUDE_BIN_ENV).as_deref(),
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("HOME").as_deref().map(Path::new),
    )
}

/// [`find_claude_binary`] with explicit inputs.
pub fn find_binary_in(
    override_path: Option<&OsStr>,
    path_var: Option<&OsStr>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    let fallbacks: Vec<PathBuf> = home
        .map(|home| home.join(".local/bin/claude"))
        .into_iter()
        .collect();
    find_program_in("claude", override_path, path_var, &fallbacks)
}

/// The version `claude --version` reports, e.g. `2.1.3`.
pub async fn claude_version(binary: &Path) -> io::Result<String> {
    version(binary).await
}

/// The two fields rozu reads from `claude auth status`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthStatus {
    pub logged_in: Option<bool>,
    pub auth_method: Option<String>,
}

/// Runs `claude auth status` and keeps only `loggedIn` and `authMethod`.
pub async fn claude_auth_status(binary: &Path) -> io::Result<AuthStatus> {
    Ok(parse_auth_status(
        &query(binary, &["auth", "status"]).await?,
    ))
}

/// `loggedIn` / `authMethod` from `auth status` output (a JSON object, possibly among other
/// lines). Every other field is dropped unread.
pub fn parse_auth_status(output: &str) -> AuthStatus {
    let value = serde_json::from_str::<Value>(output.trim())
        .ok()
        .or_else(|| {
            output
                .lines()
                .filter(|line| line.trim_start().starts_with('{'))
                .find_map(|line| serde_json::from_str::<Value>(line).ok())
        })
        .unwrap_or(Value::Null);
    AuthStatus {
        logged_in: value["loggedIn"].as_bool(),
        auth_method: value["authMethod"]
            .as_str()
            .filter(|m| m.len() <= 64)
            .map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_version() {
        assert_eq!(
            parse_version("2.1.3 (Claude Code)\n").as_deref(),
            Some("2.1.3")
        );
        assert_eq!(parse_version("claude 10.0\n").as_deref(), Some("10.0"));
        assert_eq!(parse_version("no version"), None);
    }

    #[test]
    fn auth_status_keeps_only_two_fields() {
        let out = r#"{"loggedIn":true,"authMethod":"claude.ai","email":"someone@example.com","orgId":"x"}"#;
        assert_eq!(
            parse_auth_status(out),
            AuthStatus {
                logged_in: Some(true),
                auth_method: Some("claude.ai".into())
            }
        );
        assert_eq!(
            parse_auth_status("warning: something\n{\"loggedIn\":false}\n").logged_in,
            Some(false)
        );
        assert_eq!(parse_auth_status("not json"), AuthStatus::default());
    }

    fn write_executable(path: &Path) {
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn finds_binary_on_path_then_local_bin() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
        let local = home.join(".local/bin/claude");
        write_executable(&local);
        let path = std::env::join_paths([&bin]).unwrap();
        assert_eq!(find_binary_in(None, Some(&path), Some(&home)), Some(local));
        let on_path = bin.join("claude");
        write_executable(&on_path);
        assert_eq!(
            find_binary_in(None, Some(&path), Some(&home)),
            Some(on_path)
        );
        assert_eq!(
            find_binary_in(Some(OsStr::new("/x/claude")), Some(&path), Some(&home)),
            Some(PathBuf::from("/x/claude"))
        );
        assert_eq!(find_binary_in(None, None, None), None);
    }
}
