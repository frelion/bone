//! App-owned placement for BONE's SQLite data.
//!
//! `bone-store` accepts an explicit directory; this module selects BONE's
//! conventional user-data location at the application composition boundary.

use std::{env, ffi::OsString, path::PathBuf};

use bone_store::{BoneStore, StoreError, StoreRoots};
use thiserror::Error;

const APP_DIRECTORY: &str = "bone";
const STORE_DIRECTORY: &str = "store-v1";

/// Opens BONE's one conventional local SQLite store.
pub fn open_default_store() -> Result<BoneStore, AppStorageError> {
    let roots = default_store_roots(env::var_os("XDG_DATA_HOME"), env::var_os("HOME"))?;
    Ok(BoneStore::open_at(roots)?)
}

fn default_store_roots(
    xdg_data_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<StoreRoots, AppStorageError> {
    let data_home = absolute_path(xdg_data_home)
        .or_else(|| absolute_path(home).map(|home| home.join(".local/share")))
        .ok_or(AppStorageError::MissingDefaultDataRoot)?;
    Ok(StoreRoots::new(
        data_home.join(APP_DIRECTORY).join(STORE_DIRECTORY),
    )?)
}

fn absolute_path(value: Option<OsString>) -> Option<PathBuf> {
    value
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

/// Failures while choosing or opening the BONE application data store.
#[derive(Debug, Error)]
pub enum AppStorageError {
    #[error("could not determine BONE's default local data directory")]
    MissingDefaultDataRoot,
    #[error(transparent)]
    Store(#[from] StoreError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_data_home_wins_when_absolute() {
        let roots = default_store_roots(
            Some(OsString::from("/tmp/xdg-data")),
            Some(OsString::from("/tmp/home")),
        )
        .unwrap();
        assert_eq!(
            roots.data_root(),
            PathBuf::from("/tmp/xdg-data/bone/store-v1")
        );
    }

    #[test]
    fn home_is_the_fallback_for_missing_or_relative_xdg_data_home() {
        let roots = default_store_roots(
            Some(OsString::from("relative")),
            Some(OsString::from("/tmp/home")),
        )
        .unwrap();
        assert_eq!(
            roots.data_root(),
            PathBuf::from("/tmp/home/.local/share/bone/store-v1")
        );
    }

    #[test]
    fn no_absolute_data_location_is_an_explicit_error() {
        assert!(matches!(
            default_store_roots(None, Some(OsString::from("relative"))),
            Err(AppStorageError::MissingDefaultDataRoot)
        ));
    }
}
