//! App-owned ChatGPT subscription credential cache.
//!
//! Rig owns `auth.json` contents and refreshes it. This module owns its
//! private location and the lease that prevents logout from racing a live
//! endpoint.

use std::{
    fs::{self, File},
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::Arc,
};

use bone_adapters::llm::service::chatgpt_subscription::ChatGptAuthCache;
use fs2::FileExt;
use thiserror::Error;

use crate::safe_file::{self, SafeFileError};

const SERVICE: &str = "chatgpt-subscription";

/// Redacted failures while accessing the local ChatGPT credential cache.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum CredentialError {
    #[error("ChatGPT sign-in cache is in use by another BONE process")]
    Busy,
    #[error("ChatGPT sign-in cache is unavailable or unsafe")]
    Unavailable,
}

/// Owns placement and leasing of Rig's ChatGPT subscription cache.
#[derive(Clone)]
pub struct ChatGptCredentials {
    config_root: PathBuf,
}

impl ChatGptCredentials {
    /// Uses an explicit BONE config root; useful for embedding and tests.
    pub fn at(config_root: impl Into<PathBuf>) -> Result<Self, CredentialError> {
        let config_root = config_root.into();
        if !config_root.is_absolute() {
            return Err(CredentialError::Unavailable);
        }
        Ok(Self { config_root })
    }

    /// Acquires the exclusive lease required by a ChatGPT endpoint.
    pub fn acquire(&self) -> Result<ChatGptAuthLease, CredentialError> {
        let directory = self.create_service_directory()?;
        let auth_file = directory.join("auth.json");
        let lock_file = directory.join("auth.lock");
        let lock = lock_or_create(&lock_file)?;

        if open_private_file(&auth_file, false)?.is_none()
            && !write_new_private_file(&auth_file, b"{}")?
            && open_private_file(&auth_file, false)?.is_none()
        {
            return Err(CredentialError::Unavailable);
        }

        Ok(ChatGptAuthLease {
            inner: Arc::new(ChatGptAuthLeaseInner { auth_file, lock }),
        })
    }

    /// Removes the local cache without revoking the upstream account.
    pub fn clear(&self) -> Result<(), CredentialError> {
        let Some(directory) = self.existing_service_directory()? else {
            return Ok(());
        };
        let auth_file = directory.join("auth.json");
        let lock_file = directory.join("auth.lock");
        let auth_exists = open_private_file(&auth_file, false)?.is_some();
        let _lock = match lock_if_exists(&lock_file)? {
            Some(lock) => lock,
            None if auth_exists => lock_or_create(&lock_file)?,
            None => return Ok(()),
        };

        if auth_exists {
            fs::remove_file(auth_file).map_err(|_| CredentialError::Unavailable)?;
            sync_directory(&directory)?;
        }
        Ok(())
    }

    fn create_service_directory(&self) -> Result<PathBuf, CredentialError> {
        let providers = self.config_root.join("providers");
        let directory = providers.join(SERVICE);
        for path in [&self.config_root, &providers, &directory] {
            safe(safe_file::ensure_private_directory(path))?;
        }
        Ok(directory)
    }

    fn existing_service_directory(&self) -> Result<Option<PathBuf>, CredentialError> {
        let providers = self.config_root.join("providers");
        let directory = providers.join(SERVICE);
        for path in [&self.config_root, &providers, &directory] {
            if !safe(safe_file::private_directory_exists(path))? {
                return Ok(None);
            }
        }
        Ok(Some(directory))
    }
}

struct ChatGptAuthLeaseInner {
    auth_file: PathBuf,
    lock: File,
}

impl Drop for ChatGptAuthLeaseInner {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.lock);
    }
}

/// A cloneable capability over a live ChatGPT credential-cache lease.
#[derive(Clone)]
pub struct ChatGptAuthLease {
    inner: Arc<ChatGptAuthLeaseInner>,
}

impl ChatGptAuthCache for ChatGptAuthLease {
    fn auth_file(&self) -> &Path {
        &self.inner.auth_file
    }
}

fn open_private_file(path: &Path, create_new: bool) -> Result<Option<File>, CredentialError> {
    if create_new {
        safe(safe_file::create_private_file(path))
    } else {
        safe(safe_file::open_existing_private_file(path))
    }
}

fn lock_or_create(path: &Path) -> Result<File, CredentialError> {
    let file = match open_private_file(path, false)? {
        Some(file) => file,
        None => match open_private_file(path, true)? {
            Some(file) => file,
            None => open_private_file(path, false)?.ok_or(CredentialError::Unavailable)?,
        },
    };
    try_lock(file)
}

fn lock_if_exists(path: &Path) -> Result<Option<File>, CredentialError> {
    open_private_file(path, false)?.map(try_lock).transpose()
}

fn try_lock(file: File) -> Result<File, CredentialError> {
    match file.try_lock_exclusive() {
        Ok(()) => Ok(file),
        Err(error) if error.kind() == ErrorKind::WouldBlock => Err(CredentialError::Busy),
        Err(_) => Err(CredentialError::Unavailable),
    }
}

fn write_new_private_file(path: &Path, bytes: &[u8]) -> Result<bool, CredentialError> {
    safe(safe_file::atomic_write_new_private(path, bytes))
}

fn sync_directory(path: &Path) -> Result<(), CredentialError> {
    safe(safe_file::sync_directory(path))
}

fn safe<T>(result: Result<T, SafeFileError>) -> Result<T, CredentialError> {
    result.map_err(|_| CredentialError::Unavailable)
}
