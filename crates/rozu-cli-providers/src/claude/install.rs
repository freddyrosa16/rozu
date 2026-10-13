//! Finding the `claude` binary and asking it about itself (`--version`, `auth status`).
//!
//! rozu never looks at Claude Code's credential files; whether the CLI is logged in comes only
//! from `claude auth status`, of whose JSON output only `loggedIn` and `authMethod` are read.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::process::Command;

/// Overrides where rozu looks for the `claude` binary.
pub const CLAUDE_BIN_ENV: &str = "ROZU_CLAUDE_BIN";
/// How long `claude --version` / `claude auth status` may take.
const QUERY_TIMEOUT: Duration = Duration::from_secs(15);

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
    if let Some(path) = override_path.filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }
    path_var
        .into_iter()
        .flat_map(std::env::split_paths)
        .map(|dir| dir.join("claude"))
        .chain(home.map(|home| home.join(".local/bin/claude")))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
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

/// Runs `binary args…` and returns its stdout (stderr is discarded).
async fn query(binary: &Path, args: &[&str]) -> io::Result<String> {
    let output = tokio::time::timeout(
        QUERY_TIMEOUT,
        Command::new(binary)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "timed out"))??;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The version `claude --version` reports, e.g. `2.1.3`.
pub async fn claude_version(binary: &Path) -> io::Result<String> {
    let out = query(binary, &["--version"]).await?;
    parse_version(&out)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no version in the output"))
}

/// The first `x.y.z`-looking word of `--version` output.
pub fn parse_version(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .find(|word| {
            let parts: Vec<_> = word.split('.').collect();
            parts.len() >= 2
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        })
        .map(str::to_owned)
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

    #[test]
    fn finds_binary_on_path_then_local_bin() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
        let local = home.join(".local/bin/claude");
        std::fs::write(&local, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = std::env::join_paths([&bin]).unwrap();
        assert_eq!(
            find_binary_in(None, Some(&path), Some(&home)),
            Some(local.clone())
        );
        let on_path = bin.join("claude");
        std::fs::write(&on_path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&on_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
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
