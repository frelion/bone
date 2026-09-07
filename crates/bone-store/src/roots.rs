use std::path::{Path, PathBuf};

use crate::StoreError;

/// The directory that contains one local SQLite database and its lease files.
///
/// The application chooses this root. Tests and embedding hosts can provide
/// any explicit absolute directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreRoots {
    data_root: PathBuf,
}

impl StoreRoots {
    pub fn new(data_root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let data_root = data_root.into();
        if !data_root.is_absolute() {
            return Err(StoreError::RelativeRoot { path: data_root });
        }
        Ok(Self { data_root })
    }

    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub fn database_path(&self) -> PathBuf {
        self.data_root.join("bone.sqlite3")
    }

    pub(crate) fn with_data_root(&self, data_root: PathBuf) -> Self {
        Self { data_root }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_root_must_be_absolute() {
        assert!(matches!(
            StoreRoots::new("relative"),
            Err(StoreError::RelativeRoot { .. })
        ));
    }
}
