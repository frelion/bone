use std::{
    collections::BTreeSet,
    fmt,
    path::Path,
    str::FromStr,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use bone_store::{Document, Lease, Revision, StoreError, WorkspaceStateStore};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ModelSelection;

use super::{
    CanonicalPath, JournalEntry, JournalError, JournalFact, SessionJournal, SessionLeaseError,
    SessionStoreError, WorkspaceContext, WorkspaceId,
};

const MAX_TITLE_BYTES: usize = 512;
const MAX_DRAFT_BYTES: usize = 1024 * 1024;
const CREATE_RETRY_LIMIT: usize = 8;

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

/// UTC milliseconds since Unix epoch, represented without a UI-specific time
/// dependency. Consumers may render it in their own locale.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UnixMillis(i64);

impl UnixMillis {
    pub fn now() -> Result<Self, super::RecordError> {
        let duration = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| super::RecordError::ClockBeforeUnixEpoch)?;
        let milliseconds =
            i64::try_from(duration.as_millis()).map_err(|_| super::RecordError::ClockOutOfRange)?;
        Ok(Self(milliseconds))
    }

    pub fn from_millis(milliseconds: i64) -> Result<Self, super::RecordError> {
        if milliseconds < 0 {
            return Err(super::RecordError::InvalidTimestamp);
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
    pub fn new(
        text: impl Into<String>,
        cursor_byte_offset: usize,
    ) -> Result<Self, super::RecordError> {
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

    pub fn validate(&self) -> Result<(), super::RecordError> {
        if self.text.len() > MAX_DRAFT_BYTES {
            return Err(super::RecordError::ValueTooLong {
                field: "session draft",
                maximum_bytes: MAX_DRAFT_BYTES,
            });
        }
        if self.cursor_byte_offset > self.text.len()
            || !self.text.is_char_boundary(self.cursor_byte_offset)
        {
            return Err(super::RecordError::InvalidDraftCursor);
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
    /// A Session-local model selection. `None` means inherit Workspace then
    /// User defaults; this document is the only source for this override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solver_model_override: Option<ModelSelection>,
}

impl SessionMetadata {
    pub fn new(title: impl Into<String>, now: UnixMillis) -> Result<Self, super::RecordError> {
        let metadata = Self {
            title: title.into(),
            created_at: now,
            updated_at: now,
            last_opened_at: now,
            solver_model_override: None,
        };
        metadata.validate()?;
        Ok(metadata)
    }

    fn validate(&self) -> Result<(), super::RecordError> {
        validate_non_empty_text("session title", &self.title, MAX_TITLE_BYTES)?;
        if let Some(selection) = &self.solver_model_override {
            selection
                .validate()
                .map_err(|_| super::RecordError::InvalidModelOverride)?;
        }
        if self.created_at.0 < 0 || self.updated_at.0 < 0 || self.last_opened_at.0 < 0 {
            return Err(super::RecordError::InvalidTimestamp);
        }
        if self.updated_at < self.created_at || self.last_opened_at < self.created_at {
            return Err(super::RecordError::NonMonotonicTimestamp);
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
    /// SQLite document revision, injected when the record is read. It is not
    /// serialized into the JSON payload, so SQLite is the sole CAS authority.
    #[serde(skip, default)]
    pub revision: Revision,
}

impl SessionRecord {
    pub fn new(
        workspace: &WorkspaceContext,
        title: impl Into<String>,
    ) -> Result<Self, super::RecordError> {
        Self::new_at(workspace, title, UnixMillis::now()?)
    }

    pub fn new_at(
        workspace: &WorkspaceContext,
        title: impl Into<String>,
        now: UnixMillis,
    ) -> Result<Self, super::RecordError> {
        let record = Self {
            id: SessionId::new(),
            workspace_id: workspace.id(),
            canonical_workspace_path: workspace.canonical_path().clone(),
            metadata: SessionMetadata::new(title, now)?,
            draft: SessionDraft::empty(),
            status: SessionStatus::default(),
            revision: Revision::default(),
        };
        record.validate()?;
        Ok(record)
    }

    pub fn validate(&self) -> Result<(), super::RecordError> {
        if self.id.is_nil() {
            return Err(super::RecordError::NilSessionId);
        }
        if self.workspace_id.is_nil() {
            return Err(super::RecordError::NilWorkspaceId);
        }
        if !self.canonical_workspace_path.as_path().is_absolute() {
            return Err(super::RecordError::NonAbsoluteWorkspacePath);
        }
        self.metadata.validate()?;
        self.draft.validate()
    }
}

/// A non-fatal issue found when listing sessions. SQLite storage corruption is
/// reported by the store as a whole, so a healthy listing has no per-file scan
/// issues; retaining this type keeps UI recovery reporting domain-oriented.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionStoreIssue {
    pub session_id: Option<SessionId>,
    pub message: String,
}

/// Results of listing a workspace's logical sessions.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SessionListing {
    pub records: Vec<SessionRecord>,
    pub issues: Vec<SessionStoreIssue>,
}

/// A process-lifetime exclusive writer lease for one logical session.
#[must_use = "dropping a SessionWriterLease immediately releases writer ownership"]
pub struct SessionWriterLease {
    session_id: SessionId,
    workspace_id: WorkspaceId,
    issuer: Arc<()>,
    _lease: Lease,
}

impl SessionWriterLease {
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub(crate) fn assert_grants_write(
        &self,
        workspace_id: WorkspaceId,
        session_id: SessionId,
        issuer: &Arc<()>,
    ) -> Result<(), SessionStoreError> {
        if self.session_id != session_id
            || self.workspace_id != workspace_id
            || !Arc::ptr_eq(&self.issuer, issuer)
        {
            return Err(SessionStoreError::WriterLeaseMismatch {
                session_id,
                lease_session_id: self.session_id,
            });
        }
        Ok(())
    }

    /// Diagnostic-only lock path. Product code never uses this to build a
    /// storage location or change lease ownership.
    pub fn path(&self) -> &Path {
        self._lease.path()
    }
}

impl fmt::Debug for SessionWriterLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionWriterLease")
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

/// Per-workspace repository for typed Session records and journals.
#[derive(Clone, Debug)]
pub struct SessionStore {
    workspace: WorkspaceContext,
    state: WorkspaceStateStore,
    /// A local capability issuer. A lease must come from this repository
    /// instance, not merely have the same public Session ID.
    lease_issuer: Arc<()>,
}

impl SessionStore {
    pub(crate) fn new(state: WorkspaceStateStore, workspace: WorkspaceContext) -> Self {
        Self {
            workspace,
            state,
            lease_issuer: Arc::new(()),
        }
    }

    pub fn workspace(&self) -> &WorkspaceContext {
        &self.workspace
    }

    /// Create a new Draft session while holding its writer capability before
    /// the record exists. There is therefore no observable SessionRecord that
    /// was created by a process which did not own its writer lease.
    pub fn create_writer(
        &self,
        title: impl Into<String>,
    ) -> Result<(SessionRecord, SessionWriterLease), SessionStoreError> {
        let title = title.into();
        for _ in 0..CREATE_RETRY_LIMIT {
            let record = SessionRecord::new(&self.workspace, title.clone())?;
            let lease = self
                .acquire_writer_lease(record.id)
                .map_err(SessionStoreError::from)?;
            match self.create_record(record) {
                Ok(record) => return Ok((record, lease)),
                Err(SessionStoreError::AlreadyExists(_)) => {
                    drop(lease);
                    continue;
                }
                Err(error) => return Err(error),
            }
        }
        Err(SessionStoreError::IdAllocationExhausted)
    }

    /// Insert a caller-created record after the creation path has acquired
    /// its writer lease. This is deliberately private so callers cannot
    /// create an owned SessionRecord through an unleased repository API.
    fn create_record(&self, record: SessionRecord) -> Result<SessionRecord, SessionStoreError> {
        self.validate_record_for_workspace(&record)?;
        if record.revision != Revision::default() {
            return Err(SessionStoreError::ImmutableField {
                session_id: record.id,
                field: "revision on creation",
            });
        }
        let document = self.document(record.id)?;
        match document.replace(&record, Revision::default()) {
            Ok(revision) => Ok(SessionRecord { revision, ..record }),
            Err(StoreError::RevisionConflict { .. }) => {
                Err(SessionStoreError::AlreadyExists(record.id))
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn get(&self, id: SessionId) -> Result<Option<SessionRecord>, SessionStoreError> {
        let snapshot = self.document(id)?.read()?;
        snapshot
            .value
            .map(|record| self.hydrate_for_id(id, record, snapshot.revision))
            .transpose()
    }

    /// Open the append-only durable fact journal for one existing session.
    pub fn journal(&self, id: SessionId) -> Result<SessionJournal, JournalError> {
        if self.get(id)?.is_none() {
            return Err(JournalError::SessionNotFound(id));
        }
        Ok(SessionJournal::new(
            id,
            self.workspace.id(),
            Arc::clone(&self.lease_issuer),
            self.state
                .session_journal(self.workspace.id(), id)
                .map_err(JournalError::from)?,
        ))
    }

    /// Attempt to become the sole runtime and durable-turn writer. This never
    /// waits; a held lease is presented as read-only rather than freezing TUI.
    pub fn try_acquire_writer_lease(
        &self,
        id: SessionId,
    ) -> Result<SessionWriterLease, SessionLeaseError> {
        if self.get(id)?.is_none() {
            return Err(SessionLeaseError::NotFound(id));
        }
        match self.acquire_writer_lease(id) {
            Ok(lease) => Ok(lease),
            Err(StoreError::Busy) => Err(SessionLeaseError::HeldElsewhere { session_id: id }),
            Err(error) => Err(error.into()),
        }
    }

    /// Read all records for this workspace. A corrupt database causes a store
    /// repair error; unlike file scanning, BONE never silently omits rows.
    pub fn list(&self) -> Result<SessionListing, SessionStoreError> {
        let snapshots = self
            .state
            .list_sessions::<SessionRecord>(self.workspace.id())?;
        let mut seen_ids = BTreeSet::new();
        let mut records = Vec::with_capacity(snapshots.len());
        for snapshot in snapshots {
            let Some(record) = snapshot.value else {
                continue;
            };
            let record = self.hydrate(record, snapshot.revision)?;
            if !seen_ids.insert(record.id) {
                return Err(SessionStoreError::DuplicateSessionId(record.id));
            }
            records.push(record);
        }
        records.sort_by(|left, right| {
            right
                .metadata
                .last_opened_at
                .cmp(&left.metadata.last_opened_at)
                .then_with(|| right.metadata.updated_at.cmp(&left.metadata.updated_at))
                .then_with(|| right.id.cmp(&left.id))
        });
        Ok(SessionListing {
            records,
            issues: Vec::new(),
        })
    }

    /// Replace mutable record fields using document revision CAS. IDs,
    /// workspace binding, canonical root, and creation time remain immutable.
    pub fn replace(
        &self,
        lease: &SessionWriterLease,
        mut record: SessionRecord,
        expected_revision: Revision,
    ) -> Result<SessionRecord, SessionStoreError> {
        self.require_writer_lease(lease, record.id)?;
        let current = self
            .get(record.id)?
            .ok_or(SessionStoreError::NotFound(record.id))?;
        self.validate_replace(&current, &record, expected_revision)?;
        record.metadata.updated_at = UnixMillis::now()?.max(current.metadata.updated_at);
        record.validate()?;
        let document = self.document(record.id)?;
        let revision = document
            .replace(&record, expected_revision)
            .map_err(|error| map_store_error(record.id, expected_revision, error))?;
        record.revision = revision;
        Ok(record)
    }

    pub fn update_draft(
        &self,
        lease: &SessionWriterLease,
        id: SessionId,
        expected_revision: Revision,
        draft: SessionDraft,
    ) -> Result<SessionRecord, SessionStoreError> {
        draft.validate()?;
        let mut record = self.get(id)?.ok_or(SessionStoreError::NotFound(id))?;
        record.draft = draft;
        self.replace(lease, record, expected_revision)
    }

    pub fn update_status(
        &self,
        lease: &SessionWriterLease,
        id: SessionId,
        expected_revision: Revision,
        status: SessionStatus,
    ) -> Result<SessionRecord, SessionStoreError> {
        let mut record = self.get(id)?.ok_or(SessionStoreError::NotFound(id))?;
        record.status = status;
        self.replace(lease, record, expected_revision)
    }

    pub fn rename(
        &self,
        lease: &SessionWriterLease,
        id: SessionId,
        expected_revision: Revision,
        title: impl Into<String>,
    ) -> Result<SessionRecord, SessionStoreError> {
        let mut record = self.get(id)?.ok_or(SessionStoreError::NotFound(id))?;
        record.metadata.title = title.into();
        self.replace(lease, record, expected_revision)
    }

    pub fn mark_opened(
        &self,
        lease: &SessionWriterLease,
        id: SessionId,
        expected_revision: Revision,
    ) -> Result<SessionRecord, SessionStoreError> {
        let mut record = self.get(id)?.ok_or(SessionStoreError::NotFound(id))?;
        record.metadata.last_opened_at = UnixMillis::now()?;
        self.replace(lease, record, expected_revision)
    }

    /// Change only the Session-scoped model override through the same writer
    /// capability and CAS path as every other SessionRecord mutation.
    pub fn set_solver_model_override(
        &self,
        lease: &SessionWriterLease,
        id: SessionId,
        expected_revision: Revision,
        selection: Option<ModelSelection>,
    ) -> Result<SessionRecord, SessionStoreError> {
        if let Some(selection) = &selection {
            selection
                .validate()
                .map_err(|_| super::RecordError::InvalidModelOverride)?;
        }
        let mut record = self.get(id)?.ok_or(SessionStoreError::NotFound(id))?;
        record.metadata.solver_model_override = selection;
        self.replace(lease, record, expected_revision)
    }

    /// Atomically record durable user-turn acceptance and its matching session
    /// summary. The caller has already validated writer ownership and built
    /// `record` with an empty draft plus Queued/Opening status. On any SQLite
    /// failure both changes roll back, so the TUI keeps its composer and does
    /// not schedule the Agent effect.
    pub fn replace_and_append(
        &self,
        lease: &SessionWriterLease,
        mut record: SessionRecord,
        expected_revision: Revision,
        fact: JournalFact,
    ) -> Result<(SessionRecord, JournalEntry), SessionStoreError> {
        self.require_writer_lease(lease, record.id)?;
        fact.validate()
            .map_err(|error| SessionStoreError::InvalidTurn {
                message: error.to_string(),
            })?;
        let current = self
            .get(record.id)?
            .ok_or(SessionStoreError::NotFound(record.id))?;
        self.validate_replace(&current, &record, expected_revision)?;
        record.metadata.updated_at = UnixMillis::now()?.max(current.metadata.updated_at);
        record.validate()?;
        let document = self.document(record.id)?;
        let journal = self
            .state
            .session_journal(self.workspace.id(), record.id)
            .map_err(SessionStoreError::from)?;
        let (revision, stored_entry) = self
            .state
            .transaction(|transaction| {
                let revision = transaction.replace(&document, &record, expected_revision)?;
                let entry = transaction.append(&journal, &fact)?;
                Ok((revision, entry))
            })
            .map_err(|error| map_store_error(record.id, expected_revision, error))?;
        record.revision = revision;
        let entry = JournalEntry::from_store(stored_entry).map_err(|error| {
            SessionStoreError::InvalidTurn {
                message: error.to_string(),
            }
        })?;
        Ok((record, entry))
    }

    fn document(&self, id: SessionId) -> Result<Document<SessionRecord>, SessionStoreError> {
        self.state
            .session(self.workspace.id(), id)
            .map_err(SessionStoreError::from)
    }

    fn require_writer_lease(
        &self,
        lease: &SessionWriterLease,
        session_id: SessionId,
    ) -> Result<(), SessionStoreError> {
        lease.assert_grants_write(self.workspace.id(), session_id, &self.lease_issuer)
    }

    fn acquire_writer_lease(&self, id: SessionId) -> Result<SessionWriterLease, StoreError> {
        self.state
            .try_acquire_session_writer_lease(id)
            .map(|lease| SessionWriterLease {
                session_id: id,
                workspace_id: self.workspace.id(),
                issuer: Arc::clone(&self.lease_issuer),
                _lease: lease,
            })
    }

    fn hydrate_for_id(
        &self,
        key_id: SessionId,
        record: SessionRecord,
        revision: Revision,
    ) -> Result<SessionRecord, SessionStoreError> {
        if record.id != key_id {
            return Err(SessionStoreError::RecordKeyMismatch {
                key_id,
                record_id: record.id,
            });
        }
        self.hydrate(record, revision)
    }

    fn hydrate(
        &self,
        mut record: SessionRecord,
        revision: Revision,
    ) -> Result<SessionRecord, SessionStoreError> {
        record.revision = revision;
        self.validate_record_for_workspace(&record)?;
        Ok(record)
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

    fn validate_replace(
        &self,
        current: &SessionRecord,
        next: &SessionRecord,
        expected_revision: Revision,
    ) -> Result<(), SessionStoreError> {
        self.validate_record_for_workspace(next)?;
        if current.revision != expected_revision || next.revision != expected_revision {
            return Err(SessionStoreError::RevisionConflict {
                session_id: current.id,
                expected: expected_revision,
                actual: current.revision,
            });
        }
        assert_immutable(current, next)?;
        if next.metadata.last_opened_at < current.metadata.last_opened_at {
            return Err(SessionStoreError::Record(
                super::RecordError::NonMonotonicTimestamp,
            ));
        }
        Ok(())
    }
}

fn map_store_error(
    session_id: SessionId,
    expected: Revision,
    error: StoreError,
) -> SessionStoreError {
    match error {
        StoreError::RevisionConflict { actual, .. } => SessionStoreError::RevisionConflict {
            session_id,
            expected,
            actual,
        },
        error => SessionStoreError::Store(error),
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
) -> Result<(), super::RecordError> {
    if value.trim().is_empty() {
        return Err(super::RecordError::EmptyTitle);
    }
    if value.len() > maximum_bytes {
        return Err(super::RecordError::ValueTooLong {
            field,
            maximum_bytes,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use bone_store::{BoneStore, Revision, StoreRoots};

    use super::*;
    use crate::{CanonicalPath, WorkspaceContext, WorkspaceRegistry};

    fn application() -> (tempfile::TempDir, tempfile::TempDir, SessionStore) {
        let temporary = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(
            temporary.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let project = tempfile::tempdir().unwrap();
        let store = BoneStore::open_at(
            StoreRoots::new(
                temporary.path().join("data"),
                temporary.path().join("config"),
            )
            .unwrap(),
        )
        .unwrap();
        let registry = WorkspaceRegistry::new(store.workspace_state());
        let canonical = CanonicalPath::new(std::fs::canonicalize(project.path()).unwrap()).unwrap();
        let workspace = WorkspaceContext::from_canonical(
            registry.resolve_or_create_canonical(&canonical).unwrap(),
            canonical,
            project.path(),
        )
        .unwrap();
        (
            temporary,
            project,
            SessionStore::new(store.workspace_state(), workspace),
        )
    }

    #[test]
    fn document_revision_is_the_only_session_cas_token() {
        let (_temporary, _project, sessions) = application();
        let (first, lease) = sessions.create_writer("New conversation").unwrap();
        assert_eq!(first.revision.value(), 1);
        let reopened = sessions.get(first.id).unwrap().unwrap();
        assert_eq!(reopened.revision, first.revision);
        let renamed = sessions
            .rename(&lease, first.id, first.revision, "Renamed")
            .unwrap();
        assert!(renamed.revision > first.revision);
        assert!(matches!(
            sessions.rename(&lease, first.id, first.revision, "Stale"),
            Err(SessionStoreError::RevisionConflict { .. })
        ));
    }

    #[test]
    fn mutations_are_capability_gated_and_reject_a_wrong_session_lease() {
        let (_temporary, _project, sessions) = application();
        let (first, first_lease) = sessions.create_writer("First conversation").unwrap();
        let (second, second_lease) = sessions.create_writer("Second conversation").unwrap();

        // SessionStore exposes no mutable SessionRecord API without a
        // SessionWriterLease. Supplying a lease for a different conversation
        // is rejected before any SQLite mutation is attempted.
        assert!(matches!(
            sessions.rename(&second_lease, first.id, first.revision, "Must not write"),
            Err(SessionStoreError::WriterLeaseMismatch {
                session_id,
                lease_session_id,
            }) if session_id == first.id && lease_session_id == second.id
        ));
        assert_eq!(sessions.get(first.id).unwrap().unwrap(), first);

        let renamed = sessions
            .rename(&first_lease, first.id, first.revision, "Owned write")
            .unwrap();
        assert_eq!(renamed.metadata.title, "Owned write");
    }

    #[test]
    fn journal_append_requires_the_matching_writer_lease() {
        let (_temporary, _project, sessions) = application();
        let (first, first_lease) = sessions.create_writer("First conversation").unwrap();
        let (second, second_lease) = sessions.create_writer("Second conversation").unwrap();
        let journal = sessions.journal(first.id).unwrap();
        let fact = JournalFact::RuntimeInterrupted {
            reason: "test shutdown".into(),
        };

        assert!(matches!(
            journal.append(&second_lease, fact.clone()),
            Err(JournalError::Session(SessionStoreError::WriterLeaseMismatch {
                session_id,
                lease_session_id,
            })) if session_id == first.id && lease_session_id == second.id
        ));
        assert!(journal.read().unwrap().entries.is_empty());

        journal.append(&first_lease, fact).unwrap();
        assert_eq!(journal.read().unwrap().entries.len(), 1);
    }

    #[test]
    fn rejects_a_record_whose_payload_id_does_not_match_its_document_key() {
        let (_temporary, _project, sessions) = application();
        let (keyed_record, _lease) = sessions.create_writer("Keyed conversation").unwrap();
        let mismatched_record =
            SessionRecord::new(sessions.workspace(), "Different conversation").unwrap();
        sessions
            .document(keyed_record.id)
            .unwrap()
            .replace(&mismatched_record, keyed_record.revision)
            .unwrap();

        assert!(matches!(
            sessions.get(keyed_record.id),
            Err(SessionStoreError::RecordKeyMismatch {
                key_id,
                record_id,
            }) if key_id == keyed_record.id && record_id == mismatched_record.id
        ));
    }

    #[test]
    fn rejects_duplicate_session_payload_ids_in_one_workspace_listing() {
        let (_temporary, _project, sessions) = application();
        let (first, _lease) = sessions.create_writer("First conversation").unwrap();
        let duplicate_key = SessionId::new();
        sessions
            .document(duplicate_key)
            .unwrap()
            .replace(&first, Revision::default())
            .unwrap();

        assert!(matches!(
            sessions.list(),
            Err(SessionStoreError::DuplicateSessionId(id)) if id == first.id
        ));
    }
}
