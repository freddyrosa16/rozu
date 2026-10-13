//! LLM provider layer for rozu.
//!
//! Defines the `Provider` trait and one internal message / tool-schema model, with an adapter per
//! provider: OpenAI (`/v1/responses` SSE), Anthropic (Messages SSE with prompt caching) and xAI
//! (the OpenAI adapter pointed at `https://api.x.ai/v1`). Owns SSE parsing, partial tool-call JSON
//! accumulation, retries with backoff and typed errors.

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
