//! Private local-file helpers used only for store roots and provider OAuth
//! files. BONE documents and journals themselves live in SQLite.

use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

use fs2::FileExt;

use crate::StoreError;

#[derive(Clone, Copy)]
pub(crate) enum OpenDisposition {
    Existing,
    CreateNew,
}

/// Create an application-owned directory tree, or validate an existing final
/// directory. The final directory is strictly private and current-user owned;
/// ancestors must be trusted but may remain normally readable (for example
/// `~/.local/share`).
pub(crate) fn ensure_private_directory(path: &Path) -> Result<PathBuf, StoreError> {
    if !path.is_absolute() {
        return Err(StoreError::RelativeRoot {
            path: path.to_path_buf(),
        });
    }

    let mut missing = Vec::<OsString>::new();
    let mut existing = path;
    loop {
        match fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                let component = existing
                    .file_name()
                    .ok_or_else(|| unsafe_storage(existing, "directory has no name"))?;
                missing.push(component.to_owned());
                existing = existing
                    .parent()
                    .ok_or_else(|| unsafe_storage(existing, "directory has no parent"))?;
            }
            Err(error) => return Err(StoreError::io("inspect directory", existing, error)),
        }
    }

    if missing.is_empty() {
        validate_private_directory(existing)?;
        let canonical = fs::canonicalize(existing)
            .map_err(|error| StoreError::io("resolve directory", existing, error))?;
        validate_ancestor_directories(&canonical)?;
        return Ok(canonical);
    }

    let mut current = canonicalize_trusted_directory(existing)?;
    for component in missing.into_iter().rev() {
        current.push(component);
        match create_private_directory(&current) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(StoreError::io("create private directory", &current, error)),
        }
        validate_private_directory(&current)?;
        current = fs::canonicalize(&current)
            .map_err(|error| StoreError::io("resolve private directory", &current, error))?;
        validate_ancestor_directories(&current)?;
    }
    Ok(current)
}

/// Validate a root that must already exist and be strictly private. Unlike
/// `ensure_private_directory`, this never creates it.
pub(crate) fn validate_existing_private_directory(path: &Path) -> Result<PathBuf, StoreError> {
    if !path.is_absolute() {
        return Err(StoreError::RelativeRoot {
            path: path.to_path_buf(),
        });
    }
    validate_private_directory(path)?;
    let canonical =
        fs::canonicalize(path).map_err(|error| StoreError::io("resolve directory", path, error))?;
    validate_ancestor_directories(&canonical)?;
    Ok(canonical)
}

pub(crate) fn validate_private_directory(path: &Path) -> Result<(), StoreError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| StoreError::io("inspect private directory", path, error))?;
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

pub(crate) fn validate_private_file(path: &Path) -> Result<(), StoreError> {
    let file = secure_open(path, OpenDisposition::Existing)?;
    drop(file);
    Ok(())
}

/// Open a private regular file while rejecting symlinks and hard links. A
/// missing file is represented by `Ok(None)` only for `Existing`.
pub(crate) fn secure_open(path: &Path, disposition: OpenDisposition) -> Result<File, StoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| unsafe_storage(path, "file has no parent directory"))?;
    validate_private_directory(parent)?;

    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(unsafe_storage(
                path,
                "must be a regular file, not a symlink",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {
            if matches!(disposition, OpenDisposition::Existing) {
                return Err(StoreError::io("open private file", path, error));
            }
        }
        Err(error) => return Err(StoreError::io("inspect private file", path, error)),
    }

    let mut options = OpenOptions::new();
    options.read(true).write(true);
    match disposition {
        OpenDisposition::Existing => {}
        OpenDisposition::CreateNew => {
            options.create_new(true);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|error| StoreError::io("open private file", path, error))?;
    validate_open_private_file(&file, path)?;
    Ok(file)
}

pub(crate) fn existing_private_file(path: &Path) -> Result<Option<File>, StoreError> {
    match fs::symlink_metadata(path) {
        Ok(_) => secure_open(path, OpenDisposition::Existing).map(Some),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(StoreError::io("inspect private file", path, error)),
    }
}

pub(crate) fn create_private_file(path: &Path) -> Result<Option<File>, StoreError> {
    match secure_open(path, OpenDisposition::CreateNew) {
        Ok(file) => Ok(Some(file)),
        Err(StoreError::Io { source, .. }) if source.kind() == ErrorKind::AlreadyExists => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) fn try_acquire_private_lock(path: &Path) -> Result<File, StoreError> {
    let file = match existing_private_file(path)? {
        Some(file) => file,
        None => match create_private_file(path)? {
            Some(file) => file,
            None => secure_open(path, OpenDisposition::Existing)?,
        },
    };
    match file.try_lock_exclusive() {
        Ok(()) => Ok(file),
        Err(error) if error.kind() == ErrorKind::WouldBlock => Err(StoreError::Busy),
        Err(error) => Err(StoreError::io("lock private file", path, error)),
    }
}

pub(crate) fn write_new_private_file(path: &Path, bytes: &[u8]) -> Result<bool, StoreError> {
    let Some(mut file) = create_private_file(path)? else {
        return Ok(false);
    };
    file.write_all(bytes)
        .map_err(|error| StoreError::io("write private file", path, error))?;
    file.sync_all()
        .map_err(|error| StoreError::io("sync private file", path, error))?;
    Ok(true)
}

pub(crate) fn sync_directory(path: &Path) -> Result<(), StoreError> {
    #[cfg(unix)]
    {
        let directory = File::open(path)
            .map_err(|error| StoreError::io("open private directory for sync", path, error))?;
        directory
            .sync_all()
            .map_err(|error| StoreError::io("sync private directory", path, error))?;
    }
    Ok(())
}

pub(crate) fn validate_ancestor_directories(path: &Path) -> Result<(), StoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        for ancestor in path.ancestors().skip(1) {
            let metadata = fs::metadata(ancestor)
                .map_err(|error| StoreError::io("inspect directory ancestor", ancestor, error))?;
            let mode = metadata.permissions().mode();
            if !metadata.is_dir()
                || (metadata.uid() != 0 && metadata.uid() != effective_uid())
                || (mode & 0o022 != 0 && mode & 0o1000 == 0)
            {
                return Err(unsafe_storage(
                    ancestor,
                    "ancestor directory is not trusted",
                ));
            }
        }
    }
    Ok(())
}

fn canonicalize_trusted_directory(path: &Path) -> Result<PathBuf, StoreError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| StoreError::io("inspect directory", path, error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(unsafe_storage(path, "must be a real directory"));
    }
    let canonical =
        fs::canonicalize(path).map_err(|error| StoreError::io("resolve directory", path, error))?;
    // This is the nearest existing directory above a newly-created BONE
    // directory tree. It is not enough to validate only *its* ancestors: a
    // group-writable, non-sticky directory at this exact location would let
    // another user replace entries while we create the private descendants.
    validate_trusted_directory_and_ancestors(&canonical)?;
    Ok(canonical)
}

fn validate_trusted_directory_and_ancestors(path: &Path) -> Result<(), StoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        for directory in path.ancestors() {
            let metadata = fs::metadata(directory)
                .map_err(|error| StoreError::io("inspect directory ancestor", directory, error))?;
            let mode = metadata.permissions().mode();
            if !metadata.is_dir()
                || (metadata.uid() != 0 && metadata.uid() != effective_uid())
                || (mode & 0o022 != 0 && mode & 0o1000 == 0)
            {
                return Err(unsafe_storage(
                    directory,
                    "ancestor directory is not trusted",
                ));
            }
        }
    }
    Ok(())
}

fn create_private_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        fs::create_dir(path)
    }
}

fn validate_open_private_file(file: &File, path: &Path) -> Result<(), StoreError> {
    let metadata = file
        .metadata()
        .map_err(|error| StoreError::io("inspect private file", path, error))?;
    if !metadata.is_file() {
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

#[cfg(unix)]
fn effective_uid() -> u32 {
    // SAFETY: `geteuid` has no preconditions and only reads process metadata.
    unsafe { libc::geteuid() }
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
    fn creates_private_nested_directory() {
        let temporary = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        fs::set_permissions(
            temporary.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let directory = ensure_private_directory(&temporary.path().join("one/two")).unwrap();
        assert!(directory.is_absolute());
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_group_writable_existing_parent_when_creating_a_root() {
        let temporary = tempfile::tempdir().unwrap();
        fs::set_permissions(
            temporary.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let parent = temporary.path().join("group-writable-parent");
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, std::os::unix::fs::PermissionsExt::from_mode(0o770)).unwrap();

        assert!(matches!(
            ensure_private_directory(&parent.join("bone")),
            Err(StoreError::UnsafeStorage { .. })
        ));
    }
}
