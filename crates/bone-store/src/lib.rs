//! BONE's concrete local persistence port.
//!
//! All BONE-owned durable data is stored in one local SQLite database. The
//! public API deliberately exposes typed documents, journals, scoped state,
//! and OS leases instead of raw SQL, raw paths, or a generic key/value API.
//! Provider OAuth JSON remains an intentionally isolated exception because
//! Rig owns its schema and refresh lifecycle.

mod document;
mod error;
mod journal;
mod lease;
mod provider_auth;
mod roots;
mod security;
mod sqlite;

use std::{fmt::Display, path::Path, sync::Arc};

use serde::{Serialize, de::DeserializeOwned};

pub use document::{Document, DocumentSnapshot, Revision};
pub use error::{ProviderAuthError, StoreError};
pub use journal::{Journal, JournalEntry, JournalRead};
pub use lease::Lease;
pub use provider_auth::{ProviderAuthLease, ProviderAuthStore, ProviderId};
pub use roots::StoreRoots;

use crate::{
    document::{
        document, read_document, read_documents_with_prefix, remove_document, replace_document,
        same_store as same_document_store,
    },
    journal::{append_journal, journal, same_store as same_journal_store},
    security::{ensure_private_directory, try_acquire_private_lock},
    sqlite::StoreInner,
};

const SETTINGS_NAMESPACE: &str = "settings";
const STATE_NAMESPACE: &str = "state";
const GLOBAL_SETTINGS_KEY: &str = "global";
const WORKSPACE_REGISTRY_KEY: &str = "workspace-registry";

/// Thread-safe handle to one locally opened BONE store.
///
/// Clones share immutable store roots but open short-lived SQLite connections
/// per operation. This avoids a hidden process-global connection and lets
/// SQLite enforce cross-process writer serialization.
#[derive(Clone)]
pub struct BoneStore {
    inner: Arc<StoreInner>,
}

impl BoneStore {
    pub fn open_default() -> Result<Self, StoreError> {
        Self::open_at(StoreRoots::default_for_current_user()?)
    }

    pub fn open_at(roots: StoreRoots) -> Result<Self, StoreError> {
        Ok(Self {
            inner: StoreInner::open(roots)?,
        })
    }

    pub fn roots(&self) -> &StoreRoots {
        self.inner.roots()
    }

    pub fn database_path(&self) -> &Path {
        self.inner.database_path()
    }

    pub fn settings(&self) -> SettingsStore {
        SettingsStore {
            inner: Arc::clone(&self.inner),
        }
    }

    pub fn workspace_state(&self) -> WorkspaceStateStore {
        WorkspaceStateStore {
            inner: Arc::clone(&self.inner),
        }
    }

    pub fn provider_auth(&self) -> ProviderAuthStore {
        ProviderAuthStore::new(Arc::clone(&self.inner))
    }
}

impl std::fmt::Debug for BoneStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BoneStore")
            .field("database_path", &self.database_path())
            .finish_non_exhaustive()
    }
}

/// Capability for the one typed global settings document.
#[derive(Clone, Debug)]
pub struct SettingsStore {
    inner: Arc<StoreInner>,
}

impl SettingsStore {
    pub fn global<T>(&self) -> Document<T> {
        document(
            Arc::clone(&self.inner),
            SETTINGS_NAMESPACE,
            GLOBAL_SETTINGS_KEY.to_owned(),
        )
    }
}

/// Capability for Workspace, Session, and durable conversation state.
#[derive(Clone, Debug)]
pub struct WorkspaceStateStore {
    inner: Arc<StoreInner>,
}

impl WorkspaceStateStore {
    /// The fixed mapping between canonical workspace paths and durable IDs.
    pub fn workspace_registry<T>(&self) -> Document<T> {
        document(
            Arc::clone(&self.inner),
            STATE_NAMESPACE,
            WORKSPACE_REGISTRY_KEY.to_owned(),
        )
    }

    /// Settings that belong to exactly one durable Workspace.
    pub fn workspace_settings<T>(
        &self,
        workspace_id: impl Display,
    ) -> Result<Document<T>, StoreError> {
        let workspace_id = opaque_id(workspace_id)?;
        Ok(document(
            Arc::clone(&self.inner),
            STATE_NAMESPACE,
            format!("workspace/{workspace_id}/settings"),
        ))
    }

    /// The durable metadata/document record for one logical Session.
    pub fn session<T>(
        &self,
        workspace_id: impl Display,
        session_id: impl Display,
    ) -> Result<Document<T>, StoreError> {
        let workspace_id = opaque_id(workspace_id)?;
        let session_id = opaque_id(session_id)?;
        Ok(document(
            Arc::clone(&self.inner),
            STATE_NAMESPACE,
            format!("workspace/{workspace_id}/session/{session_id}"),
        ))
    }

    /// List all Session records belonging to one Workspace. The Session ID is
    /// intentionally part of each domain record, so no duplicate sidebar
    /// index is needed in storage.
    pub fn list_sessions<T>(
        &self,
        workspace_id: impl Display,
    ) -> Result<Vec<DocumentSnapshot<T>>, StoreError>
    where
        T: DeserializeOwned,
    {
        let workspace_id = opaque_id(workspace_id)?;
        let connection = self.inner.connection()?;
        read_documents_with_prefix(
            &connection,
            STATE_NAMESPACE,
            &format!("workspace/{workspace_id}/session/"),
        )
    }

    /// The strictly ordered, append-only conversation event stream for one
    /// Session.
    pub fn session_journal<E>(
        &self,
        workspace_id: impl Display,
        session_id: impl Display,
    ) -> Result<Journal<E>, StoreError> {
        let workspace_id = opaque_id(workspace_id)?;
        let session_id = opaque_id(session_id)?;
        Ok(journal(
            Arc::clone(&self.inner),
            format!("workspace/{workspace_id}/session/{session_id}/events"),
        ))
    }

    /// Acquire process-lifetime runtime ownership for one Session. This lock
    /// is intentionally separate from short SQLite write transactions.
    pub fn try_acquire_session_writer_lease(
        &self,
        session_id: impl Display,
    ) -> Result<Lease, StoreError> {
        let session_id = opaque_id(session_id)?;
        let directory = ensure_private_directory(&self.inner.roots().data_root().join("leases"))?;
        let path = directory.join(format!("session-{session_id}.lock"));
        let file = try_acquire_private_lock(&path)?;
        Ok(Lease::new(path, file))
    }

    /// Execute a small domain mutation in one `BEGIN IMMEDIATE` transaction.
    ///
    /// It exposes only typed document and journal operations. A callback error
    /// drops the SQLite transaction and rolls back every mutation.
    pub fn transaction<R>(
        &self,
        operation: impl FnOnce(&mut WorkspaceWriteTransaction<'_, '_>) -> Result<R, StoreError>,
    ) -> Result<R, StoreError> {
        let inner = Arc::clone(&self.inner);
        self.inner.with_write(|transaction| {
            let mut write = WorkspaceWriteTransaction { inner, transaction };
            operation(&mut write)
        })
    }
}

/// Restricted typed operations available only inside a Workspace-state write
/// transaction. It cannot execute arbitrary SQL or construct arbitrary keys,
/// and it accepts only `state` documents from this same store—not global
/// settings handles.
pub struct WorkspaceWriteTransaction<'transaction, 'connection> {
    inner: Arc<StoreInner>,
    transaction: &'transaction rusqlite::Transaction<'connection>,
}

impl WorkspaceWriteTransaction<'_, '_> {
    pub fn read<T>(&mut self, document: &Document<T>) -> Result<DocumentSnapshot<T>, StoreError>
    where
        T: DeserializeOwned,
    {
        self.assert_document_store(document)?;
        read_document(self.transaction, &document.address)
    }

    pub fn replace<T>(
        &mut self,
        document: &Document<T>,
        value: &T,
        expected: Revision,
    ) -> Result<Revision, StoreError>
    where
        T: Serialize,
    {
        self.assert_document_store(document)?;
        replace_document(self.transaction, &document.address, value, expected)
    }

    pub fn remove<T>(
        &mut self,
        document: &Document<T>,
        expected: Revision,
    ) -> Result<(), StoreError> {
        self.assert_document_store(document)?;
        remove_document(self.transaction, &document.address, expected)
    }

    pub fn append<E>(
        &mut self,
        journal: &Journal<E>,
        event: &E,
    ) -> Result<JournalEntry<E>, StoreError>
    where
        E: Serialize + DeserializeOwned,
    {
        self.assert_journal_store(journal)?;
        append_journal(self.transaction, &journal.key, event)
    }

    fn assert_document_store<T>(&self, document: &Document<T>) -> Result<(), StoreError> {
        // A transaction obtained from `WorkspaceStateStore` is deliberately
        // incapable of mutating the global settings document. Documents are
        // constructed only by scoped store capabilities, but checking both
        // their owning store and fixed namespace preserves that capability
        // boundary even when callers hold handles from several scopes.
        if same_document_store(document, &self.inner)
            && document.address.namespace == STATE_NAMESPACE
        {
            Ok(())
        } else {
            Err(StoreError::InvalidIdentifier)
        }
    }

    fn assert_journal_store<E>(&self, journal: &Journal<E>) -> Result<(), StoreError> {
        if same_journal_store(journal, &self.inner) {
            Ok(())
        } else {
            Err(StoreError::InvalidIdentifier)
        }
    }
}

fn opaque_id(value: impl Display) -> Result<String, StoreError> {
    let value = value.to_string();
    let valid = !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if valid {
        Ok(value)
    } else {
        Err(StoreError::InvalidIdentifier)
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc, thread};

    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
    struct Settings {
        name: String,
    }

    fn store() -> (tempfile::TempDir, BoneStore) {
        let temporary = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        fs::set_permissions(
            temporary.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let store = BoneStore::open_at(
            StoreRoots::new(
                temporary.path().join("data"),
                temporary.path().join("config"),
            )
            .unwrap(),
        )
        .unwrap();
        (temporary, store)
    }

    #[test]
    fn document_create_replace_and_remove_use_revision_cas() {
        let (_temporary, store) = store();
        let document = store.settings().global::<Settings>();
        let missing = document.read().unwrap();
        assert!(missing.is_missing());
        assert_eq!(missing.revision, Revision::default());
        let first = document
            .replace(
                &Settings {
                    name: "first".to_owned(),
                },
                missing.revision,
            )
            .unwrap();
        assert_eq!(first.value(), 1);
        assert!(matches!(
            document.replace(
                &Settings {
                    name: "stale".to_owned(),
                },
                Revision::default()
            ),
            Err(StoreError::RevisionConflict { .. })
        ));
        assert_eq!(
            document
                .replace(
                    &Settings {
                        name: "first".to_owned(),
                    },
                    first
                )
                .unwrap(),
            first
        );
        document.remove(first).unwrap();
        assert!(document.read().unwrap().is_missing());
    }

    #[test]
    fn transaction_rolls_back_document_and_journal_together() {
        let (_temporary, store) = store();
        let state = store.workspace_state();
        let session = state.session::<Settings>("workspace", "session").unwrap();
        let journal = state
            .session_journal::<String>("workspace", "session")
            .unwrap();
        let failed = state.transaction(|transaction| {
            transaction.replace(
                &session,
                &Settings {
                    name: "uncommitted".to_owned(),
                },
                Revision::default(),
            )?;
            transaction.append(&journal, &"uncommitted".to_owned())?;
            Err::<(), _>(StoreError::Corrupt {
                message: "test rollback",
            })
        });
        assert!(failed.is_err());
        assert!(session.read().unwrap().is_missing());
        assert!(journal.read().unwrap().is_empty());
    }

    #[test]
    fn workspace_transaction_cannot_mutate_global_settings() {
        let (_temporary, store) = store();
        let settings = store.settings().global::<Settings>();
        let state = store.workspace_state();

        let result = state.transaction(|transaction| {
            transaction.replace(
                &settings,
                &Settings {
                    name: "must remain outside workspace state".to_owned(),
                },
                Revision::default(),
            )
        });

        assert!(matches!(result, Err(StoreError::InvalidIdentifier)));
        assert!(settings.read().unwrap().is_missing());
    }

    #[test]
    fn session_writer_lease_is_exclusive() {
        let (_temporary, store) = store();
        let state = store.workspace_state();
        let lease = state.try_acquire_session_writer_lease("session").unwrap();
        assert!(matches!(
            state.try_acquire_session_writer_lease("session"),
            Err(StoreError::Busy)
        ));
        drop(lease);
        let _reacquired = state.try_acquire_session_writer_lease("session").unwrap();
    }

    #[test]
    fn document_writes_have_one_winner_under_contention() {
        let (_temporary, store) = store();
        let document = Arc::new(store.settings().global::<Settings>());
        let joins = (0..8)
            .map(|number| {
                let document = Arc::clone(&document);
                thread::spawn(move || {
                    let snapshot = document.read().unwrap();
                    document.replace(
                        &Settings {
                            name: number.to_string(),
                        },
                        snapshot.revision,
                    )
                })
            })
            .collect::<Vec<_>>();
        let successful = joins
            .into_iter()
            .filter_map(|join| join.join().unwrap().ok())
            .count();
        assert!(successful >= 1);
    }

    #[test]
    fn session_listing_treats_workspace_ids_as_literal_prefixes() {
        let (_temporary, store) = store();
        let state = store.workspace_state();
        let underscored = state.session::<Settings>("a_", "one").unwrap();
        let similar = state.session::<Settings>("ab", "two").unwrap();

        underscored
            .replace(
                &Settings {
                    name: "underscored workspace".to_owned(),
                },
                Revision::default(),
            )
            .unwrap();
        similar
            .replace(
                &Settings {
                    name: "different workspace".to_owned(),
                },
                Revision::default(),
            )
            .unwrap();

        let sessions = state.list_sessions::<Settings>("a_").unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0].value.as_ref().map(|settings| &settings.name),
            Some(&"underscored workspace".to_owned())
        );
    }

    #[test]
    fn reads_remain_available_while_another_connection_writes() {
        let (_temporary, store) = store();
        let writer = rusqlite::Connection::open(store.database_path()).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();

        let result = store.settings().global::<Settings>().read();

        writer.execute_batch("ROLLBACK").unwrap();
        assert!(
            result.is_ok(),
            "a WAL reader must not contend with a writer"
        );
    }
}
