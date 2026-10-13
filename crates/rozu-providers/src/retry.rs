//! Retry policy for transient HTTP failures (429 and 5xx).

use std::time::Duration;

use reqwest::header::HeaderMap;

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
        let delay = header("retry-after-ms")
            .map(|ms| Duration::from_secs_f64(ms / 1000.0))
            .or_else(|| header("retry-after").map(Duration::from_secs_f64))?;
        Some(delay.min(self.max_delay))
    }
}

#[cfg(test)]
mod tests {
    use reqwest::header::HeaderValue;

    use super::*;

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
}
