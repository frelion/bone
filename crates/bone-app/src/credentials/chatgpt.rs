//! App-owned private location for Rig's ChatGPT credential transactions.

use crate::safe_file;
use std::path::PathBuf;
use thiserror::Error;

const SERVICE: &str = "chatgpt-subscription";

/// Redacted failures while accessing the local ChatGPT credential cache.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum CredentialError {
    #[error("ChatGPT sign-in cache is unavailable or unsafe")]
    Unavailable,
}

/// Owns cache placement; Rig owns its schema and transaction locking.
#[derive(Clone)]
pub struct ChatGptCredentials {
    config_root: PathBuf,
}

impl ChatGptCredentials {
    /// Uses an explicit absolute BONE config root.
    pub fn at(config_root: impl Into<PathBuf>) -> Result<Self, CredentialError> {
        let config_root = config_root.into();
        if !config_root.is_absolute() {
            return Err(CredentialError::Unavailable);
        }
        Ok(Self { config_root })
    }

    /// Validate the private cache location without acquiring a lifetime lock.
    pub fn auth_file(&self) -> Result<PathBuf, CredentialError> {
        let providers = self.config_root.join("providers");
        let directory = providers.join(SERVICE);
        for path in [&self.config_root, &providers, &directory] {
            safe_file::ensure_private_directory(path).map_err(|_| CredentialError::Unavailable)?;
        }
        let path = directory.join("auth.json");
        safe_file::open_existing_private_file(&path).map_err(|_| CredentialError::Unavailable)?;
        Ok(path)
    }

    /// Delete the cache in the same transaction used by token refresh.
    pub async fn clear(&self) -> Result<(), CredentialError> {
        let path = self.auth_file()?;
        bone_adapters::llm::service::chatgpt_subscription::clear_cache(&path)
            .await
            .map_err(|_| CredentialError::Unavailable)
    }
}
