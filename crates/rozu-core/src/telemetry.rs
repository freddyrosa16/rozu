//! `tracing` setup with secret redaction. Logs go to a file, not the terminal (see
//! [`init_tracing`]).
//!
//! Keys are kept out of logs by `secrecy::SecretString` in `rozu-secrets`; this module is the
//! second layer. Every formatted log line passes through [`redact`] before it reaches the output,
//! so anything that looks like a provider API key is masked even if it slipped into a message or
//! field.

use std::borrow::Cow;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use regex::Regex;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

/// Keeps the provider prefix (capture group 1) so logs stay useful, and masks the rest.
const REPLACEMENT: &str = "${1}[REDACTED]";

/// OpenAI (`sk-…`, `sk-proj-…`), Anthropic (`sk-ant-…`) and xAI (`xai-…`) style keys.
/// Real keys are long; the 16-character minimum avoids masking ordinary words like `sk-learn`.
static KEY_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(sk-ant-|sk-|xai-)[A-Za-z0-9_-]{16,}").expect("valid key regex")
});

/// Masks anything in `input` that looks like a provider API key, keeping only its prefix.
///
/// Returns the input unchanged (borrowed) when nothing matches.
pub fn redact(input: &str) -> Cow<'_, str> {
    KEY_PATTERN.replace_all(input, REPLACEMENT)
}

/// Set to `1` to log to stderr instead of the log file.
pub const LOG_STDERR_ENV: &str = "ROZU_LOG_STDERR";
/// Overrides the log file path (tests, or to keep logs elsewhere).
pub const LOG_FILE_ENV: &str = "ROZU_LOG_FILE";

/// Where logs go by default: `~/Library/Logs/rozu/rozu.log` on macOS, `rozu.log` in the data
/// directory elsewhere (e.g. `~/.local/share/rozu/rozu.log`). `None` if there is no home dir.
pub fn default_log_path() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        let home = directories::BaseDirs::new()?.home_dir().to_owned();
        Some(home.join("Library/Logs/rozu/rozu.log"))
    } else {
        let dirs = directories::ProjectDirs::from("dev", "rozu", "rozu")?;
        Some(dirs.data_local_dir().join("rozu.log"))
    }
}

/// Where [`init_tracing`] sends log lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogTarget {
    File(PathBuf),
    Stderr,
    /// No usable log file; logs are dropped.
    Off,
}

/// Picks the log target from `ROZU_LOG_STDERR` / `ROZU_LOG_FILE`, given an env lookup.
fn log_target(env: impl Fn(&str) -> Option<String>) -> LogTarget {
    if env(LOG_STDERR_ENV).is_some_and(|v| matches!(v.trim(), "1" | "true" | "yes")) {
        return LogTarget::Stderr;
    }
    env(LOG_FILE_ENV)
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
        .or_else(default_log_path)
        .map_or(LogTarget::Off, LogTarget::File)
}

/// Opens `path` for appending, creating it (owner-only on Unix) and its directory.
fn open_log_file(path: &Path) -> io::Result<File> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

/// Installs the global `tracing` subscriber: a `fmt` layer writing through the redaction writer,
/// filtered by `RUST_LOG` (default `info`, which includes the `rozu::approval` decision log; the
/// structured record of every decision is the audit log, see [`crate::audit`]).
///
/// Logs go to a file ([`default_log_path`], or `ROZU_LOG_FILE`), never to the terminal, so
/// stderr stays for user-facing messages; `ROZU_LOG_STDERR=1` sends them to stderr instead. If
/// the file can't be opened, one warning is printed and logging is off. Returns where logs go.
///
/// Safe to call more than once; later calls only report the target.
pub fn init_tracing() -> LogTarget {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    match log_target(|var| std::env::var(var).ok()) {
        LogTarget::Stderr => {
            let _ = builder
                .with_writer(RedactingMakeWriter::new(io::stderr))
                .try_init();
            LogTarget::Stderr
        }
        LogTarget::File(path) => match open_log_file(&path) {
            Ok(file) => {
                let _ = builder
                    .with_ansi(false)
                    .with_writer(RedactingMakeWriter::new(Mutex::new(file)))
                    .try_init();
                LogTarget::File(path)
            }
            Err(err) => {
                eprintln!(
                    "warning: cannot open the log file {}: {err}; logging is off",
                    path.display()
                );
                LogTarget::Off
            }
        },
        LogTarget::Off => LogTarget::Off,
    }
}

/// A [`MakeWriter`] that redacts API keys from everything written through it.
#[derive(Debug, Clone)]
pub struct RedactingMakeWriter<M> {
    inner: M,
}

impl<M> RedactingMakeWriter<M> {
    pub fn new(inner: M) -> Self {
        Self { inner }
    }
}

impl<'a, M: MakeWriter<'a>> MakeWriter<'a> for RedactingMakeWriter<M> {
    type Writer = RedactingWriter<M::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingWriter::new(self.inner.make_writer())
    }
}

/// Buffers one log event, then redacts it and forwards it to `inner` on flush or drop.
///
/// Buffering the whole event means a key split across several `write` calls is still caught.
#[derive(Debug)]
pub struct RedactingWriter<W: Write> {
    inner: W,
    buf: Vec<u8>,
}

impl<W: Write> RedactingWriter<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            buf: Vec::new(),
        }
    }

    fn write_buffered(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let text = String::from_utf8_lossy(&self.buf);
        self.inner.write_all(redact(&text).as_bytes())?;
        self.buf.clear();
        Ok(())
    }
}

impl<W: Write> Write for RedactingWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.write_buffered()?;
        self.inner.flush()
    }
}

impl<W: Write> Drop for RedactingWriter<W> {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn masks_api_keys() {
        for (input, expected) in [
            ("key=sk-abcdefghijklmnop1234 done", "key=sk-[REDACTED] done"),
            ("sk-proj-AbC_dEf-1234567890xyz", "sk-[REDACTED]"),
            (
                "Bearer sk-ant-api03-AbCdEfGhIjKlMnOp_qrs-tuv",
                "Bearer sk-ant-[REDACTED]",
            ),
            (
                "XAI_API_KEY=xai-0123456789abcdefABCDEF",
                "XAI_API_KEY=xai-[REDACTED]",
            ),
            (
                "a=sk-aaaaaaaaaaaaaaaaaaaa b=xai-bbbbbbbbbbbbbbbbbbbb",
                "a=sk-[REDACTED] b=xai-[REDACTED]",
            ),
        ] {
            assert_eq!(redact(input), expected);
        }
    }

    #[test]
    fn leaves_ordinary_text_alone() {
        for text in [
            "plain log line",
            "uses sk-learn for models",
            "task-abcdefghijklmnopqrstuvwxyz",
            "sk-short",
        ] {
            assert!(matches!(redact(text), Cow::Borrowed(_)), "changed: {text}");
        }
    }

    #[test]
    fn writer_redacts_key_split_across_writes() {
        let mut out = Vec::new();
        {
            let mut writer = RedactingWriter::new(&mut out);
            writer.write_all(b"token sk-ant-abcdefgh").unwrap();
            writer.write_all(b"ijklmnopqrstuvwx end\n").unwrap();
        }
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "token sk-ant-[REDACTED] end\n"
        );
    }

    #[derive(Clone, Default)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedBuf {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn log_target_defaults_to_a_file() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |var: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == var)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        match log_target(env(&[])) {
            LogTarget::File(path) => {
                assert!(path.ends_with("rozu.log"), "{}", path.display());
                if cfg!(target_os = "macos") {
                    assert!(path.ends_with("Library/Logs/rozu/rozu.log"));
                }
            }
            other => panic!("expected a file, got {other:?}"),
        }
        assert_eq!(
            log_target(env(&[("ROZU_LOG_FILE", "/tmp/x.log")])),
            LogTarget::File("/tmp/x.log".into())
        );
        assert_eq!(
            log_target(env(&[
                ("ROZU_LOG_STDERR", "1"),
                ("ROZU_LOG_FILE", "/tmp/x.log")
            ])),
            LogTarget::Stderr
        );
        assert!(matches!(
            log_target(env(&[("ROZU_LOG_STDERR", "0")])),
            LogTarget::File(_)
        ));
    }

    #[test]
    fn log_file_is_created_with_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/logs/rozu.log");
        let mut file = open_log_file(&path).unwrap();
        writeln!(file, "one").unwrap();
        drop(file);
        writeln!(open_log_file(&path).unwrap(), "two").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "one\ntwo\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn tracing_output_is_redacted() {
        let buf = SharedBuf::default();
        let sink = buf.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(RedactingMakeWriter::new(move || sink.clone()))
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(api_key = "sk-0123456789abcdefghij", "calling provider");
        });

        let logged = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(logged.contains("calling provider"), "{logged}");
        assert!(logged.contains("sk-[REDACTED]"), "{logged}");
        assert!(!logged.contains("0123456789abcdefghij"), "{logged}");
    }
}
