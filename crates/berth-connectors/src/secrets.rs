//! Connector secrets (Keychain) and agent environment assembly.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Mutex, PoisonError};

use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};

use crate::{Connector, ConnectorError};

/// `errSecItemNotFound`.
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

/// Storage for connector secrets, keyed by `(service, account)`.
/// Calls may block (Keychain); keep them off the UI thread.
pub trait SecretStore: Send + Sync {
    /// The secret, or `None` if absent.
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, ConnectorError>;
    /// Creates or replaces the secret.
    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), ConnectorError>;
    /// Removes the secret; absent is not an error.
    fn delete(&self, service: &str, account: &str) -> Result<(), ConnectorError>;
}

/// macOS login Keychain, generic passwords.
#[derive(Debug, Clone, Copy, Default)]
pub struct KeychainStore;

fn keychain_error(
    service: &str,
    account: &str,
    e: &security_framework::base::Error,
) -> ConnectorError {
    ConnectorError::Keychain(format!("{service}/{account}: {e} ({})", e.code()))
}

impl SecretStore for KeychainStore {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, ConnectorError> {
        match get_generic_password(service, account) {
            Ok(bytes) => String::from_utf8(bytes).map(Some).map_err(|_| {
                ConnectorError::Keychain(format!("{service}/{account}: value is not UTF-8"))
            }),
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(None),
            Err(e) => Err(keychain_error(service, account, &e)),
        }
    }

    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), ConnectorError> {
        set_generic_password(service, account, value.as_bytes())
            .map_err(|e| keychain_error(service, account, &e))
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), ConnectorError> {
        match delete_generic_password(service, account) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(()),
            Err(e) => Err(keychain_error(service, account, &e)),
        }
    }
}

/// In-memory store for tests and dev mode.
#[derive(Default)]
pub struct MemoryStore {
    items: Mutex<HashMap<(String, String), String>>,
}

impl MemoryStore {
    /// Empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn items(&self) -> std::sync::MutexGuard<'_, HashMap<(String, String), String>> {
        self.items.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl fmt::Debug for MemoryStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryStore")
            .field("len", &self.items().len())
            .finish_non_exhaustive()
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, ConnectorError> {
        Ok(self
            .items()
            .get(&(service.to_owned(), account.to_owned()))
            .cloned())
    }

    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), ConnectorError> {
        self.items()
            .insert((service.to_owned(), account.to_owned()), value.to_owned());
        Ok(())
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), ConnectorError> {
        self.items()
            .remove(&(service.to_owned(), account.to_owned()));
        Ok(())
    }
}

/// Secret env vars for the sidecar and `slot.sh`. `Debug` redacts values.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct SecretEnv {
    /// `(VAR, value)`.
    pub vars: Vec<(String, String)>,
    /// `(connector, secret)` pairs with no stored value.
    pub missing: Vec<(String, String)>,
}

impl fmt::Debug for SecretEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.vars.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("SecretEnv")
            .field("vars", &names)
            .field("missing", &self.missing)
            .finish()
    }
}

/// Looks up every declared secret. The first connector to provide a variable wins.
pub fn secret_env(
    connectors: &[Connector],
    store: &dyn SecretStore,
) -> Result<SecretEnv, ConnectorError> {
    let mut env = SecretEnv::default();
    for connector in connectors {
        let service = connector.keychain_service();
        let optional = connector.optional_secrets.iter().map(|s| (s, false));
        for (secret, required) in connector.secrets.iter().map(|s| (s, true)).chain(optional) {
            if env.vars.iter().any(|(k, _)| k == secret) {
                tracing::warn!(
                    connector = %connector.name,
                    secret = %secret,
                    "secret already provided by another connector; ignoring"
                );
                continue;
            }
            match store.get(&service, secret)? {
                Some(value) => env.vars.push((secret.clone(), value)),
                None if required => env.missing.push((connector.name.clone(), secret.clone())),
                None => {}
            }
        }
    }
    Ok(env)
}

/// `base_path` with each connector's bin dir prepended (in order), deduplicated,
/// `:`-joined. Empty entries are dropped.
#[must_use]
pub fn path_with_connectors(connectors: &[Connector], base_path: &str) -> String {
    let mut seen = HashSet::new();
    let mut parts: Vec<String> = Vec::new();
    for connector in connectors {
        let dir = connector.bin_dir();
        match dir.to_str() {
            Some(d) if !d.is_empty() && !d.contains(':') => {
                if seen.insert(d.to_owned()) {
                    parts.push(d.to_owned());
                }
            }
            _ => tracing::warn!(
                connector = %connector.name,
                dir = %dir.display(),
                "bin dir cannot be represented in PATH; skipping"
            ),
        }
    }
    for p in base_path.split(':') {
        if !p.is_empty() && seen.insert(p.to_owned()) {
            parts.push(p.to_owned());
        }
    }
    parts.join(":")
}
