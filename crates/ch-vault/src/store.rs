use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::{SERVICE, Secret, SecretName, VaultError};

/// Where credentials are kept. Every method blocks, possibly on an OS prompt, so call them off
/// the UI thread.
pub trait SecretStore: Send + Sync {
    /// Stores `value` under `name`, replacing any earlier value.
    fn set(&self, name: &SecretName, value: &str) -> Result<(), VaultError>;

    /// The value under `name`, or `None` if nothing is stored.
    fn get(&self, name: &SecretName) -> Result<Option<Secret>, VaultError>;

    /// Removes the value under `name`. Removing a name that holds nothing succeeds.
    fn delete(&self, name: &SecretName) -> Result<(), VaultError>;
}

/// The OS credential store through the `keyring` crate: Windows Credential Manager, the macOS
/// Keychain, or the Secret Service (GNOME Keyring or `KWallet`) on Linux and the BSDs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyringStore {
    service: String,
}

impl KeyringStore {
    /// A store whose entries use `service` as the keychain service name. The app uses
    /// [`SERVICE`]; tests use a throwaway name.
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    pub fn service(&self) -> &str {
        &self.service
    }

    fn entry(&self, name: &SecretName) -> Result<keyring::Entry, VaultError> {
        keyring::Entry::new(&self.service, name.as_str())
            .map_err(|err| VaultError::from_keyring(name, err))
    }
}

impl Default for KeyringStore {
    fn default() -> Self {
        Self::new(SERVICE)
    }
}

impl SecretStore for KeyringStore {
    fn set(&self, name: &SecretName, value: &str) -> Result<(), VaultError> {
        self.entry(name)?
            .set_password(value)
            .map_err(|err| VaultError::from_keyring(name, err).redact(value))
    }

    fn get(&self, name: &SecretName) -> Result<Option<Secret>, VaultError> {
        match self.entry(name)?.get_password() {
            Ok(value) => Ok(Some(Secret::new(value))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(VaultError::from_keyring(name, err)),
        }
    }

    fn delete(&self, name: &SecretName) -> Result<(), VaultError> {
        match self.entry(name)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(VaultError::from_keyring(name, err)),
        }
    }
}

/// Credentials kept in memory for the life of the value, for tests and for a "do not save"
/// choice in the app.
#[derive(Default)]
pub struct MemoryStore {
    entries: Mutex<BTreeMap<SecretName, String>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn entries(&self) -> MutexGuard<'_, BTreeMap<SecretName, String>> {
        // Every operation leaves the map consistent, so a panic elsewhere does not spoil it.
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl fmt::Debug for MemoryStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryStore")
            .field("names", &self.entries().keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl SecretStore for MemoryStore {
    fn set(&self, name: &SecretName, value: &str) -> Result<(), VaultError> {
        self.entries().insert(name.clone(), value.to_owned());
        Ok(())
    }

    fn get(&self, name: &SecretName) -> Result<Option<Secret>, VaultError> {
        Ok(self
            .entries()
            .get(name)
            .map(|value| Secret::new(value.as_str())))
    }

    fn delete(&self, name: &SecretName) -> Result<(), VaultError> {
        self.entries().remove(name);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAKE: &str = "test-secret-not-real";

    #[test]
    fn memory_store_debug_lists_names_only() {
        let store = MemoryStore::new();
        store.set(&SecretName::github(), FAKE).unwrap();
        let debug = format!("{store:?}");
        assert!(debug.contains("github"));
        assert!(!debug.contains(FAKE));
    }

    #[test]
    fn keyring_store_uses_the_app_service_by_default() {
        assert_eq!(KeyringStore::default().service(), "c-harness");
    }

    // Touches the real OS credential store, so it only runs on request:
    // `cargo test -p ch-vault -- --ignored`.
    #[test]
    #[ignore = "writes to the real OS credential store"]
    fn keyring_round_trip_with_a_throwaway_service() {
        let store = KeyringStore::new("c-harness-test-throwaway");
        let name = SecretName::model("ch-vault-test").unwrap();
        store.delete(&name).unwrap();
        assert!(store.get(&name).unwrap().is_none());

        store.set(&name, FAKE).unwrap();
        let read = store.get(&name).unwrap();
        store.delete(&name).unwrap();
        assert_eq!(read.as_ref().map(Secret::expose), Some(FAKE));
        assert!(store.get(&name).unwrap().is_none());
        store.delete(&name).unwrap();
    }
}
