//! The [`Provider`] trait every model adapter implements, and its typed errors.

use async_trait::async_trait;
use futures::stream::BoxStream;
use tokio_util::sync::CancellationToken;

use crate::model::{ChatRequest, StreamEvent};

pub type ProviderResult<T> = Result<T, ProviderError>;

/// A stream of normalized events for one model response.
pub type EventStream = BoxStream<'static, ProviderResult<StreamEvent>>;

/// Streams model responses. Implementations live in `rozu-providers`.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Stable provider id, e.g. `"openai"`.
    fn id(&self) -> &str;

    /// Starts one model response. Retries (if any) happen before the stream is returned. The
    /// stream ends after [`StreamEvent::Done`], on the first error, or when `cancel` fires.
    async fn stream(
        &self,
        req: ChatRequest,
        cancel: CancellationToken,
    ) -> ProviderResult<EventStream>;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    #[error("authentication failed (HTTP {status}): {message}")]
    Auth { status: u16, message: String },
    #[error("rate limited (HTTP 429) after {attempts} attempts: {message}")]
    RateLimited { attempts: u32, message: String },
    #[error("HTTP {status}: {message}")]
    Http { status: u16, message: String },
    #[error("provider error{}: {message}", code.as_deref().map(|c| format!(" ({c})")).unwrap_or_default())]
    Api {
        code: Option<String>,
        message: String,
    },
    #[error("network error: {0}")]
    Transport(String),
    #[error("malformed response stream: {0}")]
    Decode(String),
    #[error("response stream ended before the model finished")]
    UnexpectedEof,
    #[error("cancelled")]
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;

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
