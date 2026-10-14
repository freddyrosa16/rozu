//! Retries for transient HTTP failures (429, 5xx including Anthropic's 529, connect errors and
//! timeouts), shared by every adapter, plus the mapping of failed responses to typed errors.

use std::time::Duration;

use reqwest::header::HeaderMap;
use reqwest::{RequestBuilder, Response, StatusCode};
use rozu_core::{ProviderError, ProviderResult};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tracing::warn;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Retries after the first attempt (so `3` means up to 4 requests).
    pub max_retries: u32,
    /// Delay before the first retry; doubles each time.
    pub base_delay: Duration,
    /// Upper bound for any single delay, including a server's `retry-after`.
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
        }
    }
}

impl RetryPolicy {
    /// Exponential backoff for retry number `attempt` (0-based) with "equal jitter": a random
    /// delay between half and all of the exponential delay, so clients don't retry in lockstep.
    pub fn backoff(&self, attempt: u32) -> Duration {
        let exp = self
            .base_delay
            .saturating_mul(2u32.saturating_pow(attempt))
            .min(self.max_delay);
        let half = exp / 2;
        let jitter_ms = fastrand::u64(0..=half.as_millis() as u64);
        half + Duration::from_millis(jitter_ms)
    }

    /// The server-requested delay from `retry-after-ms` or `retry-after` (seconds), capped at
    /// `max_delay`. HTTP-date values are ignored in favor of the backoff.
    pub fn retry_after(&self, headers: &HeaderMap) -> Option<Duration> {
        let header = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v >= 0.0)
        };
        let seconds = header("retry-after-ms")
            .map(|ms| ms / 1000.0)
            .or_else(|| header("retry-after"))?;
        Some(
            Duration::try_from_secs_f64(seconds)
                .unwrap_or(self.max_delay)
                .min(self.max_delay),
        )
    }
}

fn is_retryable(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

/// Sends the request `build` makes (a fresh one per attempt), retrying per `policy`. Returns the
/// first successful response; failures become typed [`ProviderError`]s. `provider` only labels
/// log lines.
pub(crate) async fn send_with_retry(
    policy: RetryPolicy,
    provider: &str,
    cancel: &CancellationToken,
    build: impl Fn() -> ProviderResult<RequestBuilder>,
) -> ProviderResult<Response> {
    let mut attempt = 0;
    loop {
        let request = build()?.send();
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
            result = request => result,
        };

        let delay = match result {
            Ok(response) if response.status().is_success() => return Ok(response),
            Ok(response) => {
                let status = response.status();
                if !is_retryable(status) || attempt >= policy.max_retries {
                    return Err(error_from_response(response, attempt + 1).await);
                }
                let delay = policy
                    .retry_after(response.headers())
                    .unwrap_or_else(|| policy.backoff(attempt));
                warn!(provider, %status, attempt = attempt + 1, ?delay, "retrying request");
                delay
            }
            Err(err) if (err.is_connect() || err.is_timeout()) && attempt < policy.max_retries => {
                let delay = policy.backoff(attempt);
                warn!(provider, error = %err, attempt = attempt + 1, ?delay, "retrying request");
                delay
            }
            Err(err) => return Err(ProviderError::Network(err.to_string())),
        };
        attempt += 1;
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
            _ = tokio::time::sleep(delay) => {}
        }
    }
}

/// The typed error for a failed response. OpenAI, xAI and Anthropic all put the human-readable
/// message at `error.message`; otherwise the raw body (or the status text) is used.
async fn error_from_response(response: Response, attempts: u32) -> ProviderError {
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    let message = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
        .unwrap_or_else(|| {
            if text.is_empty() {
                status
                    .canonical_reason()
                    .unwrap_or("request failed")
                    .to_owned()
            } else {
                text
            }
        });
    ProviderError::from_status(status.as_u16(), attempts, message)
}

#[cfg(test)]
mod tests {
    use reqwest::header::HeaderValue;

    use super::*;

    #[test]
    fn retries_rate_limits_and_server_errors_only() {
        assert!(is_retryable(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable(StatusCode::SERVICE_UNAVAILABLE));
        assert!(is_retryable(StatusCode::from_u16(529).unwrap()));
        assert!(!is_retryable(StatusCode::BAD_REQUEST));
        assert!(!is_retryable(StatusCode::UNAUTHORIZED));
    }

    #[test]
    fn backoff_grows_and_is_capped() {
        let policy = RetryPolicy {
            max_retries: 3,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(1000),
        };
        for _ in 0..50 {
            let first = policy.backoff(0);
            assert!((50..=100).contains(&first.as_millis()), "{first:?}");
            let third = policy.backoff(2);
            assert!((200..=400).contains(&third.as_millis()), "{third:?}");
            let capped = policy.backoff(20);
            assert!((500..=1000).contains(&capped.as_millis()), "{capped:?}");
        }
    }

    #[test]
    fn parses_retry_after_headers() {
        let policy = RetryPolicy::default();
        let mut headers = HeaderMap::new();
        assert_eq!(policy.retry_after(&headers), None);

        headers.insert("retry-after", HeaderValue::from_static("2"));
        assert_eq!(policy.retry_after(&headers), Some(Duration::from_secs(2)));

        headers.insert("retry-after-ms", HeaderValue::from_static("250"));
        assert_eq!(
            policy.retry_after(&headers),
            Some(Duration::from_millis(250))
        );

        headers.clear();
        headers.insert("retry-after", HeaderValue::from_static("3600"));
        assert_eq!(policy.retry_after(&headers), Some(policy.max_delay));

        headers.insert(
            "retry-after",
            HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"),
        );
        assert_eq!(policy.retry_after(&headers), None);
    }

    #[test]
    fn retry_after_caps_values_larger_than_a_duration() {
        let policy = RetryPolicy::default();
        for name in ["retry-after", "retry-after-ms"] {
            for value in ["1e300", "184467440737095516160000"] {
                let mut headers = HeaderMap::new();
                headers.insert(name, HeaderValue::from_str(value).unwrap());
                assert_eq!(policy.retry_after(&headers), Some(policy.max_delay));
            }
        }
    }
}
