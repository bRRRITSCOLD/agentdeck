//! The daemon's entries in the system keyring (spec §8): the saved GitHub token and the OAuth
//! client ID, under the service [`SERVICE`].
//!
//! [`Keyring`] is the port. [`SecretService`] is the real store, the keyring project's
//! Secret Service store; [`Memory`] is the integration tier's fake (`AGENTDECK_KEYRING=memory`),
//! held to the same contract by the keyring tests, which run both.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use keyring_core::api::CredentialStoreApi as _;

/// The service every entry is under.
pub const SERVICE: &str = "agentdeck";

/// The prefix of a saved token's entry: `github.com/<login>`.
pub const TOKEN_PREFIX: &str = "github.com/";

/// The OAuth client ID's entry. No GitHub login holds an `@`, so no token's entry is named
/// like it.
pub const CLIENT_ID_ENTRY: &str = "oauth-client-id@github.com";

/// The environment variable that picks the store: `memory` for [`Memory`] (tests), anything
/// else or unset for the Secret Service.
pub const KEYRING_VAR: &str = "AGENTDECK_KEYRING";

/// The entry name of `login`'s token.
pub fn token_entry(login: &str) -> String {
    format!("{TOKEN_PREFIX}{login}")
}

/// Why the keyring couldn't do something. Neither carries a secret.
#[derive(Debug, Clone, thiserror::Error)]
pub enum KeyringError {
    /// No Secret Service answers on the session bus (or there's no session bus).
    #[error("no system keyring: {0}")]
    Unavailable(String),
    /// The keyring answered, but the call failed.
    #[error("the system keyring failed: {0}")]
    Failed(String),
}

/// The entries under [`SERVICE`] in a credential store. Every call blocks.
///
/// The contract, which [`SecretService`] and [`Memory`] both keep: `get` of a name with no
/// entry is `Ok(None)`; `set` replaces the secret of an entry that exists; `delete` of a name
/// with no entry is `Ok(())`; `names` lists every entry under the service, in no set order.
pub trait Keyring: Send + Sync {
    /// The name of every entry under the service.
    fn names(&self) -> Result<Vec<String>, KeyringError>;
    /// The secret saved under `name`, if any.
    fn get(&self, name: &str) -> Result<Option<String>, KeyringError>;
    /// Saves `secret` under `name`, replacing what was there.
    fn set(&self, name: &str, secret: &str) -> Result<(), KeyringError>;
    /// Deletes the entry `name`, if there is one.
    fn delete(&self, name: &str) -> Result<(), KeyringError>;
}

/// The store [`KEYRING_VAR`] picks: `value` is the variable's value, if set.
pub fn from_env(value: Option<&str>) -> Box<dyn Keyring> {
    match value {
        Some("memory") => Box::new(Memory::default()),
        _ => Box::new(SecretService),
    }
}

/// The Secret Service on the session bus, through `keyring-core` and the keyring project's
/// zbus store. It connects on every call, so a keyring that starts after the daemon (a login
/// that unlocks it later) is found.
pub struct SecretService;

impl SecretService {
    fn store() -> Result<std::sync::Arc<zbus_secret_service_keyring_store::Store>, KeyringError> {
        zbus_secret_service_keyring_store::Store::new()
            .map_err(|err| KeyringError::Unavailable(err.to_string()))
    }

    fn entry(name: &str) -> Result<keyring_core::Entry, KeyringError> {
        Self::store()?
            .build(SERVICE, name, None)
            .map_err(|err| KeyringError::Failed(err.to_string()))
    }
}

fn failed(err: &keyring_core::Error) -> KeyringError {
    KeyringError::Failed(err.to_string())
}

impl Keyring for SecretService {
    fn names(&self) -> Result<Vec<String>, KeyringError> {
        let entries = Self::store()?
            .search(&HashMap::from([("service", SERVICE)]))
            .map_err(|err| failed(&err))?;
        let mut names = Vec::with_capacity(entries.len());
        for entry in entries {
            let attributes = entry.get_attributes().map_err(|err| failed(&err))?;
            if let Some(name) = attributes.get("username") {
                names.push(name.clone());
            }
        }
        Ok(names)
    }

    fn get(&self, name: &str) -> Result<Option<String>, KeyringError> {
        match Self::entry(name)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(err) => Err(failed(&err)),
        }
    }

    fn set(&self, name: &str, secret: &str) -> Result<(), KeyringError> {
        Self::entry(name)?
            .set_password(secret)
            .map_err(|err| failed(&err))
    }

    fn delete(&self, name: &str) -> Result<(), KeyringError> {
        match Self::entry(name)?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(err) => Err(failed(&err)),
        }
    }
}

/// An in-memory store, gone when the daemon exits: the integration tier's fake.
#[derive(Default)]
pub struct Memory {
    entries: Mutex<BTreeMap<String, String>>,
}

impl Memory {
    fn entries(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, String>> {
        // Nothing panics while holding the lock.
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Keyring for Memory {
    fn names(&self) -> Result<Vec<String>, KeyringError> {
        Ok(self.entries().keys().cloned().collect())
    }

    fn get(&self, name: &str) -> Result<Option<String>, KeyringError> {
        Ok(self.entries().get(name).cloned())
    }

    fn set(&self, name: &str, secret: &str) -> Result<(), KeyringError> {
        self.entries().insert(name.to_owned(), secret.to_owned());
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<(), KeyringError> {
        self.entries().remove(name);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod token_entry {
        use super::*;

        #[test]
        fn should_name_the_entry_after_the_login() {
            assert_eq!(token_entry("octocat"), "github.com/octocat");
        }

        #[test]
        fn should_never_collide_with_the_client_id_entry() {
            assert!(!CLIENT_ID_ENTRY.starts_with(TOKEN_PREFIX));
        }
    }
}
