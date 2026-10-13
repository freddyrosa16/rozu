//! LLM provider layer for rozu.
//!
//! Implements the `rozu_core::Provider` trait over the shared message / tool-schema model, with an
//! adapter per provider: OpenAI (`/v1/responses` SSE), Anthropic (Messages SSE with prompt caching)
//! and xAI (the OpenAI adapter pointed at `https://api.x.ai/v1`). Owns SSE parsing, partial
//! tool-call JSON accumulation, retries with backoff and typed errors.
//!
//! M1 ships the OpenAI adapter.

pub mod openai;
mod retry;

pub use openai::{OpenAiConfig, OpenAiProvider};
pub use retry::RetryPolicy;

/// Version of this crate.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_semver() {
        assert_eq!(VERSION.split('.').count(), 3);
    }
}
