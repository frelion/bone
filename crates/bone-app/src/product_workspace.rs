//! Product composition for one launch-directory workspace.
//!
//! A `WorkspaceApplication` owns scoped access to the single `BoneStore` that
//! was opened at application startup. It never creates a `.bone` directory in
//! the project, reads a legacy state path, or opens an independent store.

use std::path::Path;

use bone_store::{BoneStore, StoreError, StoreRoots};
use thiserror::Error;

use crate::{
    RegistryError, SessionLeaseError, SessionLifecycle, SessionRecord, SessionStore,
    SessionStoreError, SessionStoreIssue, SessionWriterLease, WorkspaceContext, WorkspaceError,
    WorkspaceRegistry,
};

const OPEN_RETRY_LIMIT: usize = 8;
const NEW_CONVERSATION_TITLE: &str = "New conversation";

/// A durable logical session selected during application startup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenDraft {
    pub record: SessionRecord,
    pub disposition: DraftDisposition,
    pub issues: Vec<SessionStoreIssue>,
}

/// A startup selection together with the process-lifetime right to mutate it.
/// Product runners must retain this lease for every attached runtime and
/// durable turn write; read-only callers can use `open_or_create_draft`.
#[derive(Debug)]
pub struct OpenWriterDraft {
    pub draft: OpenDraft,
    pub lease: SessionWriterLease,
}

/// Whether startup restored an existing logical session or persisted a fresh
/// empty draft.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DraftDisposition {
    Restored,
    Created,
}

/// Product boot services for exactly one launch-directory workspace.
#[derive(Clone, Debug)]
pub struct WorkspaceApplication {
    store: BoneStore,
    registry: WorkspaceRegistry,
    workspace: WorkspaceContext,
    sessions: SessionStore,
}

impl WorkspaceApplication {
    /// Normal product entry point. It opens the conventional XDG SQLite store
    /// exactly once for this application instance.
    pub fn open(launch_directory: impl AsRef<Path>) -> Result<Self, WorkspaceApplicationError> {
        let store = BoneStore::open_default()?;
        Self::open_with_store(launch_directory, store)
    }

    /// Composition-root constructor used by the binary, tests, portable hosts,
    /// and future desktop hosts. Settings, OAuth, workspace state, and TUI
    /// effects must all receive clones of this same `BoneStore`.
    pub fn open_with_store(
        launch_directory: impl AsRef<Path>,
        store: BoneStore,
    ) -> Result<Self, WorkspaceApplicationError> {
        let registry = WorkspaceRegistry::new(store.workspace_state());
        let workspace = WorkspaceContext::discover(launch_directory, &registry)?;
        let sessions = SessionStore::new(store.workspace_state(), workspace.clone());
        Ok(Self {
            store,
            registry,
            workspace,
            sessions,
        })
    }

    /// Explicit root injection for tests and portable embedding. This is the
    /// only root override; no BONE-specific environment variable is consulted.
    pub fn open_at(
        launch_directory: impl AsRef<Path>,
        roots: StoreRoots,
    ) -> Result<Self, WorkspaceApplicationError> {
        Self::open_with_store(launch_directory, BoneStore::open_at(roots)?)
    }

    /// The one store opened by the composition root.
    pub fn store(&self) -> &BoneStore {
        &self.store
    }

    pub fn registry(&self) -> &WorkspaceRegistry {
        &self.registry
    }

    /// Immutable workspace context for this launch. It does not climb to a
    /// Git root or use a parent directory as an implicit workspace.
    pub fn workspace(&self) -> &WorkspaceContext {
        &self.workspace
    }

    /// Per-workspace durable logical-session repository.
    pub fn sessions(&self) -> &SessionStore {
        &self.sessions
    }

    /// Read-only helper which never retains writer ownership after returning.
    pub fn open_or_create_draft(&self) -> Result<OpenDraft, WorkspaceApplicationError> {
        let OpenWriterDraft { draft, lease } = self.open_or_create_writer_draft()?;
        drop(lease);
        Ok(draft)
    }

    /// Select a writable startup conversation and retain its exclusive
    /// process-lifetime writer lease. The lease is acquired before
    /// `last_opened_at` changes, so a second process never mutates a session
    /// it failed to own. If all active sessions are held, create a fresh one.
    pub fn open_or_create_writer_draft(
        &self,
    ) -> Result<OpenWriterDraft, WorkspaceApplicationError> {
        for _ in 0..OPEN_RETRY_LIMIT {
            let listing = self.sessions.list()?;
            let mut candidates = listing
                .records
                .iter()
                .filter(|record| record.status.lifecycle == SessionLifecycle::Active)
                .cloned()
                .collect::<Vec<_>>();
            candidates.sort_by(|left, right| {
                left.metadata
                    .last_opened_at
                    .cmp(&right.metadata.last_opened_at)
                    .then_with(|| left.metadata.updated_at.cmp(&right.metadata.updated_at))
                    .then_with(|| left.id.cmp(&right.id))
            });
            candidates.reverse();

            let mut relist = false;
            for candidate in candidates {
                let lease = match self.sessions.try_acquire_writer_lease(candidate.id) {
                    Ok(lease) => lease,
                    Err(SessionLeaseError::HeldElsewhere { .. }) => continue,
                    Err(SessionLeaseError::NotFound(_)) => {
                        relist = true;
                        break;
                    }
                    Err(error) => return Err(error.into()),
                };
                match self
                    .sessions
                    .mark_opened(&lease, candidate.id, candidate.revision)
                {
                    Ok(record) => {
                        return Ok(OpenWriterDraft {
                            draft: OpenDraft {
                                record,
                                disposition: DraftDisposition::Restored,
                                issues: listing.issues,
                            },
                            lease,
                        });
                    }
                    Err(
                        SessionStoreError::NotFound(_) | SessionStoreError::RevisionConflict { .. },
                    ) => {
                        drop(lease);
                        relist = true;
                        break;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            if relist {
                continue;
            }

            let (record, lease) = self.sessions.create_writer(NEW_CONVERSATION_TITLE)?;
            return Ok(OpenWriterDraft {
                draft: OpenDraft {
                    record,
                    disposition: DraftDisposition::Created,
                    issues: listing.issues,
                },
                lease,
            });
        }
        Err(WorkspaceApplicationError::ConcurrentDraftOpen)
    }
}

/// Failures while constructing or using a product workspace application.
#[derive(Debug, Error)]
pub enum WorkspaceApplicationError {
    #[error("logical sessions changed repeatedly while opening a draft; please retry")]
    ConcurrentDraftOpen,
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    #[error(transparent)]
    Sessions(#[from] SessionStoreError),
    #[error(transparent)]
    Lease(#[from] SessionLeaseError),
}

#[cfg(test)]
mod tests {
    use bone_store::StoreRoots;

    use super::*;

    fn roots(directory: &tempfile::TempDir) -> StoreRoots {
        #[cfg(unix)]
        std::fs::set_permissions(
            directory.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        StoreRoots::new(
            directory.path().join("data"),
            directory.path().join("config"),
        )
        .unwrap()
    }

    #[test]
    fn injected_store_boots_a_durable_draft_without_model_or_oauth() {
        let data = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let app = WorkspaceApplication::open_at(project.path(), roots(&data)).unwrap();
        let opened = app.open_or_create_draft().unwrap();
        assert_eq!(opened.disposition, DraftDisposition::Created);
        assert_eq!(opened.record.metadata.title, NEW_CONVERSATION_TITLE);
        assert!(app.store().database_path().exists());
        assert!(!project.path().join(".bone").exists());
    }
}
