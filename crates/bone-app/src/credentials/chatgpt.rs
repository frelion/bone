//! App-owned ChatGPT subscription credential cache.
//!
//! Rig owns `auth.json` contents and refreshes it. This module owns its
//! private location and the lease that prevents logout from racing a live
//! endpoint.

use std::{
    env,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use bone_adapters::llm::service::chatgpt_subscription::ChatGptAuthCache;
use fs2::FileExt;
use tempfile::NamedTempFile;
use thiserror::Error;

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
    /// Resolves `$XDG_CONFIG_HOME/bone`, or `~/.config/bone`.
    pub fn default_for_current_user() -> Result<Self, CredentialError> {
        let base = xdg_or_home(
            env::var_os("XDG_CONFIG_HOME"),
            env::var_os("HOME").as_ref(),
            ".config",
        )?;
        Self::at(base.join("bone"))
    }

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
            ensure_private_directory(path)?;
        }
        Ok(directory)
    }

    fn existing_service_directory(&self) -> Result<Option<PathBuf>, CredentialError> {
        let providers = self.config_root.join("providers");
        let directory = providers.join(SERVICE);
        for path in [&self.config_root, &providers, &directory] {
            if !private_directory_exists(path)? {
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

fn xdg_or_home(
    xdg: Option<OsString>,
    home: Option<&OsString>,
    fallback: &str,
) -> Result<PathBuf, CredentialError> {
    if let Some(path) = xdg
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return Ok(path);
    }
    home.filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .map(|home| home.join(fallback))
        .ok_or(CredentialError::Unavailable)
}

fn ensure_private_directory(path: &Path) -> Result<(), CredentialError> {
    match fs::symlink_metadata(path) {
        Ok(_) => validate_private_directory(path),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let parent = path.parent().ok_or(CredentialError::Unavailable)?;
            fs::create_dir_all(parent).map_err(|_| CredentialError::Unavailable)?;
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;

                builder.mode(0o700);
            }
            match builder.create(path) {
                Ok(()) => set_private_permissions(path, 0o700)?,
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(_) => return Err(CredentialError::Unavailable),
            }
            validate_private_directory(path)
        }
        Err(_) => Err(CredentialError::Unavailable),
    }
}

fn private_directory_exists(path: &Path) -> Result<bool, CredentialError> {
    match fs::symlink_metadata(path) {
        Ok(_) => validate_private_directory(path).map(|()| true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(_) => Err(CredentialError::Unavailable),
    }
}

fn validate_private_directory(path: &Path) -> Result<(), CredentialError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| CredentialError::Unavailable)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(CredentialError::Unavailable);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(CredentialError::Unavailable);
        }
    }
    Ok(())
}

fn set_private_permissions(path: &Path, mode: u32) -> Result<(), CredentialError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(|_| CredentialError::Unavailable)?;
    }
    Ok(())
}

fn open_private_file(path: &Path, create_new: bool) -> Result<Option<File>, CredentialError> {
    let parent = path.parent().ok_or(CredentialError::Unavailable)?;
    validate_private_directory(parent)?;
    if !create_new {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(CredentialError::Unavailable);
            }
            Ok(metadata) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;

                    if metadata.permissions().mode() & 0o077 != 0 {
                        return Err(CredentialError::Unavailable);
                    }
                }
            }
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(CredentialError::Unavailable),
        }
    }

    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(create_new);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    match options.open(path) {
        Ok(file) => {
            if create_new {
                set_private_permissions(path, 0o600)?;
            }
            Ok(Some(file))
        }
        Err(error) if error.kind() == ErrorKind::NotFound && !create_new => Ok(None),
        Err(error) if error.kind() == ErrorKind::AlreadyExists && create_new => Ok(None),
        Err(_) => Err(CredentialError::Unavailable),
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
    let parent = path.parent().ok_or(CredentialError::Unavailable)?;
    validate_private_directory(parent)?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(|_| CredentialError::Unavailable)?;
    set_private_permissions(temporary.path(), 0o600)?;
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|_| CredentialError::Unavailable)?;
    match temporary.persist_noclobber(path) {
        Ok(_) => Ok(true),
        Err(error) if error.error.kind() == ErrorKind::AlreadyExists => Ok(false),
        Err(_) => Err(CredentialError::Unavailable),
    }
}

fn sync_directory(path: &Path) -> Result<(), CredentialError> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| CredentialError::Unavailable)?;
    Ok(())
}
