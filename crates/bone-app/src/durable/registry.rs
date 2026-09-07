use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use bone_store::{BoneStore, StoreError};
use serde::{Deserialize, Serialize};

use super::{CanonicalPath, RegistryError, WorkspaceId, keys};

const REGISTRY_RETRY_LIMIT: usize = 8;
const MAX_WORKSPACES: usize = 10_000;

/// The single typed document behind the workspace registry. It is deliberately
/// private: product code receives a `WorkspaceRegistry`, not a generic map.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct WorkspaceRegistryDocument {
    workspaces: BTreeMap<String, WorkspaceId>,
}

impl WorkspaceRegistryDocument {
    fn validate(&self) -> Result<(), RegistryError> {
        if self.workspaces.len() > MAX_WORKSPACES {
            return Err(RegistryError::TooManyWorkspaces {
                maximum_entries: MAX_WORKSPACES,
            });
        }

        let mut ids = BTreeSet::new();
        for (key, id) in &self.workspaces {
            if id.is_nil() {
                return Err(RegistryError::InvalidWorkspaceId { key: key.clone() });
            }
            if CanonicalPath::from_storage_encoding(key).is_err() {
                return Err(RegistryError::InvalidWorkspaceKey { key: key.clone() });
            }
            if !ids.insert(*id) {
                return Err(RegistryError::DuplicateWorkspaceId { id: *id });
            }
        }
        Ok(())
    }
}

/// Stable identities for the exact canonical directories where users launch
/// BONE. The registry is global BONE state, while all session documents stay
/// inside their own workspace namespace.
#[derive(Clone)]
pub struct WorkspaceRegistry {
    store: BoneStore,
}

impl fmt::Debug for WorkspaceRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkspaceRegistry")
            .finish_non_exhaustive()
    }
}

impl WorkspaceRegistry {
    pub(crate) fn new(store: BoneStore) -> Self {
        Self { store }
    }

    /// Look up an already allocated workspace ID without creating state.
    pub fn lookup_canonical(
        &self,
        canonical_path: &CanonicalPath,
    ) -> Result<Option<WorkspaceId>, RegistryError> {
        let document = self
            .store
            .document::<WorkspaceRegistryDocument>(keys::workspace_registry());
        let snapshot = document.read()?;
        let registry = snapshot.value.unwrap_or_default();
        registry.validate()?;
        Ok(registry
            .workspaces
            .get(&canonical_path.storage_encoding())
            .copied())
    }

    /// Return the stable ID for a canonical workspace, allocating it in the
    /// registry document exactly once with optimistic concurrency.
    pub fn resolve_or_create_canonical(
        &self,
        canonical_path: &CanonicalPath,
    ) -> Result<WorkspaceId, RegistryError> {
        let document = self
            .store
            .document::<WorkspaceRegistryDocument>(keys::workspace_registry());
        let key = canonical_path.storage_encoding();
        for _ in 0..REGISTRY_RETRY_LIMIT {
            let snapshot = document.read()?;
            let mut registry = snapshot.value.unwrap_or_default();
            registry.validate()?;
            if let Some(id) = registry.workspaces.get(&key) {
                return Ok(*id);
            }
            if registry.workspaces.len() >= MAX_WORKSPACES {
                return Err(RegistryError::TooManyWorkspaces {
                    maximum_entries: MAX_WORKSPACES,
                });
            }
            let id = WorkspaceId::new();
            registry.workspaces.insert(key.clone(), id);
            registry.validate()?;
            match document.replace(&registry, snapshot.revision) {
                Ok(_) => return Ok(id),
                Err(StoreError::RevisionConflict { .. }) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        // Repeated CAS contention is equivalent to a non-blocking store busy
        // result for this user-facing operation.
        Err(StoreError::Busy.into())
    }
}

#[cfg(test)]
mod tests {
    use bone_store::{BoneStore, Revision, StoreRoots};

    use super::*;

    fn store() -> (tempfile::TempDir, BoneStore) {
        let temporary = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(
            temporary.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let roots = StoreRoots::new(temporary.path().join("data")).unwrap();
        let store = BoneStore::open_at(roots).unwrap();
        (temporary, store)
    }

    #[test]
    fn maps_one_canonical_path_to_one_stable_id() {
        let (_temporary, store) = store();
        let registry = WorkspaceRegistry::new(store.clone());
        let path = CanonicalPath::new("/tmp/bone-registry-test").unwrap();
        let first = registry.resolve_or_create_canonical(&path).unwrap();
        assert_eq!(registry.resolve_or_create_canonical(&path).unwrap(), first);
        assert_eq!(registry.lookup_canonical(&path).unwrap(), Some(first));
    }

    #[test]
    fn rejects_malformed_registry_keys_before_lookup_or_allocation() {
        let (_temporary, store) = store();
        let document = store.document::<WorkspaceRegistryDocument>(keys::workspace_registry());
        let mut workspaces = BTreeMap::new();
        workspaces.insert("not-a-canonical-path".to_owned(), WorkspaceId::new());
        document
            .replace(
                &WorkspaceRegistryDocument { workspaces },
                Revision::default(),
            )
            .unwrap();

        let registry = WorkspaceRegistry::new(store.clone());
        let path = CanonicalPath::new("/tmp/bone-registry-test").unwrap();
        assert!(matches!(
            registry.lookup_canonical(&path),
            Err(RegistryError::InvalidWorkspaceKey { .. })
        ));
    }

    #[test]
    fn rejects_nil_or_duplicated_workspace_ids() {
        let (_temporary, store) = store();
        let document = store.document::<WorkspaceRegistryDocument>(keys::workspace_registry());
        let left = CanonicalPath::new("/tmp/bone-registry-left").unwrap();
        let right = CanonicalPath::new("/tmp/bone-registry-right").unwrap();
        let mut workspaces = BTreeMap::new();
        workspaces.insert(
            left.storage_encoding(),
            WorkspaceId::parse_str("00000000-0000-0000-0000-000000000000").unwrap(),
        );
        let first_revision = document
            .replace(
                &WorkspaceRegistryDocument { workspaces },
                Revision::default(),
            )
            .unwrap();

        let registry = WorkspaceRegistry::new(store.clone());
        assert!(matches!(
            registry.lookup_canonical(&left),
            Err(RegistryError::InvalidWorkspaceId { .. })
        ));

        let mut workspaces = BTreeMap::new();
        let id = WorkspaceId::new();
        workspaces.insert(left.storage_encoding(), id);
        workspaces.insert(right.storage_encoding(), id);
        document
            .replace(&WorkspaceRegistryDocument { workspaces }, first_revision)
            .unwrap();
        assert!(matches!(
            registry.lookup_canonical(&left),
            Err(RegistryError::DuplicateWorkspaceId { .. })
        ));
    }
}
