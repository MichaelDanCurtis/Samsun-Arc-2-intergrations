//! Where tokens and shared secrets live: the OS keychain by default, a plain
//! `secrets.json` when the config says so (tests, throwaway machines).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::config::SecretStoreKind;

/// Secret key for `SmartThings` client secret.
pub const SMARTTHINGS_CLIENT_SECRET: &str = "smartthings.client_secret";
/// Secret key for `SmartThings` tokens.
pub const SMARTTHINGS_TOKENS: &str = "smartthings.tokens";
/// Secret key for LAN secret.
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
    /// Retrieve a secret by key, returning `None` if not found.
    fn get(&self, key: &str) -> Result<Option<String>, SecretError>;
    /// Store a secret with the given key.
    fn set(&self, key: &str, value: &str) -> Result<(), SecretError>;
    /// Delete a secret by key, returning `Ok` even if not found.
    // Part of the trait contract; no command calls it yet.
    #[allow(dead_code)]
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

    /// Write `map` via a sibling temp file and rename. The temp name carries
    /// this process's pid so a concurrent writer never clobbers another
    /// writer's in-flight temp file, and the file is created owner-only from
    /// the first byte rather than world-readable then chmod'd.
    fn write(&self, map: &BTreeMap<String, String>) -> Result<(), SecretError> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temp = self
            .path
            .with_extension(format!("json.{}.tmp", std::process::id()));
        let bytes = serde_json::to_vec_pretty(map)?;
        if let Err(error) = write_owner_only(&temp, &bytes) {
            let _ = std::fs::remove_file(&temp);
            return Err(error.into());
        }
        if let Err(error) = std::fs::rename(&temp, &self.path) {
            let _ = std::fs::remove_file(&temp);
            return Err(error.into());
        }
        Ok(())
    }
}

/// Create `path` fresh (failing if it already exists) with owner-only
/// permissions from the moment it's created, and write `bytes` to it.
#[cfg(unix)]
fn write_owner_only(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)
}

#[cfg(not(unix))]
fn write_owner_only(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
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

/// Open a secret store of the specified kind.
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
        // Smoke check: set does not error against the mock.
        store.set("k", "v").unwrap();
        // A fresh Entry in the mock's get_password() returns Error::NoEntry,
        // which KeyringStore.get() should map to Ok(None).
        assert_eq!(store.get("never-set").unwrap(), None);
        // KeyringStore.delete() should map Error::NoEntry to Ok(()).
        assert_eq!(store.delete("never-set").unwrap(), ());
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_rename_leaves_no_temp_file() {
        use std::fs;

        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::in_dir(dir.path());
        fs::create_dir(dir.path().join("secrets.json")).unwrap();
        let result = store.set("a", "1");
        assert!(result.is_err());
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| {
                std::path::Path::new(name)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("tmp"))
            })
            .collect();
        assert_eq!(leftovers, Vec::<String>::new());
    }

    #[cfg(unix)]
    #[test]
    fn the_temp_file_is_owner_only_from_creation() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        write_owner_only(&dir.path().join("probe.tmp"), b"{}").unwrap();
        let mode = std::fs::metadata(dir.path().join("probe.tmp"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
