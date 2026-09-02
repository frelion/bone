use std::{
    collections::BTreeMap,
    fmt,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_CREDENTIAL_FILE_BYTES: usize = 1024 * 1024;
const MAX_CREDENTIALS: usize = 1024;
const MAX_KEY_BYTES: usize = 128;
const MAX_SECRET_BYTES: usize = 64 * 1024;

/// Failures from the application-owned credential store.
///
/// Error values never contain credential contents.
#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("credential path must be absolute")]
    RelativePath,
    #[error("credential path has no parent directory")]
    MissingParent,
    #[error("invalid credential key")]
    InvalidKey,
    #[error("credential value must not be empty")]
    EmptySecret,
    #[error("credential value exceeds the {maximum_bytes}-byte limit")]
    SecretTooLarge { maximum_bytes: usize },
    #[error("credential store exceeds the {maximum_bytes}-byte limit")]
    StoreTooLarge { maximum_bytes: usize },
    #[error("credential store contains too many entries")]
    TooManyCredentials,
    #[error("credential is not configured: {0}")]
    NotFound(String),
    #[error("credential store is invalid")]
    InvalidStore,
    #[error("credential storage is busy")]
    Busy,
    #[error("unsafe credential storage at {path}: {reason}")]
    UnsafeStorage { path: PathBuf, reason: String },
    #[error("failed to {operation} credential storage at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl CredentialError {
    fn io(operation: &'static str, path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            operation,
            path: path.into(),
            source,
        }
    }
}

/// Secret input accepted by trusted human-facing code.
///
/// This type deliberately implements neither `Serialize` nor `Clone`.
pub struct SecretValue(String);

impl SecretValue {
    pub fn new(value: impl Into<String>) -> Result<Self, CredentialError> {
        let value = value.into();
        if value.is_empty() {
            return Err(CredentialError::EmptySecret);
        }
        if value.len() > MAX_SECRET_BYTES {
            return Err(CredentialError::SecretTooLarge {
                maximum_bytes: MAX_SECRET_BYTES,
            });
        }
        Ok(Self(value))
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretValue(<redacted>)")
    }
}

/// Resolved credential passed directly to a configured client.
///
/// This type deliberately implements neither `Serialize` nor `Display`.
#[derive(Clone)]
pub struct SecretLease(Arc<str>);

impl SecretLease {
    /// Explicitly borrow the secret for constructing an authenticated client.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretLease(<redacted>)")
    }
}

/// Model- and UI-safe credential presence.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CredentialStatus {
    Missing,
    Configured,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredCredential {
    token: String,
}

/// A small application-owned JSON credential store.
///
/// The path is explicit so applications, tests, and future frontends share the
/// same implementation without hidden environment discovery.
#[derive(Clone)]
pub struct CredentialStore {
    path: Arc<PathBuf>,
    lock_path: Arc<PathBuf>,
}

impl fmt::Debug for CredentialStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl CredentialStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CredentialError> {
        let path = path.as_ref();
        if !path.is_absolute() {
            return Err(CredentialError::RelativePath);
        }
        let parent = path.parent().ok_or(CredentialError::MissingParent)?;
        ensure_parent(parent)?;
        let parent = fs::canonicalize(parent)
            .map_err(|source| CredentialError::io("resolve directory", parent, source))?;
        let file_name = path.file_name().ok_or(CredentialError::MissingParent)?;
        let path = parent.join(file_name);
        let store = Self {
            lock_path: Arc::new(lock_path(&path)),
            path: Arc::new(path),
        };
        let _lock = store.acquire_lock()?;
        let _ = read_credentials(&store.path)?;
        Ok(store)
    }

    pub fn status(&self, key: &str) -> Result<CredentialStatus, CredentialError> {
        validate_key(key)?;
        let _lock = self.acquire_lock()?;
        let credentials = read_credentials(&self.path)?;
        Ok(if credentials.contains_key(key) {
            CredentialStatus::Configured
        } else {
            CredentialStatus::Missing
        })
    }

    /// Store or replace a credential. Returns whether persistent state changed.
    pub fn put(&self, key: &str, value: SecretValue) -> Result<bool, CredentialError> {
        validate_key(key)?;
        let _lock = self.acquire_lock()?;
        let mut credentials = read_credentials(&self.path)?;
        if credentials.len() >= MAX_CREDENTIALS && !credentials.contains_key(key) {
            return Err(CredentialError::TooManyCredentials);
        }
        if credentials
            .get(key)
            .is_some_and(|stored| stored.token == value.0)
        {
            return Ok(false);
        }
        credentials.insert(key.to_owned(), StoredCredential { token: value.0 });
        write_credentials(&self.path, &credentials)?;
        Ok(true)
    }

    /// Remove a credential. Returns whether an entry existed.
    pub fn remove(&self, key: &str) -> Result<bool, CredentialError> {
        validate_key(key)?;
        let _lock = self.acquire_lock()?;
        let mut credentials = read_credentials(&self.path)?;
        let removed = credentials.remove(key).is_some();
        if removed {
            write_credentials(&self.path, &credentials)?;
        }
        Ok(removed)
    }

    pub fn resolve(&self, key: &str) -> Result<SecretLease, CredentialError> {
        validate_key(key)?;
        let _lock = self.acquire_lock()?;
        let credentials = read_credentials(&self.path)?;
        let credential = credentials
            .get(key)
            .ok_or_else(|| CredentialError::NotFound(key.to_owned()))?;
        Ok(SecretLease(Arc::from(credential.token.as_str())))
    }

    fn acquire_lock(&self) -> Result<StoreLock, CredentialError> {
        let parent = self.path.parent().ok_or(CredentialError::MissingParent)?;
        validate_parent(parent)?;
        StoreLock::acquire(&self.lock_path)
    }
}

fn validate_key(key: &str) -> Result<(), CredentialError> {
    let valid = !key.is_empty()
        && key.len() <= MAX_KEY_BYTES
        && key.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        });
    if valid {
        Ok(())
    } else {
        Err(CredentialError::InvalidKey)
    }
}

fn read_credentials(path: &Path) -> Result<BTreeMap<String, StoredCredential>, CredentialError> {
    let mut file = match open_existing_file(path)? {
        Some(file) => file,
        None => return Ok(BTreeMap::new()),
    };
    let metadata = file
        .metadata()
        .map_err(|source| CredentialError::io("inspect", path, source))?;
    if metadata.len() > MAX_CREDENTIAL_FILE_BYTES as u64 {
        return Err(CredentialError::StoreTooLarge {
            maximum_bytes: MAX_CREDENTIAL_FILE_BYTES,
        });
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take((MAX_CREDENTIAL_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| CredentialError::io("read", path, source))?;
    if bytes.len() > MAX_CREDENTIAL_FILE_BYTES {
        return Err(CredentialError::StoreTooLarge {
            maximum_bytes: MAX_CREDENTIAL_FILE_BYTES,
        });
    }
    let credentials: BTreeMap<String, StoredCredential> =
        serde_json::from_slice(&bytes).map_err(|_| CredentialError::InvalidStore)?;
    if credentials.len() > MAX_CREDENTIALS {
        return Err(CredentialError::TooManyCredentials);
    }
    for (key, credential) in &credentials {
        validate_key(key)?;
        if credential.token.is_empty() {
            return Err(CredentialError::InvalidStore);
        }
        if credential.token.len() > MAX_SECRET_BYTES {
            return Err(CredentialError::SecretTooLarge {
                maximum_bytes: MAX_SECRET_BYTES,
            });
        }
    }
    Ok(credentials)
}

fn write_credentials(
    path: &Path,
    credentials: &BTreeMap<String, StoredCredential>,
) -> Result<(), CredentialError> {
    let mut bytes =
        serde_json::to_vec_pretty(credentials).map_err(|_| CredentialError::InvalidStore)?;
    bytes.push(b'\n');
    if bytes.len() > MAX_CREDENTIAL_FILE_BYTES {
        return Err(CredentialError::StoreTooLarge {
            maximum_bytes: MAX_CREDENTIAL_FILE_BYTES,
        });
    }

    validate_existing_file(path)?;
    let parent = path.parent().ok_or(CredentialError::MissingParent)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".bone-credentials-")
        .tempfile_in(parent)
        .map_err(|source| CredentialError::io("create temporary file", parent, source))?;
    set_private_file_permissions(temporary.as_file(), temporary.path())?;
    temporary
        .write_all(&bytes)
        .map_err(|source| CredentialError::io("write temporary file", temporary.path(), source))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|source| CredentialError::io("sync temporary file", temporary.path(), source))?;
    temporary
        .persist(path)
        .map_err(|error| CredentialError::io("replace credential file", path, error.error))?;
    sync_directory(parent)
}

fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".lock");
    PathBuf::from(name)
}

fn ensure_parent(path: &Path) -> Result<(), CredentialError> {
    if !path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            builder
                .create(path)
                .map_err(|source| CredentialError::io("create directory", path, source))?;
        }
        #[cfg(not(unix))]
        fs::create_dir_all(path)
            .map_err(|source| CredentialError::io("create directory", path, source))?;
    }
    validate_parent(path)
}

fn validate_parent(path: &Path) -> Result<(), CredentialError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| CredentialError::io("inspect directory", path, source))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(CredentialError::UnsafeStorage {
            path: path.to_path_buf(),
            reason: "parent must be a real directory".to_owned(),
        });
    }
    validate_owned_directory(path, &metadata)
}

fn open_existing_file(path: &Path) -> Result<Option<File>, CredentialError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(CredentialError::UnsafeStorage {
                path: path.to_path_buf(),
                reason: "credential store must be a regular file".to_owned(),
            });
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(CredentialError::io("inspect file", path, source)),
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(CredentialError::io("open", path, source)),
    };
    validate_open_file(&file, path)?;
    Ok(Some(file))
}

fn validate_existing_file(path: &Path) -> Result<(), CredentialError> {
    let _ = open_existing_file(path)?;
    Ok(())
}

fn validate_open_file(file: &File, path: &Path) -> Result<(), CredentialError> {
    let metadata = file
        .metadata()
        .map_err(|source| CredentialError::io("inspect file", path, source))?;
    if !metadata.file_type().is_file() {
        return Err(CredentialError::UnsafeStorage {
            path: path.to_path_buf(),
            reason: "credential store must be a regular file".to_owned(),
        });
    }
    validate_owned_private_file(path, &metadata)
}

struct StoreLock {
    file: File,
}

impl StoreLock {
    fn acquire(path: &Path) -> Result<Self, CredentialError> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options
            .open(path)
            .map_err(|source| CredentialError::io("open lock", path, source))?;
        validate_open_file(&file, path)?;
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(CredentialError::Busy);
            }
            Err(source) => return Err(CredentialError::io("lock", path, source)),
        }
        Ok(Self { file })
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[cfg(unix)]
fn validate_owned_directory(path: &Path, metadata: &fs::Metadata) -> Result<(), CredentialError> {
    use std::os::unix::fs::MetadataExt;
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(CredentialError::UnsafeStorage {
            path: path.to_path_buf(),
            reason: "directory is not owned by the current user".to_owned(),
        });
    }
    if metadata.mode() & 0o022 != 0 {
        return Err(CredentialError::UnsafeStorage {
            path: path.to_path_buf(),
            reason: "directory must not be writable by group or other users".to_owned(),
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_owned_directory(_: &Path, _: &fs::Metadata) -> Result<(), CredentialError> {
    Ok(())
}

#[cfg(unix)]
fn validate_owned_private_file(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), CredentialError> {
    use std::os::unix::fs::MetadataExt;
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(CredentialError::UnsafeStorage {
            path: path.to_path_buf(),
            reason: "file is not owned by the current user".to_owned(),
        });
    }
    if metadata.nlink() != 1 {
        return Err(CredentialError::UnsafeStorage {
            path: path.to_path_buf(),
            reason: "file must have exactly one hard link".to_owned(),
        });
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(CredentialError::UnsafeStorage {
            path: path.to_path_buf(),
            reason: "file permissions must be private".to_owned(),
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_owned_private_file(_: &Path, _: &fs::Metadata) -> Result<(), CredentialError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(file: &File, path: &Path) -> Result<(), CredentialError> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|source| CredentialError::io("set private permissions", path, source))
}

#[cfg(not(unix))]
fn set_private_file_permissions(_: &File, _: &Path) -> Result<(), CredentialError> {
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), CredentialError> {
    let directory =
        File::open(path).map_err(|source| CredentialError::io("open directory", path, source))?;
    directory
        .sync_all()
        .map_err(|source| CredentialError::io("sync directory", path, source))
}

#[cfg(not(unix))]
fn sync_directory(_: &Path) -> Result<(), CredentialError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn store() -> (tempfile::TempDir, CredentialStore) {
        let directory = tempfile::tempdir().unwrap();
        let store = CredentialStore::open(directory.path().join("credentials.json")).unwrap();
        (directory, store)
    }

    #[test]
    fn stores_resolves_and_removes_credentials_without_exposing_them() {
        let (_directory, store) = store();
        assert_eq!(
            store.status("github.work").unwrap(),
            CredentialStatus::Missing
        );
        assert!(
            store
                .put("github.work", SecretValue::new("token-value").unwrap())
                .unwrap()
        );
        assert_eq!(
            store.status("github.work").unwrap(),
            CredentialStatus::Configured
        );
        let lease = store.resolve("github.work").unwrap();
        assert_eq!(lease.expose_secret(), "token-value");
        assert!(!format!("{lease:?}").contains("token-value"));
        assert!(!format!("{store:?}").contains("token-value"));
        assert!(
            !store
                .put("github.work", SecretValue::new("token-value").unwrap())
                .unwrap()
        );
        assert!(store.remove("github.work").unwrap());
        assert!(!store.remove("github.work").unwrap());
    }

    #[test]
    fn validates_keys_and_secret_sizes() {
        let (_directory, store) = store();
        for key in ["", ".github", "github.", "github..work", "github/work"] {
            assert!(matches!(
                store.status(key),
                Err(CredentialError::InvalidKey)
            ));
        }
        assert!(matches!(
            SecretValue::new(""),
            Err(CredentialError::EmptySecret)
        ));
        assert!(matches!(
            SecretValue::new("x".repeat(MAX_SECRET_BYTES + 1)),
            Err(CredentialError::SecretTooLarge { .. })
        ));
    }

    #[test]
    fn debug_is_redacted() {
        let secret = SecretValue::new("never-print-this").unwrap();
        let debug = format!("{secret:?}");
        assert!(debug.contains("redacted"));
        assert!(!debug.contains("never-print-this"));
    }

    #[test]
    fn corrupt_store_errors_never_echo_file_contents() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("credentials.json");
        fs::write(&path, br#"{"github.work":"never-print-this"}"#).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let error = CredentialStore::open(path).unwrap_err();
        assert!(matches!(error, CredentialError::InvalidStore));
        assert!(!format!("{error:?}").contains("never-print-this"));
        assert!(!error.to_string().contains("never-print-this"));
    }

    #[test]
    fn a_held_store_lock_returns_busy() {
        let (_directory, store) = store();
        let _lock = StoreLock::acquire(&store.lock_path).unwrap();
        assert!(matches!(
            store.status("github.work"),
            Err(CredentialError::Busy)
        ));
    }

    #[test]
    fn refuses_relative_paths_and_unsafe_files() {
        assert!(matches!(
            CredentialStore::open("credentials.json"),
            Err(CredentialError::RelativePath)
        ));

        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            let directory = tempfile::tempdir().unwrap();
            let target = directory.path().join("target.json");
            fs::write(&target, b"{}").unwrap();
            fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
            let link = directory.path().join("credentials.json");
            symlink(&target, &link).unwrap();
            assert!(CredentialStore::open(link).is_err());

            let readable = directory.path().join("readable.json");
            fs::write(&readable, b"{}").unwrap();
            fs::set_permissions(&readable, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(matches!(
                CredentialStore::open(readable),
                Err(CredentialError::UnsafeStorage { .. })
            ));
        }
    }

    #[test]
    #[cfg(unix)]
    fn refuses_hard_linked_credential_files() {
        let (directory, store) = store();
        store
            .put("github.work", SecretValue::new("secret").unwrap())
            .unwrap();
        fs::hard_link(
            directory.path().join("credentials.json"),
            directory.path().join("credential-copy.json"),
        )
        .unwrap();
        assert!(matches!(
            store.resolve("github.work"),
            Err(CredentialError::UnsafeStorage { .. })
        ));
    }
}
