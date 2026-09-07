use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
};

use crate::StoreError;

const APP_DIRECTORY: &str = "bone";
const STORE_DIRECTORY: &str = "store-v1";

/// Explicit filesystem roots for a local BONE store.
///
/// `data_root` is the directory that directly contains `bone.sqlite3`.
/// `config_root` is the directory that directly contains `providers/` for
/// provider-managed caches. Both must be absolute; neither is resolved from a
/// hidden BONE-specific environment variable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreRoots {
    data_root: PathBuf,
    config_root: PathBuf,
}

impl StoreRoots {
    pub fn new(
        data_root: impl Into<PathBuf>,
        config_root: impl Into<PathBuf>,
    ) -> Result<Self, StoreError> {
        let data_root = data_root.into();
        let config_root = config_root.into();
        if !data_root.is_absolute() {
            return Err(StoreError::RelativeRoot { path: data_root });
        }
        if !config_root.is_absolute() {
            return Err(StoreError::RelativeRoot { path: config_root });
        }
        Ok(Self {
            data_root,
            config_root,
        })
    }

    /// Resolve the conventional XDG roots without creating files or
    /// directories.
    pub fn default_for_current_user() -> Result<Self, StoreError> {
        let home = env::var_os("HOME");
        let data_root = xdg_or_home(env::var_os("XDG_DATA_HOME"), home.as_ref(), ".local/share")?;
        let config_root = xdg_or_home(env::var_os("XDG_CONFIG_HOME"), home.as_ref(), ".config")?;
        Self::new(
            data_root.join(APP_DIRECTORY).join(STORE_DIRECTORY),
            config_root.join(APP_DIRECTORY).join(STORE_DIRECTORY),
        )
    }

    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub fn config_root(&self) -> &Path {
        &self.config_root
    }

    pub fn database_path(&self) -> PathBuf {
        self.data_root.join("bone.sqlite3")
    }

    pub(crate) fn with_data_root(&self, data_root: PathBuf) -> Self {
        Self {
            data_root,
            config_root: self.config_root.clone(),
        }
    }
}

fn xdg_or_home(
    xdg: Option<OsString>,
    home: Option<&OsString>,
    fallback: &str,
) -> Result<PathBuf, StoreError> {
    if let Some(path) = xdg
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return Ok(path);
    }
    let home = home
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or(StoreError::MissingDefaultRoot)?;
    Ok(home.join(fallback))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_roots_must_be_absolute() {
        assert!(matches!(
            StoreRoots::new("relative", "/absolute"),
            Err(StoreError::RelativeRoot { .. })
        ));
        assert!(matches!(
            StoreRoots::new("/absolute", "relative"),
            Err(StoreError::RelativeRoot { .. })
        ));
    }
}
