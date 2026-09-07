//! Product startup policy for a directory-scoped BONE workspace.
//!
//! This module intentionally stops before configuration, credentials, agent
//! runtimes, and terminal concerns. Its job is to make the durable product
//! identity available first, so a user can enter the application and work on
//! a draft even when no provider has been configured yet.

use std::{
    env,
    path::{Path, PathBuf},
};

use crate::{
    RegistryError, SessionLeaseError, SessionLifecycle, SessionRecord, SessionStore,
    SessionStoreError, SessionStoreIssue, SessionWriterLease, WorkspaceContext, WorkspaceError,
    WorkspaceRegistry,
};
use thiserror::Error;

const NEW_CONVERSATION_TITLE: &str = "New conversation";
const OPEN_RETRY_LIMIT: usize = 4;

/// A resolved, application-owned user-data root and the rule that selected it.
///
/// Resolving this value is side-effect free. The directory is only created by
/// [`WorkspaceApplication::open`] or [`WorkspaceApplication::open_in`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateRoot {
    path: PathBuf,
    source: StateRootSource,
}

impl StateRoot {
    fn new(path: PathBuf, source: StateRootSource) -> Result<Self, StateRootError> {
        if path.is_absolute() {
            Ok(Self { path, source })
        } else {
            Err(StateRootError::RelativePath {
                variable: source.variable_name(),
                path,
            })
        }
    }

    /// The absolute directory under which BONE may keep private user data.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The precedence rule which resolved this root.
    pub fn source(&self) -> StateRootSource {
        self.source
    }

    /// Consume this resolved value and return its absolute path.
    pub fn into_path_buf(self) -> PathBuf {
        self.path
    }
}

/// Origin of a BONE user-data root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StateRootSource {
    /// An explicit product override, suitable for tests, portable installs,
    /// and users who deliberately manage their BONE state location.
    BoneStateDir,
    /// The XDG data root on Unix.
    XdgDataHome,
    /// The conventional `~/.local/share` fallback on Unix.
    Home,
}

impl StateRootSource {
    fn variable_name(self) -> &'static str {
        match self {
            Self::BoneStateDir => "BONE_STATE_DIR",
            Self::XdgDataHome => "XDG_DATA_HOME",
            Self::Home => "HOME",
        }
    }
}

/// Process-environment inputs used to resolve the BONE state root.
///
/// Keeping the inputs as data makes precedence deterministic and testable
/// without mutating global process environment variables. Production callers
/// normally use [`StateRootEnvironment::from_process`] through
/// [`resolve_state_root`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StateRootEnvironment {
    pub bone_state_dir: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
    pub home: Option<PathBuf>,
}

impl StateRootEnvironment {
    /// Capture only the environment variables relevant to BONE user data.
    pub fn from_process() -> Self {
        Self {
            bone_state_dir: env::var_os("BONE_STATE_DIR").map(PathBuf::from),
            xdg_data_home: env::var_os("XDG_DATA_HOME").map(PathBuf::from),
            home: env::var_os("HOME").map(PathBuf::from),
        }
    }

    /// Resolve the state root without creating it.
    ///
    /// On Unix, precedence is exactly `BONE_STATE_DIR`, then
    /// `XDG_DATA_HOME/bone`, then `HOME/.local/share/bone`. A supplied
    /// relative path is rejected rather than silently making persistence
    /// depend on the launch directory.
    pub fn resolve(&self) -> Result<StateRoot, StateRootError> {
        if let Some(path) = &self.bone_state_dir {
            return StateRoot::new(path.clone(), StateRootSource::BoneStateDir);
        }

        #[cfg(unix)]
        {
            if let Some(path) = &self.xdg_data_home {
                return StateRoot::new(path.join("bone"), StateRootSource::XdgDataHome);
            }
            if let Some(path) = &self.home {
                return StateRoot::new(
                    path.join(".local").join("share").join("bone"),
                    StateRootSource::Home,
                );
            }
        }

        #[cfg(not(unix))]
        {
            let _ = self;
        }

        Err(StateRootError::Unavailable)
    }
}

/// Resolve the normal BONE user-data root from the current process
/// environment, without creating the path.
pub fn resolve_state_root() -> Result<StateRoot, StateRootError> {
    StateRootEnvironment::from_process().resolve()
}

/// Failures while locating BONE's application-owned user-data root.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum StateRootError {
    #[error("{variable} must be an absolute path, but was {path}")]
    RelativePath {
        variable: &'static str,
        path: PathBuf,
    },
    #[error(
        "could not determine BONE user-data directory; set BONE_STATE_DIR, XDG_DATA_HOME, or HOME"
    )]
    Unavailable,
}

/// A durable logical session selected during application startup.
///
/// The record is intentionally available before any model, credential, or
/// Agent runtime exists. `issues` lets the UI surface non-fatal corrupt
/// session warnings without withholding all healthy sessions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenDraft {
    pub record: SessionRecord,
    pub disposition: DraftDisposition,
    pub issues: Vec<SessionStoreIssue>,
}

/// A startup selection together with the process-lifetime right to mutate it.
///
/// Product runners must retain `lease` for as long as they attach a runtime,
/// append turns, or update the selected session's durable summary. Read-only
/// callers can use [`WorkspaceApplication::open_or_create_draft`] instead.
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
///
/// A `WorkspaceApplication` is deliberately independent from both the TUI
/// and the Agent runtime. It owns no credentials and makes no network calls.
/// Opening it persists only BONE's private workspace registry and session
/// metadata below `state_root`; it never creates a `.bone` directory in the
/// user's project.
#[derive(Clone, Debug)]
pub struct WorkspaceApplication {
    state_root: PathBuf,
    registry: WorkspaceRegistry,
    workspace: WorkspaceContext,
    sessions: SessionStore,
}

impl WorkspaceApplication {
    /// Open an application shell using the process-default BONE user-data
    /// root. This is the normal product entry point.
    pub fn open(launch_directory: impl AsRef<Path>) -> Result<Self, WorkspaceApplicationError> {
        let state_root = resolve_state_root()?;
        Self::open_in(launch_directory, state_root.path())
    }

    /// Open an application shell with an explicit absolute state root.
    ///
    /// This injection point lets the TUI, tests, portable distributions, and
    /// future daemon mode share identical startup semantics without consulting
    /// process configuration. It creates the private state directories needed
    /// by the registry and session store, but never touches the workspace
    /// beyond canonicalizing the exact launch directory.
    pub fn open_in(
        launch_directory: impl AsRef<Path>,
        state_root: impl AsRef<Path>,
    ) -> Result<Self, WorkspaceApplicationError> {
        let state_root = state_root.as_ref().to_path_buf();
        if !state_root.is_absolute() {
            return Err(WorkspaceApplicationError::RelativeStateRoot { path: state_root });
        }

        let registry = WorkspaceRegistry::open_in(&state_root)?;
        let workspace = WorkspaceContext::discover(launch_directory, &registry)?;
        let sessions = SessionStore::open_in(&state_root, workspace.clone())?;
        Ok(Self {
            state_root,
            registry,
            workspace,
            sessions,
        })
    }

    /// The global, private BONE state root used by this application shell.
    pub fn state_root(&self) -> &Path {
        &self.state_root
    }

    /// The registry that gives the exact launch directory its stable identity.
    pub fn registry(&self) -> &WorkspaceRegistry {
        &self.registry
    }

    /// Immutable workspace context for this launch. It does not climb to a
    /// Git root or use a parent directory as an implicit workspace.
    pub fn workspace(&self) -> &WorkspaceContext {
        &self.workspace
    }

    /// Per-workspace durable logical-session metadata storage.
    pub fn sessions(&self) -> &SessionStore {
        &self.sessions
    }

    /// Restore and mark open the most recently opened active session, or
    /// atomically create one empty `New conversation` draft when this
    /// workspace has none.
    ///
    /// This deliberately does not attach an Agent runtime or attempt a model
    /// login. Therefore it remains safe and useful in first-run, offline, or
    /// misconfigured states. Session-store corruption is non-fatal: healthy
    /// active records still win and issues are returned for the UI to show.
    ///
    /// This compatibility/read-only helper does not retain writer ownership
    /// after it returns. A product runtime must call
    /// [`WorkspaceApplication::open_or_create_writer_draft`] so it never
    /// marks, recovers, or runs a conversation owned by another BONE process.
    pub fn open_or_create_draft(&self) -> Result<OpenDraft, WorkspaceApplicationError> {
        let OpenWriterDraft { draft, lease } = self.open_or_create_writer_draft()?;
        drop(lease);
        Ok(draft)
    }

    /// Select a writable startup conversation and retain its exclusive
    /// process-lifetime writer lease.
    ///
    /// The lease is acquired *before* `last_opened_at` is updated. A second
    /// BONE process therefore never mutates the record of a conversation it
    /// failed to acquire. If the most recently opened active conversation is
    /// held elsewhere, this method tries the next active conversation; when
    /// every active one is held, it creates a fresh draft for this process.
    /// Existing held conversations remain readable through `sessions()` and
    /// can be shown by the TUI as read-only.
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
                    Err(SessionLeaseError::HeldElsewhere { .. }) => {
                        // This is a healthy read-only conversation owned by
                        // another process, not a startup failure.
                        continue;
                    }
                    Err(SessionLeaseError::NotFound(_)) => {
                        relist = true;
                        break;
                    }
                    Err(error) => return Err(error.into()),
                };
                match self.sessions.mark_opened(candidate.id, candidate.revision) {
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
                        // Dropping this local lease before re-listing lets a
                        // process which just won the metadata race continue.
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

            // No active session was writable (or none existed). A freshly
            // allocated UUID has no prior writer; nevertheless acquire its
            // lease before returning so a racing process cannot take over the
            // new record between creation and product bootstrap.
            let record = self.sessions.create(NEW_CONVERSATION_TITLE)?;
            match self.sessions.try_acquire_writer_lease(record.id) {
                Ok(lease) => {
                    return Ok(OpenWriterDraft {
                        draft: OpenDraft {
                            record,
                            disposition: DraftDisposition::Created,
                            issues: listing.issues,
                        },
                        lease,
                    });
                }
                Err(SessionLeaseError::HeldElsewhere { .. } | SessionLeaseError::NotFound(_)) => {
                    // A different process can only reach this path after
                    // discovering the newly created record. Re-list and pick
                    // a safe candidate rather than returning an unowned one.
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err(WorkspaceApplicationError::ConcurrentDraftOpen)
    }
}

/// Failures while constructing or using a product workspace application.
#[derive(Debug, Error)]
pub enum WorkspaceApplicationError {
    #[error("BONE state root must be absolute: {path}")]
    RelativeStateRoot { path: PathBuf },
    #[error("logical sessions changed repeatedly while opening a draft; please retry")]
    ConcurrentDraftOpen,
    #[error(transparent)]
    StateRoot(#[from] StateRootError),
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
    use std::fs;

    use super::*;
    use crate::{SessionExecution, SessionStatus};

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
    fn injected_root_boots_a_durable_draft_without_agent_or_config() {
        let data = private_data();
        let project = tempfile::tempdir().unwrap();

        let first = WorkspaceApplication::open_in(project.path(), data.path()).unwrap();
        let first_workspace = first.workspace().id();
        let initial = first.open_or_create_draft().unwrap();
        assert_eq!(initial.disposition, DraftDisposition::Created);
        assert_eq!(initial.record.metadata.title, NEW_CONVERSATION_TITLE);
        assert_eq!(initial.record.status.lifecycle, SessionLifecycle::Active);
        assert_eq!(initial.record.status.execution, SessionExecution::Draft);
        assert!(initial.issues.is_empty());
        assert!(data.path().join("workspaces.json").exists());
        assert!(first.sessions().directory().exists());
        assert!(!project.path().join(".bone").exists());

        drop(first);
        let reopened = WorkspaceApplication::open_in(project.path(), data.path()).unwrap();
        let restored = reopened.open_or_create_draft().unwrap();
        assert_eq!(reopened.workspace().id(), first_workspace);
        assert_eq!(restored.disposition, DraftDisposition::Restored);
        assert_eq!(restored.record.id, initial.record.id);
        assert!(restored.record.revision.value() > initial.record.revision.value());
        assert_eq!(reopened.sessions().list().unwrap().records.len(), 1);
    }

    #[test]
    fn skips_archived_sessions_and_creates_when_no_active_session_remains() {
        let data = private_data();
        let project = tempfile::tempdir().unwrap();
        let app = WorkspaceApplication::open_in(project.path(), data.path()).unwrap();
        let initial = app.open_or_create_draft().unwrap().record;

        let archived = SessionStatus {
            lifecycle: SessionLifecycle::Archived,
            ..initial.status.clone()
        };
        app.sessions()
            .update_status(initial.id, initial.revision, archived)
            .unwrap();

        let next = app.open_or_create_draft().unwrap();
        assert_eq!(next.disposition, DraftDisposition::Created);
        assert_ne!(next.record.id, initial.id);
        assert_eq!(next.record.metadata.title, NEW_CONVERSATION_TITLE);
    }

    #[test]
    fn restores_active_session_even_when_other_session_is_archived() {
        let data = private_data();
        let project = tempfile::tempdir().unwrap();
        let app = WorkspaceApplication::open_in(project.path(), data.path()).unwrap();
        let active = app.open_or_create_draft().unwrap().record;
        let archived = app.sessions().create("Old session").unwrap();
        app.sessions()
            .update_status(
                archived.id,
                archived.revision,
                SessionStatus {
                    lifecycle: SessionLifecycle::Archived,
                    ..archived.status.clone()
                },
            )
            .unwrap();

        let selected = app.open_or_create_draft().unwrap();
        assert_eq!(selected.disposition, DraftDisposition::Restored);
        assert_eq!(selected.record.id, active.id);
    }

    #[test]
    fn writer_startup_never_marks_a_conversation_owned_by_another_process() {
        let data = private_data();
        let project = tempfile::tempdir().unwrap();
        let first = WorkspaceApplication::open_in(project.path(), data.path()).unwrap();
        let owned = first.open_or_create_writer_draft().unwrap();
        let first_id = owned.draft.record.id;
        let first_revision = owned.draft.record.revision;

        // Keeping `owned.lease` alive simulates the first product runner. A
        // second runner must get a fresh writable draft rather than touching
        // `first_id`'s last-opened timestamp before it has the writer lease.
        let second = WorkspaceApplication::open_in(project.path(), data.path()).unwrap();
        let alternate = second.open_or_create_writer_draft().unwrap();
        assert_ne!(alternate.draft.record.id, first_id);
        assert_eq!(alternate.draft.disposition, DraftDisposition::Created);

        let unchanged = first.sessions().get(first_id).unwrap().unwrap();
        assert_eq!(unchanged.revision, first_revision);
        assert_eq!(
            unchanged.metadata.last_opened_at,
            owned.draft.record.metadata.last_opened_at
        );
        assert_eq!(first.sessions().list().unwrap().records.len(), 2);
    }

    #[test]
    fn state_root_precedence_is_injected_and_side_effect_free() {
        let temp = tempfile::tempdir().unwrap();
        let bone = temp.path().join("explicit");
        let xdg = temp.path().join("xdg");
        let home = temp.path().join("home");
        let root = StateRootEnvironment {
            bone_state_dir: Some(bone.clone()),
            xdg_data_home: Some(xdg),
            home: Some(home),
        }
        .resolve()
        .unwrap();

        assert_eq!(root.path(), bone);
        assert_eq!(root.source(), StateRootSource::BoneStateDir);
        assert!(!root.path().exists());
    }

    #[cfg(unix)]
    #[test]
    fn state_root_uses_xdg_then_home_and_rejects_relative_paths() {
        let temp = tempfile::tempdir().unwrap();
        let xdg = temp.path().join("xdg-data");
        let home = temp.path().join("home");
        let xdg_root = StateRootEnvironment {
            bone_state_dir: None,
            xdg_data_home: Some(xdg.clone()),
            home: Some(home.clone()),
        }
        .resolve()
        .unwrap();
        assert_eq!(xdg_root.path(), xdg.join("bone"));
        assert_eq!(xdg_root.source(), StateRootSource::XdgDataHome);

        let home_root = StateRootEnvironment {
            bone_state_dir: None,
            xdg_data_home: None,
            home: Some(home.clone()),
        }
        .resolve()
        .unwrap();
        assert_eq!(home_root.path(), home.join(".local/share/bone"));
        assert_eq!(home_root.source(), StateRootSource::Home);

        assert_eq!(
            StateRootEnvironment {
                bone_state_dir: Some(PathBuf::from("relative-state")),
                xdg_data_home: Some(xdg),
                home: Some(home),
            }
            .resolve()
            .unwrap_err(),
            StateRootError::RelativePath {
                variable: "BONE_STATE_DIR",
                path: PathBuf::from("relative-state"),
            }
        );
    }

    #[test]
    fn explicit_open_rejects_relative_state_root() {
        let project = tempfile::tempdir().unwrap();
        assert!(matches!(
            WorkspaceApplication::open_in(project.path(), "state"),
            Err(WorkspaceApplicationError::RelativeStateRoot { .. })
        ));
    }
}
