//! Finding the `codex` binary and asking it about itself: `--version`, `codex login status`
//! and the app-server's model list.
//!
//! rozu never looks at Codex's credential files (`~/.codex/auth.json`). Login state comes only
//! from `codex login status`, whose output is matched against three phrases and never shown, or
//! from the app-server's `account/read` (of which only the account type is read).

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::process::Command;
use tracing::debug;

use super::protocol::{self, Incoming};
use crate::claude::install::{parse_version, query};
use crate::process::{CliCommand, CliProcess};
use crate::which::find_program_in;

/// Overrides where rozu looks for the `codex` binary.
pub const CODEX_BIN_ENV: &str = "ROZU_CODEX_BIN";
/// Homebrew's location on Apple silicon, checked after `PATH`.
pub const HOMEBREW_CODEX: &str = "/opt/homebrew/bin/codex";
/// How long `codex login status` / the model list may take.
const QUERY_TIMEOUT: Duration = Duration::from_secs(15);

/// The `codex` binary: [`CODEX_BIN_ENV`] if set, else `codex` on `PATH`, else
/// [`HOMEBREW_CODEX`].
pub fn find_codex_binary() -> Option<PathBuf> {
    find_program_in(
        "codex",
        std::env::var_os(CODEX_BIN_ENV).as_deref(),
        std::env::var_os("PATH").as_deref(),
        &[PathBuf::from(HOMEBREW_CODEX)],
    )
}

/// The version `codex --version` reports, e.g. `0.147.0` from `codex-cli 0.147.0`.
pub async fn codex_version(binary: &Path) -> io::Result<String> {
    let out = query(binary, &["--version"]).await?;
    parse_version(&out)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no version in the output"))
}

/// What `codex login status` says, reduced to the auth mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginStatus {
    ChatGpt,
    ApiKey,
    NotLoggedIn,
    Unknown,
}

/// Runs `codex login status`. Its output (stdout and stderr: Codex prints the status on stderr)
/// is only matched, never returned: with an API key it includes a masked key.
pub async fn codex_login_status(binary: &Path) -> io::Result<LoginStatus> {
    let output = tokio::time::timeout(
        QUERY_TIMEOUT,
        Command::new(binary)
            .args(["login", "status"])
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "timed out"))??;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(parse_login_status(&text))
}

/// The auth mode in `codex login status` output.
pub fn parse_login_status(output: &str) -> LoginStatus {
    let lower = output.to_lowercase();
    if lower.contains("not logged in") {
        LoginStatus::NotLoggedIn
    } else if lower.contains("logged in using chatgpt") {
        LoginStatus::ChatGpt
    } else if lower.contains("logged in using an api key") || lower.contains("api key") {
        LoginStatus::ApiKey
    } else {
        LoginStatus::Unknown
    }
}

/// A model from the app-server's `model/list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexModel {
    /// What `thread/start` / `turn/start` take.
    pub id: String,
    pub display_name: String,
    pub is_default: bool,
}

/// Asks `codex app-server` for its model list (`initialize`, `initialized`, `model/list`),
/// then stops it. Hidden models are left out.
pub async fn list_models(binary: &Path) -> io::Result<Vec<CodexModel>> {
    let mut cmd = CliCommand::new("codex", binary);
    cmd.args = vec!["app-server".into()];
    cmd.env_remove = super::ENV_REMOVE.iter().map(Into::into).collect();
    let mut process = CliProcess::spawn(&cmd)?;
    let result = tokio::time::timeout(QUERY_TIMEOUT, async {
        rpc(&mut process, 1, "initialize", protocol::initialize_params()).await?;
        process
            .send_json(&protocol::notification("initialized"))
            .await?;
        let mut models = Vec::new();
        let mut cursor = Value::Null;
        for page in 0..10u64 {
            let result = rpc(
                &mut process,
                2 + page,
                "model/list",
                json!({ "cursor": cursor, "includeHidden": false }),
            )
            .await?;
            models.extend(
                result["data"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|m| m["hidden"].as_bool() != Some(true))
                    .filter_map(|m| {
                        let id = m["model"].as_str().or_else(|| m["id"].as_str())?;
                        Some(CodexModel {
                            id: id.to_owned(),
                            display_name: m["displayName"].as_str().unwrap_or(id).to_owned(),
                            is_default: m["isDefault"].as_bool() == Some(true),
                        })
                    }),
            );
            cursor = result["nextCursor"].clone();
            if cursor.is_null() {
                break;
            }
        }
        Ok(models)
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "codex app-server timed out"))?;
    process.kill();
    result
}

/// Sends request `id` and waits for its response, refusing any server request meanwhile.
async fn rpc(process: &mut CliProcess, id: u64, method: &str, params: Value) -> io::Result<Value> {
    process
        .send_json(&protocol::request(id, method, params))
        .await?;
    loop {
        let message = process
            .next_json()
            .await
            .map_err(io::Error::other)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "codex exited"))?;
        match Incoming::parse(message) {
            Some(Incoming::Response { id: got, result }) if got == json!(id) => {
                return result.map_err(io::Error::other);
            }
            Some(Incoming::Request {
                id: req, method, ..
            }) => {
                debug!(target: "rozu::cli", %method, "refusing a codex request while listing models");
                let reply = protocol::error_response(
                    &req,
                    protocol::METHOD_NOT_FOUND,
                    "rozu is only listing models",
                );
                process.send_json(&reply).await?;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_status_is_reduced_to_the_mode() {
        assert_eq!(
            parse_login_status("Logged in using ChatGPT\n"),
            LoginStatus::ChatGpt
        );
        assert_eq!(
            parse_login_status("Logged in using an API key - sk-proj-***abcd\n"),
            LoginStatus::ApiKey
        );
        assert_eq!(
            parse_login_status("Not logged in\n"),
            LoginStatus::NotLoggedIn
        );
        assert_eq!(parse_login_status("???"), LoginStatus::Unknown);
        assert_eq!(
            parse_version("codex-cli 0.147.0\n").as_deref(),
            Some("0.147.0")
        );
    }
}
