//! `tracing` setup with secret redaction.
//!
//! Keys are kept out of logs by `secrecy::SecretString` in `rozu-secrets`; this module is the
//! second layer. Every formatted log line passes through [`redact`] before it reaches the output,
//! so anything that looks like a provider API key is masked even if it slipped into a message or
//! field.

use std::borrow::Cow;
use std::io::{self, Write};
use std::sync::LazyLock;

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

/// Installs the global `tracing` subscriber: a `fmt` layer writing to stderr through the
/// redaction writer, filtered by `RUST_LOG` (default `info`).
///
/// Safe to call more than once; later calls are no-ops.
pub fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(RedactingMakeWriter::new(io::stderr))
        .try_init();
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
    fn masks_openai_key() {
        assert_eq!(
            redact("key=sk-abcdefghijklmnop1234 done"),
            "key=sk-[REDACTED] done"
        );
    }

    #[test]
    fn masks_openai_project_key() {
        assert_eq!(redact("sk-proj-AbC_dEf-1234567890xyz"), "sk-[REDACTED]");
    }

    #[test]
    fn masks_anthropic_key_keeping_prefix() {
        assert_eq!(
            redact("Bearer sk-ant-api03-AbCdEfGhIjKlMnOp_qrs-tuv"),
            "Bearer sk-ant-[REDACTED]"
        );
    }

    #[test]
    fn masks_xai_key() {
        assert_eq!(
            redact("XAI_API_KEY=xai-0123456789abcdefABCDEF"),
            "XAI_API_KEY=xai-[REDACTED]"
        );
    }

    #[test]
    fn masks_multiple_keys() {
        assert_eq!(
            redact("a=sk-aaaaaaaaaaaaaaaaaaaa b=xai-bbbbbbbbbbbbbbbbbbbb"),
            "a=sk-[REDACTED] b=xai-[REDACTED]"
        );
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
