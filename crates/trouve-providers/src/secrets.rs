//! Secret storage: OS keychain/keystore when available, with an encrypted-
//! permissions file fallback for headless machines.
//!
//! Keys are namespaced strings like `provider:openai:api_key` or
//! `provider:openai:oauth`.

use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result};

pub trait SecretStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>>;
    fn set(&self, key: &str, value: &str) -> Result<()>;
    fn delete(&self, key: &str) -> Result<()>;
}

const SERVICE: &str = "trouve";

/// OS keychain (Secret Service / keychain / credential manager).
pub struct KeyringStore;

impl KeyringStore {
    /// Probe the backend so callers can fall back cleanly on headless boxes.
    pub fn available() -> bool {
        keyring::Entry::new(SERVICE, "trouve-probe")
            .map(|e| !matches!(e.get_password(), Err(keyring::Error::PlatformFailure(_))))
            .unwrap_or(false)
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, key: &str) -> Result<Option<String>> {
        let entry = keyring::Entry::new(SERVICE, key)?;
        match entry.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<()> {
        keyring::Entry::new(SERVICE, key)?.set_password(value)?;
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<()> {
        let entry = keyring::Entry::new(SERVICE, key)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// Fallback: a JSON file with owner-only permissions in the data dir.
pub struct FileStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FileStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    fn read_all(&self) -> Result<serde_json::Map<String, serde_json::Value>> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => Ok(serde_json::from_str(&text).unwrap_or_default()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
            Err(e) => Err(e).context("reading secrets file"),
        }
    }

    fn write_all(&self, map: &serde_json::Map<String, serde_json::Value>) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(map)?;
        std::fs::write(&self.path, text)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}

impl SecretStore for FileStore {
    fn get(&self, key: &str) -> Result<Option<String>> {
        let _guard = self.lock.lock().unwrap();
        Ok(self
            .read_all()?
            .get(key)
            .and_then(|v| v.as_str())
            .map(String::from))
    }

    fn set(&self, key: &str, value: &str) -> Result<()> {
        let _guard = self.lock.lock().unwrap();
        let mut map = self.read_all()?;
        map.insert(key.to_string(), value.into());
        self.write_all(&map)
    }

    fn delete(&self, key: &str) -> Result<()> {
        let _guard = self.lock.lock().unwrap();
        let mut map = self.read_all()?;
        map.remove(key);
        self.write_all(&map)
    }
}

/// The default store: keychain when the platform has one, file otherwise.
pub fn default_store(data_dir: &std::path::Path) -> Box<dyn SecretStore> {
    if KeyringStore::available() {
        Box::new(KeyringStore)
    } else {
        tracing::info!(
            "no usable OS keychain; storing secrets in {}",
            data_dir.display()
        );
        Box::new(FileStore::new(data_dir.join("secrets.json")))
    }
}

pub fn api_key_secret(provider_id: &str) -> String {
    format!("provider:{provider_id}:api_key")
}

pub fn oauth_secret(provider_id: &str) -> String {
    format!("provider:{provider_id}:oauth")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_store_roundtrip_and_permissions() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::new(tmp.path().join("sub/secrets.json"));
        assert_eq!(store.get("k").unwrap(), None);
        store.set("k", "v1").unwrap();
        store.set("k2", "v2").unwrap();
        assert_eq!(store.get("k").unwrap().as_deref(), Some("v1"));
        store.delete("k").unwrap();
        assert_eq!(store.get("k").unwrap(), None);
        assert_eq!(store.get("k2").unwrap().as_deref(), Some("v2"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(tmp.path().join("sub/secrets.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
