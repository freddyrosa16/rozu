//! The [`Provider`] trait every model adapter implements, and its typed errors.

use async_trait::async_trait;
use futures::stream::BoxStream;
use tokio_util::sync::CancellationToken;

use crate::model::{ChatRequest, StreamEvent};

pub type ProviderResult<T> = Result<T, ProviderError>;

/// A stream of normalized events for one model response.
pub type EventStream = BoxStream<'static, ProviderResult<StreamEvent>>;

/// Who runs the tool loop.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProviderKind {
    /// A model API: each stream is one model response, and rozu's agent loop runs the tools.
    #[default]
    Model,
    /// A whole agent (a vendor CLI) that runs its own tool loop. One stream is one user turn:
    /// the provider sends only the newest user message, reaches rozu's tools through its own
    /// channel (MCP), and reports tool calls, [`StreamEvent::ToolResult`]s and
    /// [`StreamEvent::Approval`]s for display. The agent forwards them and executes nothing.
    AgentBackend,
}

/// Streams model responses. Implementations live in `rozu-providers` and `rozu-cli-providers`.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Stable provider id, e.g. `"openai"`.
    fn id(&self) -> &str;

    fn kind(&self) -> ProviderKind {
        ProviderKind::Model
    }

    /// Starts one model response. Retries (if any) happen before the stream is returned. The
    /// stream ends after [`StreamEvent::Done`], on the first error, or when `cancel` fires.
    async fn stream(
        &self,
        req: ChatRequest,
        cancel: CancellationToken,
    ) -> ProviderResult<EventStream>;
}

/// Errors shared by every provider adapter, so callers can react without knowing the provider.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    /// HTTP 401 / 403: missing, wrong or unauthorized API key.
    #[error("authentication failed (HTTP {status}): {message}")]
    Auth { status: u16, message: String },
    /// HTTP 429 that outlasted the retries.
    #[error("rate limited (HTTP 429) after {attempts} attempts: {message}")]
    RateLimited { attempts: u32, message: String },
    /// Any other 4xx: bad parameters, unknown model, prompt too long. Never retried.
    #[error("request rejected (HTTP {status}): {message}")]
    BadRequest { status: u16, message: String },
    /// 5xx (including Anthropic's 529 "overloaded") that outlasted the retries.
    #[error("server error (HTTP {status}) after {attempts} attempts: {message}")]
    Server {
        status: u16,
        attempts: u32,
        message: String,
    },
    /// An error event inside an otherwise successful response stream.
    #[error("provider error{}: {message}", code.as_deref().map(|c| format!(" ({c})")).unwrap_or_default())]
    Api {
        code: Option<String>,
        message: String,
    },
    /// Connection, TLS or timeout failure.
    #[error("network error: {0}")]
    Network(String),
    #[error("malformed response stream: {0}")]
    Decode(String),
    #[error("response stream ended before the model finished")]
    UnexpectedEof,
    #[error("cancelled")]
    Cancelled,
    /// A CLI-backed provider's binary couldn't be found or started.
    #[error("the `{cli}` CLI is not available: {message}")]
    CliNotFound { cli: String, message: String },
    /// A CLI-backed provider's CLI isn't logged in. `hint` says how to log in with the vendor's
    /// own CLI (rozu never handles the credentials).
    #[error("the `{cli}` CLI is not logged in: {hint}")]
    NotLoggedIn { cli: String, hint: String },
    /// A subscription's usage or rate limit was reached (not retried).
    #[error("usage limit reached: {message}")]
    UsageLimit { message: String },
}

impl ProviderError {
    /// The typed error for a failed HTTP response after `attempts` requests.
    pub fn from_status(status: u16, attempts: u32, message: String) -> Self {
        match status {
            401 | 403 => Self::Auth { status, message },
            429 => Self::RateLimited { attempts, message },
            500..=599 => Self::Server {
                status,
                attempts,
                message,
            },
            _ => Self::BadRequest { status, message },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_statuses() {
        let err = |status| ProviderError::from_status(status, 2, "m".into());
        assert!(matches!(err(401), ProviderError::Auth { status: 401, .. }));
        assert!(matches!(err(403), ProviderError::Auth { .. }));
        assert!(matches!(
            err(429),
            ProviderError::RateLimited { attempts: 2, .. }
        ));
        assert!(matches!(err(400), ProviderError::BadRequest { .. }));
        assert!(matches!(err(404), ProviderError::BadRequest { .. }));
        assert!(matches!(
            err(529),
            ProviderError::Server {
                status: 529,
                attempts: 2,
                ..
            }
        ));
    }

    #[test]
    fn api_error_display() {
        let with_code = ProviderError::Api {
            code: Some("server_error".into()),
            message: "boom".into(),
        };
        assert_eq!(with_code.to_string(), "provider error (server_error): boom");
        let without = ProviderError::Api {
            code: None,
            message: "boom".into(),
        };
        assert_eq!(without.to_string(), "provider error: boom");
    }
}
