//! Credential storage for rozu.
//!
//! Stores provider API keys in the OS keychain (`keyring`, service `dev.rozu`) and never in files
//! or logs; keys are wrapped in `secrecy::SecretString`. Also owns the official Sign in with
//! ChatGPT flow (OAuth 2.0 PKCE with a loopback redirect) and token refresh.

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
