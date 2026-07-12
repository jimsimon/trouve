//! Secret storage: OS keychain/keystore when available, with a plaintext
//! JSON file fallback (owner-only 0600 permissions, no encryption at rest)
//! for headless machines.
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
            // A corrupt file must surface as an error, not silently read as
            // empty: `set` would then persist an empty map plus one key,
            // wiping every other provider's stored credentials.
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("secrets file {} is corrupt", self.path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
            Err(e) => Err(e).context("reading secrets file"),
        }
    }

    fn write_all(&self, map: &serde_json::Map<String, serde_json::Value>) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(map)?;
        // Write to a temp file created with owner-only perms, then rename:
        // secrets never exist on disk world-readable (not even briefly on
        // first write), and a crash mid-write can't truncate the real file.
        let tmp = self.path.with_extension("tmp");
        {
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            use std::io::Write as _;
            let mut f = opts.open(&tmp).context("creating secrets temp file")?;
            f.write_all(text.as_bytes())
                .context("writing secrets temp file")?;
            f.sync_all().ok();
        }
        std::fs::rename(&tmp, &self.path).context("replacing secrets file")?;
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

    #[test]
    fn corrupt_file_errors_instead_of_wiping_secrets() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("secrets.json");
        std::fs::write(&path, "{ this is not json").unwrap();
        let store = FileStore::new(path);
        // A corrupt file must not read as empty (which would let the next
        // set() overwrite it and destroy other providers' credentials).
        assert!(store.get("k").is_err());
        assert!(store.set("k", "v").is_err());
    }
}
