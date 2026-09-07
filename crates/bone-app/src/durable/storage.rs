//! Small private-file primitives shared by the registry and session store.
//!
//! Locks are deliberately held only around one read/validate/write operation.
//! Durable session records use atomic replacement, so readers never need to
//! observe a partially-written JSON document.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use fs2::FileExt;
use serde::{Serialize, de::DeserializeOwned};

use super::StorageError;

pub(crate) const MAX_REGISTRY_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_SESSION_BYTES: usize = 4 * 1024 * 1024;

/// Canonicalize a private directory owned by BONE, creating it on first use.
pub(crate) fn ensure_private_directory(path: &Path) -> Result<PathBuf, StorageError> {
    if !path.is_absolute() {
        return Err(StorageError::RelativePath {
            path: path.to_path_buf(),
        });
    }

    if !path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;

            let mut builder = fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            builder
                .create(path)
                .map_err(|source| StorageError::io("create private directory", path, source))?;
        }
        #[cfg(not(unix))]
        fs::create_dir_all(path)
            .map_err(|source| StorageError::io("create private directory", path, source))?;
    }

    validate_private_directory(path)?;
    fs::canonicalize(path)
        .map_err(|source| StorageError::io("resolve private directory", path, source))
}

pub(crate) fn private_file_path(path: &Path) -> Result<PathBuf, StorageError> {
    if !path.is_absolute() {
        return Err(StorageError::RelativePath {
            path: path.to_path_buf(),
        });
    }
    let parent = path.parent().ok_or_else(|| StorageError::MissingParent {
        path: path.to_path_buf(),
    })?;
    let parent = ensure_private_directory(parent)?;
    let file_name = path
        .file_name()
        .ok_or_else(|| StorageError::MissingFileName {
            path: path.to_path_buf(),
        })?;
    Ok(parent.join(file_name))
}

pub(crate) fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".lock");
    PathBuf::from(name)
}

pub(crate) fn read_json<T>(path: &Path, maximum_bytes: usize) -> Result<Option<T>, StorageError>
where
    T: DeserializeOwned,
{
    let mut file = match open_existing_file(path)? {
        Some(file) => file,
        None => return Ok(None),
    };
    let metadata = file
        .metadata()
        .map_err(|source| StorageError::io("inspect storage document", path, source))?;
    if metadata.len() > maximum_bytes as u64 {
        return Err(StorageError::DocumentTooLarge {
            path: path.to_path_buf(),
            maximum_bytes,
        });
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take((maximum_bytes + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| StorageError::io("read storage document", path, source))?;
    if bytes.len() > maximum_bytes {
        return Err(StorageError::DocumentTooLarge {
            path: path.to_path_buf(),
            maximum_bytes,
        });
    }

    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|source| StorageError::InvalidDocument {
            path: path.to_path_buf(),
            message: source.to_string(),
        })
}

pub(crate) fn write_json<T>(
    path: &Path,
    value: &T,
    maximum_bytes: usize,
) -> Result<(), StorageError>
where
    T: Serialize,
{
    let mut bytes =
        serde_json::to_vec_pretty(value).map_err(|source| StorageError::InvalidDocument {
            path: path.to_path_buf(),
            message: source.to_string(),
        })?;
    bytes.push(b'\n');
    if bytes.len() > maximum_bytes {
        return Err(StorageError::DocumentTooLarge {
            path: path.to_path_buf(),
            maximum_bytes,
        });
    }

    validate_existing_file(path)?;
    let parent = path.parent().ok_or_else(|| StorageError::MissingParent {
        path: path.to_path_buf(),
    })?;
    validate_private_directory(parent)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".bone-workspace-")
        .tempfile_in(parent)
        .map_err(|source| StorageError::io("create temporary storage document", parent, source))?;
    set_private_file_permissions(temporary.as_file(), temporary.path())?;
    temporary.write_all(&bytes).map_err(|source| {
        StorageError::io("write temporary storage document", temporary.path(), source)
    })?;
    temporary.as_file().sync_all().map_err(|source| {
        StorageError::io("sync temporary storage document", temporary.path(), source)
    })?;
    temporary
        .persist(path)
        .map_err(|error| StorageError::io("replace storage document", path, error.error))?;
    sync_directory(parent)
}

/// Read a bounded private file as raw bytes. This is used by append-only
/// journals, whose newline-delimited representation cannot use `read_json`.
/// The same file/symlink/permission checks as JSON documents apply.
pub(crate) fn read_private_bytes(
    path: &Path,
    maximum_bytes: usize,
) -> Result<Option<Vec<u8>>, StorageError> {
    let mut file = match open_existing_file(path)? {
        Some(file) => file,
        None => return Ok(None),
    };
    let metadata = file
        .metadata()
        .map_err(|source| StorageError::io("inspect storage document", path, source))?;
    if metadata.len() > maximum_bytes as u64 {
        return Err(StorageError::DocumentTooLarge {
            path: path.to_path_buf(),
            maximum_bytes,
        });
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take((maximum_bytes + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| StorageError::io("read storage document", path, source))?;
    if bytes.len() > maximum_bytes {
        return Err(StorageError::DocumentTooLarge {
            path: path.to_path_buf(),
            maximum_bytes,
        });
    }
    Ok(Some(bytes))
}

/// Append one already-serialized record and make the append durable before
/// returning. The caller holds the neighbouring `StoreLock`, which keeps
/// length/sequence allocation and append linearized across processes.
pub(crate) fn append_private_bytes(path: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    validate_existing_file(path)?;
    let parent = path.parent().ok_or_else(|| StorageError::MissingParent {
        path: path.to_path_buf(),
    })?;
    validate_private_directory(parent)?;

    let mut options = OpenOptions::new();
    options.append(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options
        .open(path)
        .map_err(|source| StorageError::io("open storage journal", path, source))?;
    set_private_file_permissions(&file, path)?;
    validate_open_file(&file, path)?;
    file.write_all(bytes)
        .map_err(|source| StorageError::io("append storage journal", path, source))?;
    file.sync_data()
        .map_err(|source| StorageError::io("sync storage journal", path, source))
}

pub(crate) struct StoreLock {
    file: File,
}

impl StoreLock {
    pub(crate) fn acquire(path: &Path) -> Result<Self, StorageError> {
        let parent = path.parent().ok_or_else(|| StorageError::MissingParent {
            path: path.to_path_buf(),
        })?;
        validate_private_directory(parent)?;

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
            .map_err(|source| StorageError::io("open storage lock", path, source))?;
        validate_open_file(&file, path)?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Self { file }),
            Err(source) if source.kind() == std::io::ErrorKind::WouldBlock => {
                Err(StorageError::Busy {
                    path: path.to_path_buf(),
                })
            }
            Err(source) => Err(StorageError::io("lock storage", path, source)),
        }
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn validate_private_directory(path: &Path) -> Result<(), StorageError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| StorageError::io("inspect private directory", path, source))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(StorageError::UnsafeStorage {
            path: path.to_path_buf(),
            reason: "directory must be a real directory, not a symlink".to_owned(),
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(StorageError::UnsafeStorage {
                path: path.to_path_buf(),
                reason: "directory must not grant group or other access".to_owned(),
            });
        }
    }
    Ok(())
}

fn open_existing_file(path: &Path) -> Result<Option<File>, StorageError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() || metadata.file_type().is_symlink() => {
            return Err(StorageError::UnsafeStorage {
                path: path.to_path_buf(),
                reason: "storage document must be a regular file, not a symlink".to_owned(),
            });
        }
        Ok(_) => {}
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(StorageError::io("inspect storage document", path, source)),
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
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(StorageError::io("open storage document", path, source)),
    };
    validate_open_file(&file, path)?;
    Ok(Some(file))
}

fn validate_existing_file(path: &Path) -> Result<(), StorageError> {
    let _ = open_existing_file(path)?;
    Ok(())
}

fn validate_open_file(file: &File, path: &Path) -> Result<(), StorageError> {
    let metadata = file
        .metadata()
        .map_err(|source| StorageError::io("inspect storage file", path, source))?;
    if !metadata.file_type().is_file() {
        return Err(StorageError::UnsafeStorage {
            path: path.to_path_buf(),
            reason: "storage document must be a regular file".to_owned(),
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;

        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(StorageError::UnsafeStorage {
                path: path.to_path_buf(),
                reason: "storage file must not grant group or other access".to_owned(),
            });
        }
        if metadata.nlink() != 1 {
            return Err(StorageError::UnsafeStorage {
                path: path.to_path_buf(),
                reason: "storage file must not have hard links".to_owned(),
            });
        }
    }
    Ok(())
}

fn set_private_file_permissions(file: &File, path: &Path) -> Result<(), StorageError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|source| StorageError::io("set private file permissions", path, source))?;
    }
    #[cfg(not(unix))]
    let _ = (file, path);
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), StorageError> {
    #[cfg(unix)]
    {
        let directory = File::open(path)
            .map_err(|source| StorageError::io("open private directory for sync", path, source))?;
        directory
            .sync_all()
            .map_err(|source| StorageError::io("sync private directory", path, source))?;
    }
    Ok(())
}
