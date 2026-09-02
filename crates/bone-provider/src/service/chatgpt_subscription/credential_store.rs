use std::path::PathBuf;

use super::Error;

#[cfg(unix)]
use std::{
    ffi::OsString,
    fs::{self, DirBuilder, File, OpenOptions},
    io::Write,
    path::Path,
};

#[cfg(unix)]
use fs2::FileExt;

#[cfg(unix)]
const APP_DIRECTORY: &str = "bone";
#[cfg(unix)]
const SERVICE_DIRECTORY: &str = "chatgpt-subscription";
#[cfg(unix)]
const AUTH_FILE: &str = "auth.json";

pub(super) fn prepare_default() -> Result<(PathBuf, CredentialLease), Error> {
    #[cfg(unix)]
    {
        let path = auth_file_path_from(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
            true,
        )?;
        let lease = prepare(&path)?;
        Ok((path, lease))
    }

    #[cfg(not(unix))]
    {
        Err(Error::UnsupportedPlatform)
    }
}

pub(super) fn disconnect_default() -> Result<(), Error> {
    #[cfg(unix)]
    {
        let path = auth_file_path_from(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
            false,
        )?;
        disconnect_at(&path)
    }

    #[cfg(not(unix))]
    {
        Err(Error::UnsupportedPlatform)
    }
}

#[cfg(unix)]
pub(super) fn auth_file_path_from(
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
    create_root: bool,
) -> Result<PathBuf, Error> {
    let xdg_root = xdg_config_home
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());
    let root = match xdg_root {
        Some(root) => root,
        None => home
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .map(|path| path.join(".config"))
            .ok_or(Error::CredentialStoreUnavailable)?,
    };

    let root = if create_root {
        ensure_config_root(&root)?
    } else {
        match fs::symlink_metadata(&root) {
            Ok(_) => canonicalize_directory(&root)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => root,
            Err(_) => return Err(Error::CredentialStoreUnavailable),
        }
    };

    Ok(root
        .join(APP_DIRECTORY)
        .join(SERVICE_DIRECTORY)
        .join(AUTH_FILE))
}

#[cfg(unix)]
fn ensure_config_root(path: &Path) -> Result<PathBuf, Error> {
    match fs::symlink_metadata(path) {
        Ok(_) => canonicalize_directory(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => create_config_root(path),
        Err(_) => Err(Error::CredentialStoreUnavailable),
    }
}

#[cfg(unix)]
fn create_config_root(path: &Path) -> Result<PathBuf, Error> {
    let mut missing = Vec::new();
    let mut existing = path;
    loop {
        match fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(
                    existing
                        .file_name()
                        .ok_or(Error::CredentialStoreUnavailable)?
                        .to_owned(),
                );
                existing = existing.parent().ok_or(Error::CredentialStoreUnavailable)?;
            }
            Err(_) => return Err(Error::CredentialStoreUnavailable),
        }
    }

    let mut current = canonicalize_directory(existing)?;
    for component in missing.into_iter().rev() {
        current.push(component);
        create_private_directory(&current)?;
        current = canonicalize_directory(&current)?;
    }
    Ok(current)
}

#[cfg(unix)]
fn canonicalize_directory(path: &Path) -> Result<PathBuf, Error> {
    validate_directory(path)?;
    let canonical = fs::canonicalize(path).map_err(|_| Error::CredentialStoreUnavailable)?;
    validate_directory(&canonical)?;
    validate_ancestor_directories(&canonical)?;
    Ok(canonical)
}

#[cfg(unix)]
fn validate_ancestor_directories(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    for ancestor in path.ancestors().skip(1) {
        let metadata = fs::metadata(ancestor).map_err(|_| Error::CredentialStoreUnavailable)?;
        let mode = metadata.permissions().mode();
        let owner = metadata.uid();
        if !metadata.is_dir()
            || (owner != 0 && owner != effective_uid())
            || (mode & 0o022 != 0 && mode & 0o1000 == 0)
        {
            return Err(Error::CredentialStoreUnavailable);
        }
    }
    Ok(())
}

#[cfg(unix)]
pub(super) fn prepare(path: &Path) -> Result<CredentialLease, Error> {
    let lease = acquire(path)?;

    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(Error::CredentialStoreUnavailable);
        }
        Ok(_) => drop(secure_open(path, OpenDisposition::Existing)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut file = secure_open(path, OpenDisposition::CreateNew)?;
            file.write_all(b"{}")
                .and_then(|_| file.sync_all())
                .map_err(|_| Error::CredentialStoreUnavailable)?;
        }
        Err(_) => return Err(Error::CredentialStoreUnavailable),
    }

    set_file_permissions(path)?;
    Ok(lease)
}

#[cfg(not(unix))]
#[derive(Debug)]
pub(super) struct CredentialLease;

#[cfg(unix)]
#[derive(Debug)]
pub(super) struct CredentialLease {
    _file: File,
}

#[cfg(unix)]
pub(super) fn disconnect_at(path: &Path) -> Result<(), Error> {
    let (config_root, app_dir, service_dir) = path_hierarchy(path)?;
    if !validate_if_present(config_root)? {
        return Ok(());
    }
    if !validate_if_present(app_dir)? {
        return Ok(());
    }
    if !validate_if_present(service_dir)? {
        return Ok(());
    }

    let _lease = acquire(path)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(Error::CredentialStoreUnavailable)
        }
        Ok(_) => {
            drop(secure_open(path, OpenDisposition::Existing)?);
            fs::remove_file(path).map_err(|_| Error::CredentialStoreUnavailable)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(Error::CredentialStoreUnavailable),
    }
}

#[cfg(unix)]
fn validate_if_present(path: &Path) -> Result<bool, Error> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            validate_directory(path)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(Error::CredentialStoreUnavailable),
    }
}

#[cfg(unix)]
fn acquire(path: &Path) -> Result<CredentialLease, Error> {
    let (config_root, app_dir, service_dir) = path_hierarchy(path)?;
    validate_directory(config_root)?;
    ensure_private_directory(app_dir)?;
    ensure_private_directory(service_dir)?;

    let lock_path = service_dir.join("auth.lock");
    let lock_file = secure_open(&lock_path, OpenDisposition::Create)?;
    match lock_file.try_lock_exclusive() {
        Ok(()) => Ok(CredentialLease { _file: lock_file }),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
            Err(Error::CredentialStoreBusy)
        }
        Err(_) => Err(Error::CredentialStoreUnavailable),
    }
}

#[cfg(unix)]
fn path_hierarchy(path: &Path) -> Result<(&Path, &Path, &Path), Error> {
    if !path.is_absolute() {
        return Err(Error::CredentialStoreUnavailable);
    }
    let service_dir = path.parent().ok_or(Error::CredentialStoreUnavailable)?;
    let app_dir = service_dir
        .parent()
        .ok_or(Error::CredentialStoreUnavailable)?;
    let config_root = app_dir.parent().ok_or(Error::CredentialStoreUnavailable)?;
    Ok((config_root, app_dir, service_dir))
}

#[cfg(unix)]
fn ensure_private_directory(path: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(path) {
        Ok(_) => validate_directory(path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            create_private_directory(path)?;
            validate_directory(path)?;
        }
        Err(_) => return Err(Error::CredentialStoreUnavailable),
    }
    set_directory_permissions(path)
}

#[cfg(unix)]
fn validate_directory(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let metadata = fs::symlink_metadata(path).map_err(|_| Error::CredentialStoreUnavailable)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != effective_uid()
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err(Error::CredentialStoreUnavailable);
    }
    Ok(())
}

#[cfg(unix)]
fn create_private_directory(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::DirBuilderExt;

    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    builder
        .create(path)
        .map_err(|_| Error::CredentialStoreUnavailable)
}

#[cfg(unix)]
fn set_directory_permissions(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|_| Error::CredentialStoreUnavailable)
}

#[cfg(unix)]
fn set_file_permissions(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|_| Error::CredentialStoreUnavailable)
}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum OpenDisposition {
    Existing,
    Create,
    CreateNew,
}

#[cfg(unix)]
fn secure_open(path: &Path, disposition: OpenDisposition) -> Result<File, Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(Error::CredentialStoreUnavailable);
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(Error::CredentialStoreUnavailable),
    }

    let parent = path.parent().ok_or(Error::CredentialStoreUnavailable)?;
    validate_directory(parent)?;

    let mut options = OpenOptions::new();
    options.read(true).write(true);
    match disposition {
        OpenDisposition::Existing => {}
        OpenDisposition::Create => {
            options.create(true);
        }
        OpenDisposition::CreateNew => {
            options.create_new(true);
        }
    }

    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    let file = options
        .open(path)
        .map_err(|_| Error::CredentialStoreUnavailable)?;

    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let metadata = file
        .metadata()
        .map_err(|_| Error::CredentialStoreUnavailable)?;
    if metadata.nlink() != 1
        || metadata.uid() != effective_uid()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(Error::CredentialStoreUnavailable);
    }
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|_| Error::CredentialStoreUnavailable)?;

    Ok(file)
}

#[cfg(unix)]
pub(super) fn effective_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}
