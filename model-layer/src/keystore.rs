//! Secret storage for provider credentials and caller keys.
//!
//! On macOS the default backend is the login Keychain through `/usr/bin/security`
//! (the tool keeps a stable code signature, so rebuilding `crewchat-model`
//! does not invalidate the item's ACL). SQLite keeps only a reference
//! (`keychain_ref`); the secret itself never lands in the database or logs.
//!
//! `InlineStore` is the non-persistent fallback for tests/Linux and for an
//! explicit `MODEL_LAYER_KEYSTORE=db`; in that mode the store keeps the secret
//! in SQLite (0600), matching CCR's old behavior.

use std::collections::HashMap;
use std::process::Command;
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};

pub trait KeyStore: Send + Sync {
    fn put(&self, account: &str, secret: &str) -> Result<()>;
    fn get(&self, account: &str) -> Result<Option<String>>;
    #[allow(dead_code)]
    fn delete(&self, account: &str) -> Result<()>;
    /// `false` means "keep the secret inline"; the store then leaves the DB
    /// column populated instead of writing a reference.
    fn is_persistent(&self) -> bool {
        true
    }
}

pub fn from_env() -> Arc<dyn KeyStore> {
    match std::env::var("MODEL_LAYER_KEYSTORE").ok().as_deref() {
        Some("db") | Some("inline") => Arc::new(InlineStore),
        Some("keychain") => Arc::new(KeychainStore::new()),
        _ if cfg!(target_os = "macos") => Arc::new(KeychainStore::new()),
        _ => Arc::new(InlineStore),
    }
}

/// Non-persistent: the caller keeps the secret in SQLite.
pub struct InlineStore;

impl KeyStore for InlineStore {
    fn put(&self, _account: &str, _secret: &str) -> Result<()> {
        Ok(())
    }

    fn get(&self, _account: &str) -> Result<Option<String>> {
        Ok(None)
    }

    fn delete(&self, _account: &str) -> Result<()> {
        Ok(())
    }

    fn is_persistent(&self) -> bool {
        false
    }
}

/// macOS login Keychain, through the `security` CLI.
pub struct KeychainStore {
    service: String,
    cache: Mutex<HashMap<String, String>>,
}

impl KeychainStore {
    pub fn new() -> Self {
        Self {
            service: "com.crewchat.model-layer".to_string(),
            cache: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    pub fn with_service(service: &str) -> Self {
        Self {
            service: service.to_string(),
            cache: Mutex::new(HashMap::new()),
        }
    }
}

impl KeyStore for KeychainStore {
    fn put(&self, account: &str, secret: &str) -> Result<()> {
        let status = Command::new("/usr/bin/security")
            .args([
                "add-generic-password",
                "-U",
                "-a",
                account,
                "-s",
                &self.service,
                "-w",
                secret,
            ])
            .status()
            .with_context(|| format!("run security add-generic-password for {account}"))?;
        if !status.success() {
            bail!("security add-generic-password failed for {account} ({status})");
        }
        self.cache
            .lock()
            .unwrap()
            .insert(account.to_string(), secret.to_string());
        Ok(())
    }

    fn get(&self, account: &str) -> Result<Option<String>> {
        if let Some(secret) = self.cache.lock().unwrap().get(account).cloned() {
            return Ok(Some(secret));
        }
        let output = Command::new("/usr/bin/security")
            .args([
                "find-generic-password",
                "-a",
                account,
                "-s",
                &self.service,
                "-w",
            ])
            .output()
            .with_context(|| format!("run security find-generic-password for {account}"))?;
        if !output.status.success() {
            return Ok(None);
        }
        let secret = String::from_utf8_lossy(&output.stdout)
            .trim_end_matches(['\n', '\r'])
            .to_string();
        if secret.is_empty() {
            return Ok(None);
        }
        self.cache
            .lock()
            .unwrap()
            .insert(account.to_string(), secret.clone());
        Ok(Some(secret))
    }

    fn delete(&self, account: &str) -> Result<()> {
        self.cache.lock().unwrap().remove(account);
        let status = Command::new("/usr/bin/security")
            .args([
                "delete-generic-password",
                "-a",
                account,
                "-s",
                &self.service,
            ])
            .status()
            .with_context(|| format!("run security delete-generic-password for {account}"))?;
        if !status.success() {
            // A missing item is fine; the caller only cares that the secret is
            // gone from our storage.
            tracing::debug!("keychain item {account} not deleted ({status})");
        }
        Ok(())
    }
}

#[cfg(test)]
#[derive(Default)]
pub struct MemoryKeyStore {
    inner: Mutex<HashMap<String, String>>,
}

#[cfg(test)]
impl KeyStore for MemoryKeyStore {
    fn put(&self, account: &str, secret: &str) -> Result<()> {
        self.inner
            .lock()
            .unwrap()
            .insert(account.to_string(), secret.to_string());
        Ok(())
    }

    fn get(&self, account: &str) -> Result<Option<String>> {
        Ok(self.inner.lock().unwrap().get(account).cloned())
    }

    fn delete(&self, account: &str) -> Result<()> {
        self.inner.lock().unwrap().remove(account);
        Ok(())
    }
}
