//! Where the sync key is kept: the macOS Keychain, never a plain file or a
//! log. One key per cloud address, so a development server's key never
//! stands in for the real one.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::error::{Result, SyncError};

/// Keeps sync keys.
pub trait TokenStore: Send + Sync {
    /// The key stored under `key`, if any.
    fn load(&self, key: &str) -> Option<String>;
    /// Stores `token` under `key`, replacing what was there.
    fn save(&self, key: &str, token: &str) -> Result<()>;
    /// Forgets the key stored under `key` (nothing to forget is fine).
    fn delete(&self, key: &str) -> Result<()>;
}

/// What a cloud's key is stored under: its address without the scheme
/// (`oyl.stg.spky.cloud`, `localhost:8787`).
pub fn token_key(base: &str) -> String {
    base.split("://")
        .nth(1)
        .unwrap_or(base)
        .trim_end_matches('/')
        .to_string()
}

/// The login Keychain: generic passwords of the service "Oyl Cloud", one
/// account per cloud address.
#[derive(Clone, Copy, Debug, Default)]
pub struct Keychain;

/// The Keychain service the keys are filed under.
#[cfg(target_os = "macos")]
const SERVICE: &str = "Oyl Cloud";

/// `errSecItemNotFound`.
#[cfg(target_os = "macos")]
const NOT_FOUND: i32 = -25300;

#[cfg(target_os = "macos")]
impl TokenStore for Keychain {
    fn load(&self, key: &str) -> Option<String> {
        match security_framework::passwords::get_generic_password(SERVICE, key) {
            Ok(bytes) => String::from_utf8(bytes)
                .ok()
                .filter(|token| !token.is_empty()),
            Err(err) => {
                if err.code() != NOT_FOUND {
                    log::warn!("sync: the Keychain did not give the key: {err}");
                }
                None
            }
        }
    }

    fn save(&self, key: &str, token: &str) -> Result<()> {
        security_framework::passwords::set_generic_password(SERVICE, key, token.as_bytes())
            .map_err(|err| SyncError::Keychain(err.to_string()))
    }

    fn delete(&self, key: &str) -> Result<()> {
        match security_framework::passwords::delete_generic_password(SERVICE, key) {
            Ok(()) => Ok(()),
            Err(err) if err.code() == NOT_FOUND => Ok(()),
            Err(err) => Err(SyncError::Keychain(err.to_string())),
        }
    }
}

#[cfg(not(target_os = "macos"))]
impl TokenStore for Keychain {
    fn load(&self, _key: &str) -> Option<String> {
        None
    }

    fn save(&self, _key: &str, _token: &str) -> Result<()> {
        Err(SyncError::Keychain(
            "there is no Keychain on this system".into(),
        ))
    }

    fn delete(&self, _key: &str) -> Result<()> {
        Ok(())
    }
}

/// Keys kept in memory only (tests, the self-test).
#[derive(Debug, Default)]
pub struct MemoryTokens(Mutex<HashMap<String, String>>);

impl TokenStore for MemoryTokens {
    fn load(&self, key: &str) -> Option<String> {
        self.0.lock().ok()?.get(key).cloned()
    }

    fn save(&self, key: &str, token: &str) -> Result<()> {
        if let Ok(mut tokens) = self.0.lock() {
            tokens.insert(key.to_string(), token.to_string());
        }
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<()> {
        if let Ok(mut tokens) = self.0.lock() {
            tokens.remove(key);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_filed_by_address() {
        assert_eq!(
            token_key("https://oyl.stg.spky.cloud/"),
            "oyl.stg.spky.cloud"
        );
        assert_eq!(token_key("http://localhost:8787"), "localhost:8787");
    }

    #[test]
    fn memory_tokens_keep_and_forget() {
        let store = MemoryTokens::default();
        assert_eq!(store.load("a"), None);
        store.save("a", "oyl_sync_x").unwrap();
        assert_eq!(store.load("a").as_deref(), Some("oyl_sync_x"));
        store.delete("a").unwrap();
        store.delete("a").unwrap();
        assert_eq!(store.load("a"), None);
    }

    /// Writes to the login Keychain (and removes it again): run by hand,
    /// `cargo test -p oyl-sync -- --ignored keychain`.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn the_keychain_keeps_and_forgets_a_key() {
        let key = format!("oyl-sync-test-{}", uuid::Uuid::new_v4().simple());
        let store = Keychain;
        assert_eq!(store.load(&key), None);
        store.save(&key, "oyl_sync_test_value").unwrap();
        assert_eq!(store.load(&key).as_deref(), Some("oyl_sync_test_value"));
        store.save(&key, "oyl_sync_test_other").unwrap();
        assert_eq!(store.load(&key).as_deref(), Some("oyl_sync_test_other"));
        store.delete(&key).unwrap();
        assert_eq!(store.load(&key), None);
        store.delete(&key).unwrap();
    }
}
