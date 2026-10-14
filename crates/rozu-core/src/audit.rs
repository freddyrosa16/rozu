//! The approval audit log: one JSON line per approval decision (automatic or the user's,
//! allow or deny), in a file of its own.
//!
//! Defaults to `~/Library/Logs/rozu/approvals.jsonl` on macOS and `approvals.jsonl` in the data
//! directory elsewhere; `ROZU_AUDIT_LOG` overrides it. The file is created owner-only (0600) and
//! appended to by every rozu process of a session (`rozu chat` and the `rozu mcp-serve` it
//! starts write to the same file; each line is one `write`). Previews and reasons pass through
//! [`redact`] and are cut to [`MAX_TEXT_CHARS`]; the full preview is identified by its SHA-256.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::approval::{ApprovalMode, ApprovalRecord, Decision, DecisionSource};
use crate::telemetry::redact;

/// Overrides the audit log path.
pub const AUDIT_LOG_ENV: &str = "ROZU_AUDIT_LOG";
/// Longest preview / reason kept in an entry.
pub const MAX_TEXT_CHARS: usize = 300;

/// Where the audit log goes by default (see the module docs). `None` without a home dir.
pub fn default_audit_path() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        let home = directories::BaseDirs::new()?.home_dir().to_owned();
        Some(home.join("Library/Logs/rozu/approvals.jsonl"))
    } else {
        let dirs = directories::ProjectDirs::from("dev", "rozu", "rozu")?;
        Some(dirs.data_local_dir().join("approvals.jsonl"))
    }
}

/// `ROZU_AUDIT_LOG`, else [`default_audit_path`].
pub fn audit_path_from_env() -> Option<PathBuf> {
    std::env::var_os(AUDIT_LOG_ENV)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(default_audit_path)
}

/// A new id for one rozu session: UTC start time plus random bits, e.g.
/// `20261013T194012Z-5f2c9a1e`.
pub fn new_session_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(now.as_nanos());
    hasher.write_u32(std::process::id());
    let stamp: String = utc_timestamp(now.as_secs(), 0)
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == 'T')
        .take(15)
        .collect();
    format!("{stamp}Z-{:08x}", hasher.finish() as u32)
}

/// The append-only approval log of one session.
#[derive(Debug)]
pub struct AuditLog {
    path: PathBuf,
    session_id: String,
    file: Mutex<File>,
}

impl AuditLog {
    /// Opens (creating, 0600) `path` and its directory for appending.
    pub fn open(path: impl Into<PathBuf>, session_id: impl Into<String>) -> io::Result<Self> {
        let path = path.into();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = options.open(&path)?;
        // An older file may have been created with looser permissions.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        Ok(Self {
            path,
            session_id: session_id.into(),
            file: Mutex::new(file),
        })
    }

    /// The log at [`audit_path_from_env`]; `None` (with a warning in the log file) if it can't
    /// be opened.
    pub fn from_env(session_id: impl Into<String>) -> Option<Self> {
        let path = audit_path_from_env()?;
        match Self::open(&path, session_id) {
            Ok(log) => Some(log),
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err,
                    "cannot open the approval audit log; decisions are only in rozu.log");
                None
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Appends one entry for `record`, decided in `mode`.
    pub fn record(&self, record: &ApprovalRecord, mode: ApprovalMode) {
        self.record_for(record, mode, None);
    }

    /// Like [`AuditLog::record`], tagging the entry with the CLI-backed provider that asked
    /// (e.g. `codex`), if any.
    pub fn record_for(&self, record: &ApprovalRecord, mode: ApprovalMode, provider: Option<&str>) {
        let mut entry = AuditEntry::new(record, mode, &self.session_id);
        entry.provider = provider;
        let mut line = match serde_json::to_vec(&entry) {
            Ok(line) => line,
            Err(err) => {
                tracing::warn!(error = %err, "could not encode an audit entry");
                return;
            }
        };
        line.push(b'\n');
        let result = self
            .file
            .lock()
            .map_err(|_| io::Error::other("audit log lock poisoned"))
            .and_then(|mut file| file.write_all(&line).and_then(|()| file.flush()));
        if let Err(err) = result {
            tracing::warn!(path = %self.path.display(), error = %err,
                "could not write to the approval audit log");
        }
    }
}

/// One line of the audit log.
#[derive(Debug, Serialize)]
struct AuditEntry<'a> {
    ts: String,
    session_id: &'a str,
    call_id: &'a str,
    tool: &'a str,
    /// The CLI-backed provider whose request this was (`codex`); absent for rozu's own calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<&'a str>,
    /// `allow` or `deny`.
    decision: &'static str,
    /// `approve`, `always_allow`, `approve_unsandboxed` or `deny`.
    kind: &'static str,
    /// `mode` (auto-approved), `rule`, `user`, `policy` (default-deny), `cancelled` or
    /// `delegated-to-rozu-mcp` (a CLI's prompt for rozu's own MCP tool, which `rozu mcp-serve`
    /// then decides and logs itself).
    decided_by: &'static str,
    rule: Option<&'a str>,
    mode: &'static str,
    risk: String,
    /// Whether the call runs under the OS sandbox.
    sandboxed: bool,
    sandbox: String,
    network: bool,
    extra_writes: Vec<String>,
    /// A decision on rerunning a call the sandbox blocked.
    retry_after_sandbox_block: bool,
    /// SHA-256 of the full preview (the command, diff or file contents shown to the user).
    preview_sha256: String,
    /// The preview, redacted and cut to [`MAX_TEXT_CHARS`].
    preview: String,
    reason: Option<String>,
}

impl<'a> AuditEntry<'a> {
    fn new(record: &'a ApprovalRecord, mode: ApprovalMode, session_id: &'a str) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let (kind, reason) = match &record.decision {
            Decision::Approve => ("approve", None),
            Decision::AlwaysAllowPrefix => ("always_allow", None),
            Decision::ApproveUnsandboxed => ("approve_unsandboxed", None),
            Decision::Deny { reason } => ("deny", Some(clean_text(reason))),
        };
        let (decided_by, rule) = match &record.source {
            DecisionSource::Mode(_) => ("mode", None),
            DecisionSource::Rule { prefix } => ("rule", Some(prefix.as_str())),
            DecisionSource::Gate => ("user", None),
            DecisionSource::Cancelled => ("cancelled", None),
            DecisionSource::Policy => ("policy", None),
            DecisionSource::Delegated => ("delegated-to-rozu-mcp", None),
        };
        Self {
            ts: utc_timestamp(now.as_secs(), now.subsec_millis()),
            session_id,
            call_id: &record.call_id,
            tool: &record.tool,
            provider: None,
            decision: if record.decision.is_approval() {
                "allow"
            } else {
                "deny"
            },
            kind,
            decided_by,
            rule,
            mode: match mode {
                ApprovalMode::ReadOnly => "read-only",
                ApprovalMode::AutoWorkspace => "auto",
                ApprovalMode::FullAccess => "full",
            },
            risk: record.risk.to_string(),
            sandboxed: record.sandboxed(),
            sandbox: record.sandbox.to_string(),
            network: record.network,
            extra_writes: record
                .extra_writes
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            retry_after_sandbox_block: record.blocked_retry,
            preview_sha256: sha256_hex(record.preview.as_bytes()),
            preview: clean_text(&record.preview),
            reason,
        }
    }
}

/// Redacted and cut to [`MAX_TEXT_CHARS`].
fn clean_text(text: &str) -> String {
    let redacted = redact(text);
    if redacted.chars().count() <= MAX_TEXT_CHARS {
        redacted.into_owned()
    } else {
        let mut cut: String = redacted.chars().take(MAX_TEXT_CHARS).collect();
        cut.push('…');
        cut
    }
}

/// `2026-10-13T19:40:12.345Z` for Unix time `secs` + `millis`.
fn utc_timestamp(secs: u64, millis: u32) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

/// SHA-256 of `data`, lowercase hex.
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(data))
}

#[cfg(test)]
mod tests {
    use rozu_tools::Risk;

    use super::*;
    use crate::approval::SandboxMode;

    #[test]
    fn sha256_test_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(&[b'a'; 1_000]),
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
        );
    }

    #[test]
    fn timestamps_are_utc_iso8601() {
        assert_eq!(utc_timestamp(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(utc_timestamp(1_791_920_412, 7), "2026-10-13T19:40:12.007Z");
        assert_eq!(utc_timestamp(951_782_400, 0), "2000-02-29T00:00:00.000Z");
        let id = new_session_id();
        assert_eq!(id.len(), "20261013T194012Z-5f2c9a1e".len(), "{id}");
        assert_ne!(new_session_id(), id);
    }

    #[test]
    fn entries_are_redacted_truncated_and_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/approvals.jsonl");
        let log = AuditLog::open(&path, "s-1").unwrap();
        let preview = format!(
            "$ curl -H 'Authorization: Bearer sk-ant-abcdefghijklmnopqrstu' https://x.dev {}",
            "y".repeat(500)
        );
        let record = ApprovalRecord {
            call_id: "c1".into(),
            tool: "shell".into(),
            risk: Risk::Network,
            preview: preview.clone(),
            sandbox: SandboxMode::Enforced("seatbelt"),
            network: true,
            extra_writes: vec![PathBuf::from("/Users/me/x")],
            blocked_retry: false,
            decision: Decision::Deny {
                reason: "not with sk-0123456789abcdefghij".into(),
            },
            source: DecisionSource::Gate,
        };
        log.record(&record, ApprovalMode::AutoWorkspace);

        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(!text.contains("abcdefghijklmnopqrstu") && !text.contains("0123456789abcdefghij"));
        let entry: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(entry["session_id"], "s-1");
        assert!(entry.get("provider").is_none());
        assert_eq!(entry["decision"], "deny");
        assert_eq!(entry["decided_by"], "user");
        assert_eq!(entry["mode"], "auto");
        assert_eq!(entry["sandboxed"], true);
        assert_eq!(entry["extra_writes"][0], "/Users/me/x");
        assert_eq!(entry["preview_sha256"], sha256_hex(preview.as_bytes()));
        assert_eq!(
            entry["preview"].as_str().unwrap().chars().count(),
            MAX_TEXT_CHARS + 1
        );
        assert!(entry["ts"].as_str().unwrap().ends_with('Z'));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        log.record_for(&record, ApprovalMode::AutoWorkspace, Some("codex"));
        let text = fs::read_to_string(&path).unwrap();
        let entry: serde_json::Value = serde_json::from_str(text.lines().nth(1).unwrap()).unwrap();
        assert_eq!(entry["provider"], "codex");
    }

    #[test]
    fn loose_permissions_are_tightened() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("approvals.jsonl");
        fs::write(&path, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            AuditLog::open(&path, "s").unwrap();
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
