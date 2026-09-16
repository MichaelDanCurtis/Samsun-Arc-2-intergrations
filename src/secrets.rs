//! Where tokens and shared secrets live: the OS keychain by default, a plain
//! `secrets.json` when the config says so (tests, throwaway machines).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::config::SecretStoreKind;

pub const SMARTTHINGS_CLIENT_SECRET: &str = "smartthings.client_secret";
pub const SMARTTHINGS_TOKENS: &str = "smartthings.tokens";
pub const LAN_SECRET: &str = "lan.secret";

const SERVICE: &str = "deskswitch";

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("keychain error: {0}")]
    Keyring(#[from] keyring::Error),
    #[error("secrets file error: {0}")]
    File(#[from] std::io::Error),
    #[error("secrets file is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

pub trait SecretStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError>;
    fn set(&self, key: &str, value: &str) -> Result<(), SecretError>;
    fn delete(&self, key: &str) -> Result<(), SecretError>;
}

/// Plain-text JSON map, owner-readable only. Not for real credentials.
pub struct FileStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FileStore {
    #[must_use]
    pub fn in_dir(dir: &Path) -> Self {
        Self {
            path: dir.join("secrets.json"),
            lock: Mutex::new(()),
        }
    }

    fn read(&self) -> Result<BTreeMap<String, String>, SecretError> {
        match std::fs::read(&self.path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(error) => Err(error.into()),
        }
    }

    fn write(&self, map: &BTreeMap<String, String>) -> Result<(), SecretError> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temp = self.path.with_extension("json.tmp");
        std::fs::write(&temp, serde_json::to_vec_pretty(map)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&temp, &self.path)?;
        Ok(())
    }
}

impl SecretStore for FileStore {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(self.read()?.get(key).cloned())
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut map = self.read()?;
        map.insert(key.to_string(), value.to_string());
        self.write(&map)
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut map = self.read()?;
        map.remove(key);
        self.write(&map)
    }
}

/// macOS Keychain / Windows Credential Manager / Secret Service, via `keyring`.
pub struct KeyringStore;

impl SecretStore for KeyringStore {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        match keyring::Entry::new(SERVICE, key)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        Ok(keyring::Entry::new(SERVICE, key)?.set_password(value)?)
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        match keyring::Entry::new(SERVICE, key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[must_use]
pub fn open_store(kind: SecretStoreKind, dir: &Path) -> Arc<dyn SecretStore> {
    match kind {
        SecretStoreKind::Keyring => Arc::new(KeyringStore),
        SecretStoreKind::File => Arc::new(FileStore::in_dir(dir)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_store_round_trips_and_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::in_dir(dir.path());
        assert_eq!(store.get("a").unwrap(), None);
        store.set("a", "1").unwrap();
        store.set("b", "2").unwrap();
        assert_eq!(store.get("a").unwrap().as_deref(), Some("1"));
        store.delete("a").unwrap();
        assert_eq!(store.get("a").unwrap(), None);
        assert_eq!(store.get("b").unwrap().as_deref(), Some("2"));
    }

    #[cfg(unix)]
    #[test]
    fn file_store_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::in_dir(dir.path());
        store.set("a", "1").unwrap();
        let mode = std::fs::metadata(dir.path().join("secrets.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // keyring's mock stores credentials per Entry instance, so a set/get round-trip cannot
    // be observed through `KeyringStore` (which builds a fresh Entry per call); the round-trip
    // contract is covered by the FileStore tests and by the real keychain at runtime.
    #[test]
    fn keyring_store_maps_a_missing_entry_to_none_and_delete_to_ok() {
        keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
        let store = KeyringStore;
        // A fresh Entry in the mock's get_password() returns Error::NoEntry,
        // which KeyringStore.get() should map to Ok(None).
        assert_eq!(store.get("never-set").unwrap(), None);
        // KeyringStore.delete() should map Error::NoEntry to Ok(()).
        assert_eq!(store.delete("never-set").unwrap(), ());
    }
}
