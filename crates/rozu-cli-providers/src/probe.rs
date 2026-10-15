//! Shared stdout-only queries for the official vendor CLIs.

use std::io;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

const QUERY_TIMEOUT: Duration = Duration::from_secs(15);

/// Returns stdout even on a nonzero exit: status commands can report a logged-out state that way.
pub(crate) async fn query(binary: &Path, args: &[&str]) -> io::Result<String> {
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

pub(crate) async fn version(binary: &Path) -> io::Result<String> {
    let out = query(binary, &["--version"]).await?;
    parse_version(&out)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no version in the output"))
}

/// The first dot-separated numeric version in whitespace-separated `--version` output.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parser_preserves_numeric_word_rules() {
        for (output, expected) in [
            ("claude 10.0\n", Some("10.0")),
            ("codex-cli 0.147.0\n", Some("0.147.0")),
            ("grok 1.0.52 (0920c8169e27)", Some("1.0.52")),
            ("1.2.3-beta 4.5 6.7.8", Some("4.5")),
            ("1 1. .1 1..2 v1.2 (1.2) １.２", None),
            ("no version", None),
        ] {
            assert_eq!(parse_version(output).as_deref(), expected, "{output}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn query_keeps_stdout_from_nonzero_exit_and_discards_stderr() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("probe");
        std::fs::write(
            &binary,
            r#"#!/bin/sh
printf 'stderr-only 9.9.9\n' >&2
case "$1" in
  --version) printf 'test-cli 1.2.3\n' ;;
  invalid) printf '\377' ;;
  *) printf '{"loggedIn":false}\n' ;;
esac
exit 1
"#,
        )
        .unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(version(&binary).await.unwrap(), "1.2.3");
        assert_eq!(
            query(&binary, &["auth", "status"]).await.unwrap(),
            "{\"loggedIn\":false}\n"
        );
        assert_eq!(query(&binary, &["invalid"]).await.unwrap(), "\u{fffd}");

        std::fs::write(
            &binary,
            "#!/bin/sh\nprintf 'stderr-only 9.9.9\\n' >&2\nprintf 'no version\\n'\n",
        )
        .unwrap();
        let error = version(&binary).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "no version in the output");
    }
}
