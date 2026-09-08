use std::{
    fs::{self, File, OpenOptions},
    io::ErrorKind,
    path::{Path, PathBuf},
};

use fs2::FileExt;

use super::StoreError;

/// Create a private directory if it is absent, then return its canonical path.
///
/// The store only owns this directory. It deliberately does not police every
/// ancestor: XDG and home directories are owned by the host environment.
pub(crate) fn ensure_private_directory(path: &Path) -> Result<PathBuf, StoreError> {
    match fs::symlink_metadata(path) {
        Ok(_) => validate_private_directory(path)?,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            fs::create_dir_all(path)
                .map_err(|error| StoreError::io("create store directory", path, error))?;
            set_private_directory_permissions(path)?;
            validate_private_directory(path)?;
        }
        Err(error) => return Err(StoreError::io("inspect store directory", path, error)),
    }
    fs::canonicalize(path).map_err(|error| StoreError::io("resolve store directory", path, error))
}

pub(crate) fn validate_private_directory(path: &Path) -> Result<(), StoreError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| StoreError::io("inspect store directory", path, error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(unsafe_storage(path, "must be a real directory"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        if metadata.uid() != effective_uid() {
            return Err(unsafe_storage(path, "must be owned by the current user"));
        }
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(unsafe_storage(path, "must not grant group or other access"));
        }
    }
    Ok(())
}

/// Ensure a private, regular file exists and tell the caller whether it was
/// created by this call.
pub(crate) fn ensure_private_file(path: &Path) -> Result<FileDisposition, StoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| unsafe_storage(path, "file has no parent directory"))?;
    validate_private_directory(parent)?;

    match fs::symlink_metadata(path) {
        Ok(_) => {
            validate_private_file(path)?;
            Ok(FileDisposition::Existing)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => match open_private_file(path, true) {
            Ok(file) => {
                file.sync_all()
                    .map_err(|error| StoreError::io("sync store file", path, error))?;
                Ok(FileDisposition::Created)
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                validate_private_file(path)?;
                Ok(FileDisposition::Existing)
            }
            Err(error) => Err(StoreError::io("create store file", path, error)),
        },
        Err(error) => Err(StoreError::io("inspect store file", path, error)),
    }
}

pub(crate) fn validate_private_file(path: &Path) -> Result<(), StoreError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| StoreError::io("inspect store file", path, error))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(unsafe_storage(path, "must be a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        if metadata.uid() != effective_uid() {
            return Err(unsafe_storage(path, "must be owned by the current user"));
        }
        if metadata.nlink() != 1 {
            return Err(unsafe_storage(path, "must not have hard links"));
        }
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(unsafe_storage(path, "must not grant group or other access"));
        }
    }
    Ok(())
}

pub(crate) fn try_acquire_private_lock(path: &Path) -> Result<File, StoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| unsafe_storage(path, "lock has no parent directory"))?;
    validate_private_directory(parent)?;
    let file = match fs::symlink_metadata(path) {
        Ok(_) => {
            validate_private_file(path)?;
            open_private_file(path, false)
                .map_err(|error| StoreError::io("open lease file", path, error))?
        }
        Err(error) if error.kind() == ErrorKind::NotFound => match open_private_file(path, true) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                validate_private_file(path)?;
                open_private_file(path, false)
                    .map_err(|error| StoreError::io("open lease file", path, error))?
            }
            Err(error) => return Err(StoreError::io("create lease file", path, error)),
        },
        Err(error) => return Err(StoreError::io("inspect lease file", path, error)),
    };
    match file.try_lock_exclusive() {
        Ok(()) => Ok(file),
        Err(error) if error.kind() == ErrorKind::WouldBlock => Err(StoreError::Busy),
        Err(error) => Err(StoreError::io("acquire lease", path, error)),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FileDisposition {
    Created,
    Existing,
}

fn open_private_file(path: &Path, create_new: bool) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if create_new {
        options.create_new(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn set_private_directory_permissions(path: &Path) -> Result<(), StoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| StoreError::io("set store directory permissions", path, error))?;
    }
    Ok(())
}

#[cfg(unix)]
fn effective_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

fn unsafe_storage(path: &Path, reason: &'static str) -> StoreError {
    StoreError::UnsafeStorage {
        path: path.to_path_buf(),
        reason,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn creates_a_private_nested_directory() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = ensure_private_directory(&temporary.path().join("one/two")).unwrap();
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_world_readable_file() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = ensure_private_directory(&temporary.path().join("private")).unwrap();
        let file = directory.join("file");
        fs::write(&file, b"data").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();

        assert!(matches!(
            validate_private_file(&file),
            Err(StoreError::UnsafeStorage { .. })
        ));
    }
}
