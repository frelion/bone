//! Small filesystem safety primitives used by App-owned configuration.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind, Read, Write},
    path::{Path, PathBuf},
};

use fs2::FileExt;
use tempfile::NamedTempFile;

#[derive(Debug)]
pub(crate) enum SafeFileError {
    Io(io::Error),
    Unsafe(&'static str),
}

impl From<io::Error> for SafeFileError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub(crate) fn ensure_private_directory(path: &Path) -> Result<PathBuf, SafeFileError> {
    match fs::symlink_metadata(path) {
        Ok(_) => validate_directory(path)?,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            fs::create_dir_all(path)?;
            validate_directory(path)?;
        }
        Err(error) => return Err(error.into()),
    }
    validate_owner(path)?;
    set_mode(path, 0o700)?;
    Ok(fs::canonicalize(path)?)
}

pub(crate) fn private_directory_exists(path: &Path) -> Result<bool, SafeFileError> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            validate_directory(path)?;
            validate_owner(path)?;
            validate_private_mode(path, 0o077)?;
            Ok(true)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn validate_private_file(path: &Path) -> Result<(), SafeFileError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(SafeFileError::Unsafe("must be a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        if metadata.uid() != rustix::process::geteuid().as_raw() {
            return Err(SafeFileError::Unsafe("must be owned by the current user"));
        }
        if metadata.nlink() != 1 {
            return Err(SafeFileError::Unsafe("must not have hard links"));
        }
        if metadata.permissions().mode() & 0o7777 != 0o600 {
            return Err(SafeFileError::Unsafe("must have mode 0600"));
        }
    }
    Ok(())
}

pub(crate) fn read_regular_file(path: &Path) -> Result<Option<Vec<u8>>, SafeFileError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !file.metadata()?.is_file() {
        return Err(SafeFileError::Unsafe("must be a regular file"));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

pub(crate) fn open_existing_private_file(path: &Path) -> Result<Option<File>, SafeFileError> {
    match fs::symlink_metadata(path) {
        Ok(_) => validate_private_file(path)?,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    validate_open_private_file(&file)?;
    Ok(Some(file))
}

pub(crate) fn create_private_file(path: &Path) -> Result<Option<File>, SafeFileError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    match options.open(path) {
        Ok(file) => {
            set_file_mode(&file, 0o600)?;
            validate_open_private_file(&file)?;
            Ok(Some(file))
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn lock_private_file(path: &Path) -> Result<File, SafeFileError> {
    let file = match open_existing_private_file(path)? {
        Some(file) => file,
        None => match create_private_file(path)? {
            Some(file) => file,
            None => open_existing_private_file(path)?
                .ok_or(SafeFileError::Unsafe("lock disappeared while opening"))?,
        },
    };
    file.lock_exclusive()?;
    Ok(file)
}

#[cfg(unix)]
pub(crate) fn lock_directory(path: &Path) -> Result<File, SafeFileError> {
    use rustix::fs::{Mode, OFlags, open};

    let directory = open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::DIRECTORY | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(io::Error::from)?;
    directory.lock_exclusive()?;
    Ok(directory)
}

pub(crate) fn atomic_write_private(path: &Path, bytes: &[u8]) -> Result<(), SafeFileError> {
    let parent = path
        .parent()
        .ok_or(SafeFileError::Unsafe("file has no parent"))?;
    ensure_private_directory(parent)?;
    atomic_write(path, bytes, true)
}

pub(crate) fn atomic_write_new_private(path: &Path, bytes: &[u8]) -> Result<bool, SafeFileError> {
    let parent = path
        .parent()
        .ok_or(SafeFileError::Unsafe("file has no parent"))?;
    ensure_private_directory(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    set_mode(temporary.path(), 0o600)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(path) {
        Ok(file) => {
            validate_open_private_file(&file)?;
            sync_directory(parent)?;
            Ok(true)
        }
        Err(error) if error.error.kind() == ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error.error.into()),
    }
}

pub(crate) fn atomic_write_regular(path: &Path, bytes: &[u8]) -> Result<(), SafeFileError> {
    atomic_write(path, bytes, false)
}

fn atomic_write(path: &Path, bytes: &[u8], private: bool) -> Result<(), SafeFileError> {
    let parent = path
        .parent()
        .ok_or(SafeFileError::Unsafe("file has no parent"))?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    if private {
        set_mode(temporary.path(), 0o600)?;
    }
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    let file = temporary.persist(path).map_err(|error| error.error)?;
    if private {
        validate_open_private_file(&file)?;
    }
    sync_directory(parent)
}

pub(crate) fn sync_directory(path: &Path) -> Result<(), SafeFileError> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    Ok(())
}

fn validate_directory(path: &Path) -> Result<(), SafeFileError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(SafeFileError::Unsafe("must be a real directory"));
    }
    Ok(())
}

fn validate_open_private_file(file: &File) -> Result<(), SafeFileError> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(SafeFileError::Unsafe("must be a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != rustix::process::geteuid().as_raw() {
            return Err(SafeFileError::Unsafe("must be owned by the current user"));
        }
        if metadata.nlink() != 1 {
            return Err(SafeFileError::Unsafe("must not have hard links"));
        }
        if metadata.permissions().mode() & 0o7777 != 0o600 {
            return Err(SafeFileError::Unsafe("must have mode 0600"));
        }
    }
    Ok(())
}

fn validate_owner(path: &Path) -> Result<(), SafeFileError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if fs::symlink_metadata(path)?.uid() != rustix::process::geteuid().as_raw() {
            return Err(SafeFileError::Unsafe("must be owned by the current user"));
        }
    }
    Ok(())
}

fn validate_private_mode(path: &Path, forbidden: u32) -> Result<(), SafeFileError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::symlink_metadata(path)?.permissions().mode() & forbidden != 0 {
            return Err(SafeFileError::Unsafe(
                "must not grant group or other access",
            ));
        }
    }
    let _ = forbidden;
    Ok(())
}

fn set_mode(path: &Path, mode: u32) -> Result<(), SafeFileError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    let _ = mode;
    Ok(())
}

fn set_file_mode(file: &File, mode: u32) -> Result<(), SafeFileError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(mode))?;
    }
    let _ = mode;
    Ok(())
}
