//! Credentials in the OS keychain: model provider API keys and the GitHub token.
//!
//! Values never pass through `Display` or `Debug`: [`Secret`] hides itself, and no
//! [`VaultError`] holds a value.

mod error;
mod secret;
mod store;

pub use error::VaultError;
pub use secret::{Secret, SecretName};
pub use store::{KeyringStore, MemoryStore, SecretStore};

/// The keychain service name for every C-Harness credential. The account is the
/// [`SecretName`].
pub const SERVICE: &str = "c-harness";

/// Stores, reads and deletes credentials by name.
#[derive(Debug, Default)]
pub struct Vault<S = KeyringStore> {
    store: S,
}

impl Vault {
    /// The vault backed by the OS credential store under [`SERVICE`].
    pub fn os() -> Self {
        Self::default()
    }
}

impl<S: SecretStore> Vault<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    /// Stores `value` exactly as given, replacing any earlier value.
    pub fn set(&self, name: &SecretName, value: &str) -> Result<(), VaultError> {
        if value.trim().is_empty() {
            return Err(VaultError::EmptySecret { name: name.clone() });
        }
        self.store.set(name, value).map_err(|err| err.redact(value))
    }

    pub fn get(&self, name: &SecretName) -> Result<Option<Secret>, VaultError> {
        self.store.get(name)
    }

    /// Removes the credential. Deleting one that does not exist succeeds.
    pub fn delete(&self, name: &SecretName) -> Result<(), VaultError> {
        self.store.delete(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAKE: &str = "test-secret-not-real";

    // A store whose platform error repeats the value, as a careless backend might.
    struct EchoingStore;

    impl SecretStore for EchoingStore {
        fn set(&self, name: &SecretName, value: &str) -> Result<(), VaultError> {
            Err(VaultError::Platform {
                name: name.clone(),
                detail: format!("could not save {value}"),
            })
        }

        fn get(&self, _: &SecretName) -> Result<Option<Secret>, VaultError> {
            Ok(None)
        }

        fn delete(&self, _: &SecretName) -> Result<(), VaultError> {
            Ok(())
        }
    }

    #[test]
    fn set_get_replace_delete() {
        let vault = Vault::new(MemoryStore::new());
        let name = SecretName::model("openai").unwrap();
        assert!(vault.get(&name).unwrap().is_none());

        vault.set(&name, FAKE).unwrap();
        assert_eq!(vault.get(&name).unwrap().unwrap().expose(), FAKE);
        vault.set(&name, "test-secret-2-not-real").unwrap();
        assert_eq!(
            vault.get(&name).unwrap().unwrap().expose(),
            "test-secret-2-not-real"
        );
        assert!(vault.get(&SecretName::github()).unwrap().is_none());

        vault.delete(&name).unwrap();
        assert!(vault.get(&name).unwrap().is_none());
        vault.delete(&name).unwrap();
    }

    #[test]
    fn blank_values_are_refused() {
        let vault = Vault::new(MemoryStore::new());
        for blank in ["", " ", "\n\t"] {
            assert!(matches!(
                vault.set(&SecretName::github(), blank),
                Err(VaultError::EmptySecret { .. })
            ));
        }
        assert!(vault.get(&SecretName::github()).unwrap().is_none());
    }

    #[test]
    fn a_store_error_never_carries_the_value() {
        let err = Vault::new(EchoingStore)
            .set(&SecretName::github(), FAKE)
            .unwrap_err();
        assert!(!err.to_string().contains(FAKE), "{err}");
        assert!(!format!("{err:?}").contains(FAKE));
    }

    #[test]
    fn os_vault_uses_the_app_service() {
        assert_eq!(
            format!("{:?}", Vault::os()),
            r#"Vault { store: KeyringStore { service: "c-harness" } }"#
        );
    }
}
