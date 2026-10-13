//! LLM provider layer for rozu.
//!
//! Implements the `rozu_core::Provider` trait over the shared message / tool-schema model, with an
//! adapter per provider: OpenAI (`/v1/responses` SSE), Anthropic (Messages SSE with prompt caching)
//! and xAI (the OpenAI adapter pointed at `https://api.x.ai/v1`). Owns SSE parsing, partial
//! tool-call JSON accumulation, retries with backoff and typed errors, plus the [`registry`] of
//! providers and models.

pub mod anthropic;
pub mod openai;
pub mod registry;
mod retry;
mod sse;
pub mod xai;

pub use anthropic::{AnthropicConfig, AnthropicProvider};
pub use openai::{OpenAiConfig, OpenAiProvider};
pub use registry::{ModelChoice, PROVIDERS, ProviderInfo, RegistryError};
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
