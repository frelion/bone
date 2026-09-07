use std::{
    collections::BTreeSet,
    fmt, fs,
    path::{Path, PathBuf},
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    CanonicalPath, JournalError, RecordError, SessionJournal, SessionLeaseError, SessionStoreError,
    StorageError, WorkspaceContext, WorkspaceId,
    storage::{
        MAX_SESSION_BYTES, StoreLock, ensure_private_directory, lock_path, read_json, write_json,
    },
};

const MAX_TITLE_BYTES: usize = 512;
const MAX_DRAFT_BYTES: usize = 1024 * 1024;
const MAX_MODEL_OVERRIDE_BYTES: usize = 512;
const MAX_CONFIG_REVISION_BYTES: usize = 256;

/// Globally unique persistent ID for a logical user session.
#[derive(Clone, Copy, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn is_nil(self) -> bool {
        self.0.is_nil()
    }

    pub fn as_uuid(self) -> Uuid {
        self.0
    }

    pub fn parse_str(value: &str) -> Result<Self, uuid::Error> {
        Uuid::parse_str(value).map(Self)
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl fmt::Debug for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("SessionId").field(&self.0).finish()
    }
}

impl FromStr for SessionId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse_str(value)
    }
}

/// Per-record optimistic-concurrency revision.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionRevision(u64);

impl SessionRevision {
    pub fn initial() -> Self {
        Self(0)
    }

    pub fn value(self) -> u64 {
        self.0
    }

    fn next(self) -> Result<Self, RecordError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(RecordError::RevisionExhausted)
    }
}

impl fmt::Display for SessionRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// UTC milliseconds since Unix epoch, represented without a UI-specific time
/// dependency. Consumers may render it in their own locale.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UnixMillis(i64);

impl UnixMillis {
    pub fn now() -> Result<Self, RecordError> {
        let duration = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| RecordError::ClockBeforeUnixEpoch)?;
        let milliseconds =
            i64::try_from(duration.as_millis()).map_err(|_| RecordError::ClockOutOfRange)?;
        Ok(Self(milliseconds))
    }

    pub fn from_millis(milliseconds: i64) -> Result<Self, RecordError> {
        if milliseconds < 0 {
            return Err(RecordError::InvalidTimestamp);
        }
        Ok(Self(milliseconds))
    }

    pub fn as_millis(self) -> i64 {
        self.0
    }
}

/// Draft text and the byte position of its composer cursor.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionDraft {
    pub text: String,
    pub cursor_byte_offset: usize,
}

impl SessionDraft {
    pub fn new(text: impl Into<String>, cursor_byte_offset: usize) -> Result<Self, RecordError> {
        let draft = Self {
            text: text.into(),
            cursor_byte_offset,
        };
        draft.validate()?;
        Ok(draft)
    }

    pub fn empty() -> Self {
        Self::default()
    }

    pub fn validate(&self) -> Result<(), RecordError> {
        if self.text.len() > MAX_DRAFT_BYTES {
            return Err(RecordError::ValueTooLong {
                field: "session draft",
                maximum_bytes: MAX_DRAFT_BYTES,
            });
        }
        if self.cursor_byte_offset > self.text.len()
            || !self.text.is_char_boundary(self.cursor_byte_offset)
        {
            return Err(RecordError::InvalidDraftCursor);
        }
        Ok(())
    }
}

/// Visibility/lifecycle state. It is intentionally separate from execution.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionLifecycle {
    #[default]
    Active,
    Archived,
    Deleted,
}

/// Durable description of the last known logical execution state.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionExecution {
    #[default]
    Draft,
    QueuedForSetup,
    QueuedForRuntime,
    Opening,
    Ready,
    Working,
    WaitingForUser,
    Stopping,
    Complete,
    Interrupted,
    Offline,
}

/// Runtime handles are never persisted; this field is only the last known
/// attachment state and becomes detached on cold startup.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeAttachment {
    #[default]
    Detached,
    Attaching,
    Attached,
}

/// Whether local durable state can be edited from this process.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionAvailability {
    #[default]
    Local,
    ReadOnlyElsewhere,
    Offline,
    Corrupt,
}

/// Independent rail/timeline flags; several may apply at once.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionAttention {
    Unread,
    UnresolvedEffect,
    ConfigPending,
    RecoveryNeeded,
}

/// Product status is deliberately an orthogonal state vector rather than a
/// single overloaded enum such as `Archived` or `Working`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionStatus {
    pub lifecycle: SessionLifecycle,
    pub execution: SessionExecution,
    pub attachment: RuntimeAttachment,
    pub availability: SessionAvailability,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub attention: BTreeSet<SessionAttention>,
}

/// User-facing metadata independent of the agent runtime and transcript.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionMetadata {
    pub title: String,
    pub created_at: UnixMillis,
    pub updated_at: UnixMillis,
    pub last_opened_at: UnixMillis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solver_model_override: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_config_revision: Option<String>,
}

impl SessionMetadata {
    pub fn new(title: impl Into<String>, now: UnixMillis) -> Result<Self, RecordError> {
        let metadata = Self {
            title: title.into(),
            created_at: now,
            updated_at: now,
            last_opened_at: now,
            solver_model_override: None,
            effective_config_revision: None,
        };
        metadata.validate()?;
        Ok(metadata)
    }

    fn validate(&self) -> Result<(), RecordError> {
        validate_non_empty_text("session title", &self.title, MAX_TITLE_BYTES)?;
        validate_optional_text(
            "solver model override",
            &self.solver_model_override,
            MAX_MODEL_OVERRIDE_BYTES,
        )?;
        validate_optional_text(
            "effective config revision",
            &self.effective_config_revision,
            MAX_CONFIG_REVISION_BYTES,
        )?;
        if self.created_at.0 < 0 || self.updated_at.0 < 0 || self.last_opened_at.0 < 0 {
            return Err(RecordError::InvalidTimestamp);
        }
        if self.updated_at < self.created_at || self.last_opened_at < self.created_at {
            return Err(RecordError::NonMonotonicTimestamp);
        }
        Ok(())
    }
}

/// One durable, workspace-bound logical conversation. It never contains a
/// runtime handle, provider credential, socket, or in-flight future.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRecord {
    pub id: SessionId,
    pub workspace_id: WorkspaceId,
    pub canonical_workspace_path: CanonicalPath,
    pub metadata: SessionMetadata,
    #[serde(default)]
    pub draft: SessionDraft,
    #[serde(default)]
    pub status: SessionStatus,
    #[serde(default = "SessionRevision::initial")]
    pub revision: SessionRevision,
}

impl SessionRecord {
    pub fn new(
        workspace: &WorkspaceContext,
        title: impl Into<String>,
    ) -> Result<Self, RecordError> {
        Self::new_at(workspace, title, UnixMillis::now()?)
    }

    pub fn new_at(
        workspace: &WorkspaceContext,
        title: impl Into<String>,
        now: UnixMillis,
    ) -> Result<Self, RecordError> {
        let record = Self {
            id: SessionId::new(),
            workspace_id: workspace.id(),
            canonical_workspace_path: workspace.canonical_path().clone(),
            metadata: SessionMetadata::new(title, now)?,
            draft: SessionDraft::empty(),
            status: SessionStatus::default(),
            revision: SessionRevision::initial(),
        };
        record.validate()?;
        Ok(record)
    }

    pub fn validate(&self) -> Result<(), RecordError> {
        if self.id.is_nil() {
            return Err(RecordError::NilSessionId);
        }
        if self.workspace_id.is_nil() {
            return Err(RecordError::NilWorkspaceId);
        }
        if !self.canonical_workspace_path.as_path().is_absolute() {
            return Err(RecordError::NonAbsoluteWorkspacePath);
        }
        self.metadata.validate()?;
        self.draft.validate()
    }
}

/// One non-fatal record problem found during a workspace listing. A corrupt
/// file must not prevent the user from opening every other session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionStoreIssue {
    pub session_id: Option<SessionId>,
    pub path: PathBuf,
    pub message: String,
}

/// Results of listing a workspace's logical sessions.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SessionListing {
    pub records: Vec<SessionRecord>,
    pub issues: Vec<SessionStoreIssue>,
}

/// Per-workspace durable JSON session metadata store.
///
/// Layout under an application-owned private state directory:
///
/// ```text
/// sessions/<workspace-uuid>/<session-uuid>.json
/// ```
///
/// Each record has its own short lock and atomic replacement, so two processes
/// may update different sessions in one workspace without a global session
/// lock. A separate per-session writer lease serializes runtime ownership and
/// durable turn writes; append-only message journals add their own short
/// operation-level lock.
#[derive(Clone, Debug)]
pub struct SessionStore {
    workspace: WorkspaceContext,
    directory: PathBuf,
}

/// A process-lifetime exclusive writer lease for one logical session.
///
/// The lease owns an OS file lock instead of a timestamp-based claim. It is
/// therefore released automatically when its process dies, while an orderly
/// BONE shutdown releases it by dropping this value. It must stay alive for
/// every runtime attachment and every append/update that belongs to that
/// session. It intentionally does **not** make a session record cloneable or
/// transferable to another process.
///
/// `fs2` maps this lock to the native file-lock primitive on Unix and Windows.
/// As with the rest of BONE's private-file locks, a network filesystem whose
/// locking semantics are unreliable cannot provide a stronger guarantee.
#[must_use = "dropping a SessionWriterLease immediately releases writer ownership"]
pub struct SessionWriterLease {
    session_id: SessionId,
    path: PathBuf,
    _lock: StoreLock,
}

impl SessionWriterLease {
    /// The one logical session exclusively owned by this lease.
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// The private sidecar file whose native lock represents this lease.
    /// Exposed for diagnostics only; callers must not remove or replace it.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl std::fmt::Debug for SessionWriterLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionWriterLease")
            .field("session_id", &self.session_id)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl SessionStore {
    /// Open the session directory for `workspace` below an application-owned
    /// global state root. No project directory is written.
    pub fn open_in(
        state_root: impl AsRef<Path>,
        workspace: WorkspaceContext,
    ) -> Result<Self, SessionStoreError> {
        let state_root = ensure_private_directory(state_root.as_ref())?;
        let sessions = ensure_private_directory(&state_root.join("sessions"))?;
        let directory = ensure_private_directory(&sessions.join(workspace.id().to_string()))?;
        Ok(Self {
            workspace,
            directory,
        })
    }

    pub fn workspace(&self) -> &WorkspaceContext {
        &self.workspace
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Create a new Draft session and durably persist it before returning.
    pub fn create(&self, title: impl Into<String>) -> Result<SessionRecord, SessionStoreError> {
        let title = title.into();
        for _ in 0..8 {
            let record = SessionRecord::new(&self.workspace, title.clone())?;
            match self.create_record(record) {
                Err(SessionStoreError::AlreadyExists(_)) => continue,
                result => return result,
            }
        }
        Err(SessionStoreError::IdAllocationExhausted)
    }

    /// Persist a caller-created record after checking its immutable workspace
    /// binding. Intended for controlled imports and deterministic tests.
    pub fn create_record(&self, record: SessionRecord) -> Result<SessionRecord, SessionStoreError> {
        self.validate_record_for_workspace(&record)?;
        if record.revision != SessionRevision::initial() {
            return Err(SessionStoreError::ImmutableField {
                session_id: record.id,
                field: "revision on creation",
            });
        }
        let path = self.session_path(record.id);
        let _lock = StoreLock::acquire(&lock_path(&path))?;
        if self.read_record(record.id)?.is_some() {
            return Err(SessionStoreError::AlreadyExists(record.id));
        }
        write_json(&path, &record, MAX_SESSION_BYTES)?;
        Ok(record)
    }

    pub fn get(&self, id: SessionId) -> Result<Option<SessionRecord>, SessionStoreError> {
        self.read_record(id)
    }

    /// Open the append-only durable fact journal for one existing session.
    /// Journal access is deliberately separate from the metadata record so a
    /// runtime handle can never be mistaken for a persistent conversation.
    pub fn journal(&self, id: SessionId) -> Result<SessionJournal, JournalError> {
        if self.get(id)?.is_none() {
            return Err(JournalError::SessionNotFound(id));
        }
        Ok(SessionJournal::open(id, self.journal_path(id)))
    }

    /// Attempt to become the sole runtime and durable-turn writer for one
    /// session. This operation never waits: a competing BONE process receives
    /// [`SessionLeaseError::HeldElsewhere`] and can present the conversation
    /// as read-only instead of freezing the terminal.
    ///
    /// The returned value is the lease. Keep it alive until all runtime work,
    /// journal appends, and summary writes for this session have stopped.
    /// Dropping it releases the native lock; a crashed process releases it
    /// automatically through normal OS handle cleanup.
    pub fn try_acquire_writer_lease(
        &self,
        id: SessionId,
    ) -> Result<SessionWriterLease, SessionLeaseError> {
        // Check before creating a sidecar for an arbitrary UUID. A record is
        // immutable enough for this check: BONE archives sessions but does not
        // delete their identity while another process may be looking at it.
        if self.get(id)?.is_none() {
            return Err(SessionLeaseError::NotFound(id));
        }
        let path = self.writer_lease_path(id);
        let lock = match StoreLock::acquire(&path) {
            Ok(lock) => lock,
            Err(StorageError::Busy { .. }) => {
                return Err(SessionLeaseError::HeldElsewhere { session_id: id });
            }
            Err(error) => return Err(error.into()),
        };
        Ok(SessionWriterLease {
            session_id: id,
            path,
            _lock: lock,
        })
    }

    /// Read all valid records for the current workspace. Individual damaged
    /// session files are reported in `issues` instead of aborting the listing.
    pub fn list(&self) -> Result<SessionListing, SessionStoreError> {
        let entries = fs::read_dir(&self.directory).map_err(|source| {
            StorageError::io("list session directory", &self.directory, source)
        })?;
        let mut listing = SessionListing::default();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(source) => {
                    listing.issues.push(SessionStoreIssue {
                        session_id: None,
                        path: self.directory.clone(),
                        message: format!("could not inspect a session entry: {source}"),
                    });
                    continue;
                }
            };
            let path = entry.path();
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                listing.issues.push(SessionStoreIssue {
                    session_id: None,
                    path,
                    message: "session file name is not valid UTF-8".to_owned(),
                });
                continue;
            };
            if name.ends_with(".lock")
                || name.ends_with(".lease")
                || name.starts_with(".bone-workspace-")
                || name.ends_with(".journal.jsonl")
            {
                continue;
            }
            let Some(stem) = name.strip_suffix(".json") else {
                listing.issues.push(SessionStoreIssue {
                    session_id: None,
                    path,
                    message: "unexpected file in session directory".to_owned(),
                });
                continue;
            };
            let id = match SessionId::parse_str(stem) {
                Ok(id) => id,
                Err(_) => {
                    listing.issues.push(SessionStoreIssue {
                        session_id: None,
                        path,
                        message: "session file name is not a UUID".to_owned(),
                    });
                    continue;
                }
            };
            match self.read_record(id) {
                Ok(Some(record)) => listing.records.push(record),
                Ok(None) => {}
                Err(error) => listing.issues.push(SessionStoreIssue {
                    session_id: Some(id),
                    path,
                    message: error.to_string(),
                }),
            }
        }
        listing.records.sort_by(|left, right| {
            right
                .metadata
                .updated_at
                .cmp(&left.metadata.updated_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        Ok(listing)
    }

    /// Replace mutable record fields using optimistic concurrency. IDs,
    /// workspace binding, canonical root, and creation time are immutable.
    pub fn replace(
        &self,
        mut record: SessionRecord,
        expected_revision: SessionRevision,
    ) -> Result<SessionRecord, SessionStoreError> {
        self.validate_record_for_workspace(&record)?;
        let path = self.session_path(record.id);
        let _lock = StoreLock::acquire(&lock_path(&path))?;
        let current = self
            .read_record(record.id)?
            .ok_or(SessionStoreError::NotFound(record.id))?;
        if current.revision != expected_revision || record.revision != expected_revision {
            return Err(SessionStoreError::RevisionConflict {
                session_id: record.id,
                expected: expected_revision,
                actual: current.revision,
            });
        }
        assert_immutable(&current, &record)?;
        if record.metadata.last_opened_at < current.metadata.last_opened_at {
            return Err(SessionStoreError::Record(
                RecordError::NonMonotonicTimestamp,
            ));
        }
        record.validate()?;
        record.revision = current.revision.next()?;
        let now = UnixMillis::now()?;
        record.metadata.updated_at = now.max(current.metadata.updated_at);
        write_json(&path, &record, MAX_SESSION_BYTES)?;
        Ok(record)
    }

    pub fn update_draft(
        &self,
        id: SessionId,
        expected_revision: SessionRevision,
        draft: SessionDraft,
    ) -> Result<SessionRecord, SessionStoreError> {
        draft.validate()?;
        let mut record = self.get(id)?.ok_or(SessionStoreError::NotFound(id))?;
        record.draft = draft;
        self.replace(record, expected_revision)
    }

    pub fn update_status(
        &self,
        id: SessionId,
        expected_revision: SessionRevision,
        status: SessionStatus,
    ) -> Result<SessionRecord, SessionStoreError> {
        let mut record = self.get(id)?.ok_or(SessionStoreError::NotFound(id))?;
        record.status = status;
        self.replace(record, expected_revision)
    }

    pub fn rename(
        &self,
        id: SessionId,
        expected_revision: SessionRevision,
        title: impl Into<String>,
    ) -> Result<SessionRecord, SessionStoreError> {
        let mut record = self.get(id)?.ok_or(SessionStoreError::NotFound(id))?;
        record.metadata.title = title.into();
        self.replace(record, expected_revision)
    }

    pub fn mark_opened(
        &self,
        id: SessionId,
        expected_revision: SessionRevision,
    ) -> Result<SessionRecord, SessionStoreError> {
        let mut record = self.get(id)?.ok_or(SessionStoreError::NotFound(id))?;
        record.metadata.last_opened_at = UnixMillis::now()?;
        self.replace(record, expected_revision)
    }

    fn session_path(&self, id: SessionId) -> PathBuf {
        self.directory.join(format!("{id}.json"))
    }

    fn writer_lease_path(&self, id: SessionId) -> PathBuf {
        self.directory.join(format!("{id}.writer.lease"))
    }

    fn journal_path(&self, id: SessionId) -> PathBuf {
        self.directory.join(format!("{id}.journal.jsonl"))
    }

    fn read_record(&self, id: SessionId) -> Result<Option<SessionRecord>, SessionStoreError> {
        let path = self.session_path(id);
        let record: Option<SessionRecord> = match read_json(&path, MAX_SESSION_BYTES) {
            Ok(record) => record,
            Err(error @ StorageError::InvalidDocument { .. })
            | Err(error @ StorageError::DocumentTooLarge { .. })
            | Err(error @ StorageError::UnsafeStorage { .. }) => {
                return Err(SessionStoreError::CorruptSession {
                    session_id: id,
                    path,
                    message: error.to_string(),
                });
            }
            Err(error) => return Err(error.into()),
        };
        let Some(record) = record else {
            return Ok(None);
        };
        if record.id != id {
            return Err(SessionStoreError::CorruptSession {
                session_id: id,
                path,
                message: "record ID does not match its file name".to_owned(),
            });
        }
        if let Err(error) = self.validate_record_for_workspace(&record) {
            return Err(SessionStoreError::CorruptSession {
                session_id: id,
                path,
                message: error.to_string(),
            });
        }
        Ok(Some(record))
    }

    fn validate_record_for_workspace(
        &self,
        record: &SessionRecord,
    ) -> Result<(), SessionStoreError> {
        record.validate()?;
        if record.workspace_id != self.workspace.id() {
            return Err(SessionStoreError::WorkspaceMismatch {
                session_id: record.id,
                expected_workspace: self.workspace.id(),
                actual_workspace: record.workspace_id,
            });
        }
        if record.canonical_workspace_path != *self.workspace.canonical_path() {
            return Err(SessionStoreError::ImmutableField {
                session_id: record.id,
                field: "canonical_workspace_path",
            });
        }
        Ok(())
    }
}

fn assert_immutable(
    current: &SessionRecord,
    next: &SessionRecord,
) -> Result<(), SessionStoreError> {
    if current.id != next.id {
        return Err(SessionStoreError::ImmutableField {
            session_id: current.id,
            field: "id",
        });
    }
    if current.workspace_id != next.workspace_id {
        return Err(SessionStoreError::WorkspaceMismatch {
            session_id: current.id,
            expected_workspace: current.workspace_id,
            actual_workspace: next.workspace_id,
        });
    }
    if current.canonical_workspace_path != next.canonical_workspace_path {
        return Err(SessionStoreError::ImmutableField {
            session_id: current.id,
            field: "canonical_workspace_path",
        });
    }
    if current.metadata.created_at != next.metadata.created_at {
        return Err(SessionStoreError::ImmutableField {
            session_id: current.id,
            field: "metadata.created_at",
        });
    }
    Ok(())
}

fn validate_non_empty_text(
    field: &'static str,
    value: &str,
    maximum_bytes: usize,
) -> Result<(), RecordError> {
    if value.trim().is_empty() {
        return Err(RecordError::EmptyTitle);
    }
    if value.len() > maximum_bytes {
        return Err(RecordError::ValueTooLong {
            field,
            maximum_bytes,
        });
    }
    Ok(())
}

fn validate_optional_text(
    field: &'static str,
    value: &Option<String>,
    maximum_bytes: usize,
) -> Result<(), RecordError> {
    let Some(value) = value else {
        return Ok(());
    };
    if value.trim().is_empty() {
        return Err(RecordError::EmptyTitle);
    }
    if value.len() > maximum_bytes {
        return Err(RecordError::ValueTooLong {
            field,
            maximum_bytes,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        env, fs,
        path::PathBuf,
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    use super::*;
    use crate::WorkspaceRegistry;

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

    fn workspace_context(data: &tempfile::TempDir, root: &tempfile::TempDir) -> WorkspaceContext {
        let registry = WorkspaceRegistry::open_in(data.path()).unwrap();
        WorkspaceContext::discover(root.path(), &registry).unwrap()
    }

    #[test]
    fn draft_and_orthogonal_status_survive_reopen() {
        let data = private_data();
        let root = tempfile::tempdir().unwrap();
        let workspace = workspace_context(&data, &root);
        let store = SessionStore::open_in(data.path(), workspace.clone()).unwrap();
        let created = store.create("Investigate flaky test").unwrap();

        let draft = SessionDraft::new("try --nocapture", "try --nocapture".len()).unwrap();
        let saved = store
            .update_draft(created.id, created.revision, draft)
            .unwrap();
        let status = SessionStatus {
            lifecycle: SessionLifecycle::Active,
            execution: SessionExecution::WaitingForUser,
            attachment: RuntimeAttachment::Detached,
            availability: SessionAvailability::Offline,
            attention: BTreeSet::from([SessionAttention::RecoveryNeeded]),
        };
        let saved = store
            .update_status(saved.id, saved.revision, status.clone())
            .unwrap();
        drop(store);

        let reopened = SessionStore::open_in(data.path(), workspace).unwrap();
        let restored = reopened.get(saved.id).unwrap().unwrap();
        assert_eq!(restored.draft.text, "try --nocapture");
        assert_eq!(restored.status, status);
        assert_eq!(restored.revision, SessionRevision(2));
    }

    #[test]
    fn replace_uses_revision_cas_and_keeps_workspace_binding_immutable() {
        let data = private_data();
        let root = tempfile::tempdir().unwrap();
        let workspace = workspace_context(&data, &root);
        let store = SessionStore::open_in(data.path(), workspace.clone()).unwrap();
        let created = store.create("Original").unwrap();

        let mut stale = created.clone();
        stale.metadata.title = "Changed once".to_owned();
        let saved = store.replace(stale, created.revision).unwrap();

        let mut outdated = created.clone();
        outdated.metadata.title = "Stale write".to_owned();
        assert!(matches!(
            store.replace(outdated, created.revision),
            Err(SessionStoreError::RevisionConflict { .. })
        ));

        let other_root = tempfile::tempdir().unwrap();
        let other = workspace_context(&data, &other_root);
        let saved_revision = saved.revision;
        let mut moved = saved;
        moved.workspace_id = other.id();
        assert!(matches!(
            store.replace(moved, saved_revision),
            Err(SessionStoreError::WorkspaceMismatch { .. })
        ));
    }

    #[test]
    fn listing_isolated_to_workspace_and_one_corrupt_record_is_nonfatal() {
        let data = private_data();
        let left_root = tempfile::tempdir().unwrap();
        let right_root = tempfile::tempdir().unwrap();
        let left = workspace_context(&data, &left_root);
        let right = workspace_context(&data, &right_root);
        let left_store = SessionStore::open_in(data.path(), left).unwrap();
        let right_store = SessionStore::open_in(data.path(), right).unwrap();
        let good = left_store.create("Good").unwrap();
        let broken = left_store.create("Broken").unwrap();
        right_store.create("Other workspace").unwrap();

        fs::write(left_store.session_path(broken.id), "not valid JSON").unwrap();
        let listing = left_store.list().unwrap();
        assert_eq!(listing.records.len(), 1);
        assert_eq!(listing.records[0].id, good.id);
        assert_eq!(listing.issues.len(), 1);
        assert_eq!(right_store.list().unwrap().records.len(), 1);
    }

    #[test]
    fn draft_rejects_cursor_inside_a_utf8_code_point() {
        assert!(matches!(
            SessionDraft::new("你", 1),
            Err(RecordError::InvalidDraftCursor)
        ));
    }

    #[test]
    fn state_is_global_data_and_never_a_workspace_dot_directory() {
        let data = private_data();
        let root = tempfile::tempdir().unwrap();
        let workspace = workspace_context(&data, &root);
        let store = SessionStore::open_in(data.path(), workspace).unwrap();
        store.create("No dot directory").unwrap();

        assert!(store.directory().starts_with(data.path()));
        assert!(!root.path().join(".bone").exists());
    }

    #[test]
    fn writer_lease_excludes_another_process_but_not_other_sessions() {
        let data = private_data();
        let root = tempfile::tempdir().unwrap();
        let workspace = workspace_context(&data, &root);
        let first = SessionStore::open_in(data.path(), workspace.clone()).unwrap();
        let second = SessionStore::open_in(data.path(), workspace).unwrap();
        let left = first.create("Owned here").unwrap();
        let right = first.create("Independent conversation").unwrap();

        let ready = data.path().join("writer-lease-child-ready");
        let mut child = Command::new(env::current_exe().unwrap())
            .arg("--exact")
            .arg("durable::session::tests::writer_lease_child_holder")
            .arg("--nocapture")
            .env("BONE_WRITER_LEASE_CHILD", "1")
            .env("BONE_WRITER_LEASE_STATE", data.path())
            .env("BONE_WRITER_LEASE_WORKSPACE", root.path())
            .env("BONE_WRITER_LEASE_SESSION", left.id.to_string())
            .env("BONE_WRITER_LEASE_READY", &ready)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(3);
        while !ready.exists() && Instant::now() < deadline {
            if child.try_wait().unwrap().is_some() {
                panic!("writer lease child exited before acquiring its lease");
            }
            thread::sleep(Duration::from_millis(10));
        }
        if !ready.exists() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("writer lease child did not report readiness");
        }

        assert!(matches!(
            second.try_acquire_writer_lease(left.id),
            Err(SessionLeaseError::HeldElsewhere { session_id }) if session_id == left.id
        ));

        // A lease is intentionally per logical session, not per workspace.
        let other = second.try_acquire_writer_lease(right.id).unwrap();
        assert_eq!(other.session_id(), right.id);
        drop(other);

        assert!(child.wait().unwrap().success());
        let reacquired = second.try_acquire_writer_lease(left.id).unwrap();
        assert_eq!(reacquired.session_id(), left.id);
    }

    /// This is invoked as a separate test-binary process by
    /// `writer_lease_excludes_another_process_but_not_other_sessions`. The
    /// normal parent test run intentionally treats it as a no-op.
    #[test]
    fn writer_lease_child_holder() {
        if env::var_os("BONE_WRITER_LEASE_CHILD").is_none() {
            return;
        }
        let state = PathBuf::from(env::var_os("BONE_WRITER_LEASE_STATE").unwrap());
        let workspace_root = PathBuf::from(env::var_os("BONE_WRITER_LEASE_WORKSPACE").unwrap());
        let session = SessionId::parse_str(
            &env::var("BONE_WRITER_LEASE_SESSION").expect("child received session ID"),
        )
        .expect("child received valid session ID");
        let ready = PathBuf::from(env::var_os("BONE_WRITER_LEASE_READY").unwrap());
        let registry = WorkspaceRegistry::open_in(&state).unwrap();
        let workspace = WorkspaceContext::discover(&workspace_root, &registry).unwrap();
        let store = SessionStore::open_in(&state, workspace).unwrap();
        let lease = store.try_acquire_writer_lease(session).unwrap();
        fs::write(ready, "ready").unwrap();
        thread::sleep(Duration::from_millis(150));
        drop(lease);
    }

    #[test]
    fn writer_lease_sidecars_are_not_reported_as_damaged_session_files() {
        let data = private_data();
        let root = tempfile::tempdir().unwrap();
        let workspace = workspace_context(&data, &root);
        let store = SessionStore::open_in(data.path(), workspace).unwrap();
        let record = store.create("Lease listing").unwrap();

        let _lease = store.try_acquire_writer_lease(record.id).unwrap();
        let listing = store.list().unwrap();
        assert_eq!(listing.records, vec![record]);
        assert!(listing.issues.is_empty());
    }

    #[test]
    fn writer_lease_rejects_a_missing_session_without_creating_a_sidecar() {
        let data = private_data();
        let root = tempfile::tempdir().unwrap();
        let workspace = workspace_context(&data, &root);
        let store = SessionStore::open_in(data.path(), workspace).unwrap();
        let missing = SessionId::new();

        assert!(matches!(
            store.try_acquire_writer_lease(missing),
            Err(SessionLeaseError::NotFound(id)) if id == missing
        ));
        assert!(!store.writer_lease_path(missing).exists());
    }
}
