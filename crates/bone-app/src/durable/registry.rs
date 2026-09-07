use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::{Deserialize, Serialize};

use super::{
    CanonicalPath, RegistryError, WorkspaceId,
    storage::{
        MAX_REGISTRY_BYTES, StoreLock, ensure_private_directory, lock_path, private_file_path,
        read_json, write_json,
    },
};

const REGISTRY_FORMAT_VERSION: u32 = 1;
const MAX_WORKSPACES: usize = 100_000;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RegistryDocument {
    format_version: u32,
    #[serde(default)]
    workspaces: BTreeMap<String, WorkspaceId>,
}

impl RegistryDocument {
    fn empty() -> Self {
        Self {
            format_version: REGISTRY_FORMAT_VERSION,
            workspaces: BTreeMap::new(),
        }
    }

    fn validate(&self) -> Result<(), RegistryError> {
        if self.format_version != REGISTRY_FORMAT_VERSION {
            return Err(RegistryError::UnsupportedFormat {
                version: self.format_version,
            });
        }
        if self.workspaces.len() > MAX_WORKSPACES {
            return Err(RegistryError::TooManyWorkspaces {
                maximum_entries: MAX_WORKSPACES,
            });
        }
        for (key, id) in &self.workspaces {
            if id.is_nil() || !key.starts_with("workspace/v1/") {
                return Err(RegistryError::InvalidWorkspaceId { key: key.clone() });
            }
        }
        Ok(())
    }
}

/// Stable, private mapping from canonical workspace roots to opaque UUIDs.
///
/// The registry is global user data. Its path is explicit and it never creates
/// files under a project workspace.
#[derive(Clone)]
pub struct WorkspaceRegistry {
    path: Arc<PathBuf>,
    lock_path: Arc<PathBuf>,
}

impl fmt::Debug for WorkspaceRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkspaceRegistry")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl WorkspaceRegistry {
    /// Open `state_root/workspaces.json`, creating `state_root` with private
    /// permissions if it does not yet exist.
    pub fn open_in(state_root: impl AsRef<Path>) -> Result<Self, RegistryError> {
        let state_root = ensure_private_directory(state_root.as_ref())?;
        Self::open(state_root.join("workspaces.json"))
    }

    /// Open an explicit registry document at an absolute private path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RegistryError> {
        let path = private_file_path(path.as_ref())?;
        let registry = Self {
            lock_path: Arc::new(lock_path(&path)),
            path: Arc::new(path),
        };
        let _lock = registry.acquire_lock()?;
        let _ = registry.read_document()?;
        Ok(registry)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Look up an already-known canonical root without allocating a new ID.
    ///
    /// Callers must pass the canonical absolute directory that defines their
    /// workspace. [`crate::WorkspaceContext::discover`] guarantees this.
    pub fn lookup_canonical(
        &self,
        canonical_root: &CanonicalPath,
    ) -> Result<Option<WorkspaceId>, RegistryError> {
        let key = WorkspaceKey::from_canonical(canonical_root);
        let _lock = self.acquire_lock()?;
        let document = self.read_document()?;
        Ok(document.workspaces.get(key.as_str()).copied())
    }

    /// Return a stable ID for a canonical root, allocating it under a short
    /// cross-process lock on first use.
    pub fn resolve_or_create_canonical(
        &self,
        canonical_root: &CanonicalPath,
    ) -> Result<WorkspaceId, RegistryError> {
        let key = WorkspaceKey::from_canonical(canonical_root);
        let _lock = self.acquire_lock()?;
        let mut document = self.read_document()?;
        if let Some(id) = document.workspaces.get(key.as_str()) {
            return Ok(*id);
        }
        if document.workspaces.len() >= MAX_WORKSPACES {
            return Err(RegistryError::TooManyWorkspaces {
                maximum_entries: MAX_WORKSPACES,
            });
        }

        let id = WorkspaceId::new();
        document.workspaces.insert(key.into_inner(), id);
        self.write_document(&document)?;
        Ok(id)
    }

    fn acquire_lock(&self) -> Result<StoreLock, RegistryError> {
        StoreLock::acquire(&self.lock_path).map_err(RegistryError::from)
    }

    fn read_document(&self) -> Result<RegistryDocument, RegistryError> {
        let document =
            read_json(&self.path, MAX_REGISTRY_BYTES)?.unwrap_or_else(RegistryDocument::empty);
        document.validate()?;
        Ok(document)
    }

    fn write_document(&self, document: &RegistryDocument) -> Result<(), RegistryError> {
        document.validate()?;
        write_json(&self.path, document, MAX_REGISTRY_BYTES)?;
        Ok(())
    }
}

/// Internal lossless, platform-namespaced registry key. It intentionally has
/// no `Display` implementation so a path-derived key is not casually emitted
/// into logs or telemetry.
#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
struct WorkspaceKey(String);

impl WorkspaceKey {
    fn from_canonical(path: &CanonicalPath) -> Self {
        Self(format!("workspace/v1/{}", path.storage_encoding()))
    }

    fn as_str(&self) -> &str {
        &self.0
    }

    fn into_inner(self) -> String {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::*;
    use crate::{CanonicalPath, StorageError};

    fn private_data() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        fs::set_permissions(
            directory.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        directory
    }

    #[test]
    fn registry_persists_workspace_ids_across_reopen() {
        let data = private_data();
        let workspace = tempfile::tempdir().unwrap();
        let path = data.path().join("workspaces.json");
        let canonical = CanonicalPath::new(fs::canonicalize(workspace.path()).unwrap()).unwrap();

        let initial = WorkspaceRegistry::open(&path).unwrap();
        let id = initial.resolve_or_create_canonical(&canonical).unwrap();
        drop(initial);

        let reopened = WorkspaceRegistry::open(&path).unwrap();
        assert_eq!(reopened.lookup_canonical(&canonical).unwrap(), Some(id));
        assert_eq!(
            reopened.resolve_or_create_canonical(&canonical).unwrap(),
            id
        );
    }

    #[test]
    fn registry_assigns_distinct_ids_to_distinct_canonical_roots() {
        let data = private_data();
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        let registry = WorkspaceRegistry::open_in(data.path()).unwrap();
        let left = CanonicalPath::new(fs::canonicalize(left.path()).unwrap()).unwrap();
        let right = CanonicalPath::new(fs::canonicalize(right.path()).unwrap()).unwrap();

        assert_ne!(
            registry.resolve_or_create_canonical(&left).unwrap(),
            registry.resolve_or_create_canonical(&right).unwrap()
        );
    }

    #[test]
    fn registry_rejects_bad_document_without_allocating_a_new_id() {
        let data = private_data();
        let path = data.path().join("workspaces.json");
        fs::write(&path, "{ not-json").unwrap();
        #[cfg(unix)]
        fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();

        assert!(matches!(
            WorkspaceRegistry::open(&path),
            Err(RegistryError::Storage(StorageError::InvalidDocument { .. }))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn registry_rejects_symlink_documents() {
        use std::os::unix::fs::symlink;

        let data = private_data();
        let target = data.path().join("target.json");
        let path = data.path().join("workspaces.json");
        fs::write(&target, "{}").unwrap();
        fs::set_permissions(&target, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
        symlink(&target, &path).unwrap();

        assert!(matches!(
            WorkspaceRegistry::open(&path),
            Err(RegistryError::Storage(StorageError::UnsafeStorage { .. }))
        ));
    }

    #[test]
    fn state_root_never_creates_a_workspace_dot_directory() {
        let data = private_data();
        let registry = WorkspaceRegistry::open_in(data.path()).unwrap();
        assert!(registry.path().starts_with(data.path()));
        assert!(!data.path().join(".bone").exists());
        assert_eq!(
            registry.path(),
            PathBuf::from(data.path()).join("workspaces.json")
        );
    }
}
