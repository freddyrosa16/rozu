//! Credential storage for rozu.
//!
//! Stores provider API keys in the OS keychain (`keyring`, service `dev.rozu`) and never in files
//! or logs; keys are wrapped in `secrecy::SecretString`. Subscription authentication belongs
//! to the official vendor CLIs; rozu never reads their credential files.

use std::collections::HashMap;
use std::sync::Mutex;

use secrecy::{ExposeSecret, SecretString};

/// Version of this crate.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Keychain service name; the account is the provider id (e.g. `openai`).
pub const SERVICE: &str = "dev.rozu";

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("keychain error: {0}")]
    Keychain(String),
    #[error("the API key is empty")]
    Empty,
}

/// Where provider API keys live.
pub trait SecretStore: Send + Sync {
    /// The key for `provider`, if one is stored.
    fn get(&self, provider: &str) -> Result<Option<SecretString>, SecretError>;
    /// Stores (or replaces) the key for `provider`.
    fn set(&self, provider: &str, key: &SecretString) -> Result<(), SecretError>;
    /// Removes the key for `provider`. Returns whether one was stored.
    fn delete(&self, provider: &str) -> Result<bool, SecretError>;
}

/// The OS keychain (macOS Keychain, Windows Credential Manager, Linux kernel keyring).
#[derive(Debug, Clone)]
pub struct KeyringStore {
    service: String,
}

impl Default for KeyringStore {
    fn default() -> Self {
        Self::new(SERVICE)
    }
}

impl KeyringStore {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, provider: &str) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(&self.service, provider).map_err(keychain_error)
    }
}

// Keyring errors describe the failure, never the secret, so they are safe to surface.
fn keychain_error(err: keyring::Error) -> SecretError {
    SecretError::Keychain(err.to_string())
}

impl SecretStore for KeyringStore {
    fn get(&self, provider: &str) -> Result<Option<SecretString>, SecretError> {
        match self.entry(provider)?.get_password() {
            Ok(key) => Ok(Some(SecretString::from(key))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(keychain_error(err)),
        }
    }

    fn set(&self, provider: &str, key: &SecretString) -> Result<(), SecretError> {
        let key = key.expose_secret().trim();
        if key.is_empty() {
            return Err(SecretError::Empty);
        }
        self.entry(provider)?
            .set_password(key)
            .map_err(keychain_error)
    }

    fn delete(&self, provider: &str) -> Result<bool, SecretError> {
        match self.entry(provider)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(err) => Err(keychain_error(err)),
        }
    }
}

/// An in-process store for tests.
#[derive(Debug, Default)]
pub struct MemoryStore {
    keys: Mutex<HashMap<String, SecretString>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, provider: &str) -> Result<Option<SecretString>, SecretError> {
        Ok(self
            .keys
            .lock()
            .expect("memory store lock")
            .get(provider)
            .map(|key| SecretString::from(key.expose_secret().to_owned())))
    }

    fn set(&self, provider: &str, key: &SecretString) -> Result<(), SecretError> {
        let key = key.expose_secret().trim();
        if key.is_empty() {
            return Err(SecretError::Empty);
        }
        self.keys
            .lock()
            .expect("memory store lock")
            .insert(provider.to_owned(), SecretString::from(key.to_owned()));
        Ok(())
    }

    fn delete(&self, provider: &str) -> Result<bool, SecretError> {
        Ok(self
            .keys
            .lock()
            .expect("memory store lock")
            .remove(provider)
            .is_some())
    }
}

/// Environment variable that overrides the stored key for `provider`, if it has one.
pub fn env_var_for(provider: &str) -> Option<&'static str> {
    match provider {
        "openai" => Some("OPENAI_API_KEY"),
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "xai" => Some("XAI_API_KEY"),
        _ => None,
    }
}

/// Where a resolved key came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    Env(&'static str),
    Store,
}

/// The key for `provider`: the provider's env var if set and non-empty, else the store.
pub fn resolve_api_key(
    store: &dyn SecretStore,
    provider: &str,
    env: impl Fn(&str) -> Option<String>,
) -> Result<Option<(SecretString, KeySource)>, SecretError> {
    let from_env = env_var_for(provider).and_then(|var| {
        let value = env(var)?.trim().to_owned();
        (!value.is_empty()).then(|| (SecretString::from(value), KeySource::Env(var)))
    });
    if from_env.is_some() {
        return Ok(from_env);
    }
    Ok(store.get(provider)?.map(|key| (key, KeySource::Store)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(key: &str) -> MemoryStore {
        let store = MemoryStore::new();
        store.set("openai", &SecretString::from(key)).unwrap();
        store
    }

    #[test]
    fn version_is_semver() {
        assert_eq!(VERSION.split('.').count(), 3);
    }

    #[test]
    fn memory_store_round_trip() {
        let store = MemoryStore::new();
        assert!(store.get("openai").unwrap().is_none());
        store
            .set("openai", &SecretString::from(" sk-abc\n"))
            .unwrap();
        assert_eq!(
            store.get("openai").unwrap().unwrap().expose_secret(),
            "sk-abc"
        );
        assert!(store.delete("openai").unwrap());
        assert!(!store.delete("openai").unwrap());
        assert!(store.get("openai").unwrap().is_none());
    }

    #[test]
    fn rejects_empty_keys() {
        let store = MemoryStore::new();
        assert!(matches!(
            store.set("openai", &SecretString::from("  \n")),
            Err(SecretError::Empty)
        ));
    }

    #[test]
    fn env_overrides_store() {
        let store = store_with("sk-store");

        let (key, source) = resolve_api_key(&store, "openai", |var| {
            (var == "OPENAI_API_KEY").then(|| "sk-env".to_owned())
        })
        .unwrap()
        .unwrap();
        assert_eq!(key.expose_secret(), "sk-env");
        assert_eq!(source, KeySource::Env("OPENAI_API_KEY"));

        let (key, source) = resolve_api_key(&store, "openai", |_| Some(" ".into()))
            .unwrap()
            .unwrap();
        assert_eq!(key.expose_secret(), "sk-store");
        assert_eq!(source, KeySource::Store);

        assert!(
            resolve_api_key(&MemoryStore::new(), "openai", |_| None)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn debug_never_shows_keys() {
        let store = store_with("sk-verysecretvalue");
        let debug = format!("{store:?} {:?}", store.get("openai").unwrap());
        assert!(!debug.contains("verysecret"), "{debug}");
    }
}
