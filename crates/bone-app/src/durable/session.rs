use std::{
    collections::BTreeSet,
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

use bone_store::{BoneStore, Document, Lease, Revision, StoreError};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ModelSelection;

use super::{
    CanonicalPath, JournalEntry, JournalError, JournalFact, SessionJournal, SessionLeaseError,
    SessionStoreError, WorkspaceContext, WorkspaceId, keys,
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

impl std::str::FromStr for SessionId {
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
/// runtime handle, provider credential, socket, in-flight future, or storage
/// revision. The repository owns the latter as its private CAS token.
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

/// A non-fatal issue found when listing sessions. A damaged SQLite row must not
/// prevent the user from opening every other healthy session in the workspace.
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

/// A value paired with the repository's private optimistic-concurrency token.
///
/// This is deliberately not exposed by the Session API: callers can read a
/// record, while a `SessionWriter` carries the revision needed to update it.
#[derive(Clone, Debug)]
struct Stored<T> {
    value: T,
    revision: Revision,
}

/// Per-workspace repository for typed Session records and journals.
#[derive(Clone, Debug)]
pub struct SessionStore {
    workspace: WorkspaceContext,
    store: BoneStore,
}

impl SessionStore {
    pub(crate) fn new(store: BoneStore, workspace: WorkspaceContext) -> Self {
        Self { workspace, store }
    }

    pub fn workspace(&self) -> &WorkspaceContext {
        &self.workspace
    }

    /// Create a new session and retain its process-lifetime writer ownership.
    pub fn create_writer(
        &self,
        title: impl Into<String>,
    ) -> Result<SessionWriter, SessionStoreError> {
        let title = title.into();
        for _ in 0..CREATE_RETRY_LIMIT {
            let record = SessionRecord::new(&self.workspace, title.clone())?;
            let lease = match self
                .store
                .try_acquire_lease(keys::session_writer_lease(record.id))
            {
                Ok(lease) => lease,
                Err(StoreError::Busy) => continue,
                Err(error) => return Err(error.into()),
            };
            let document = self.document(record.id);
            match document.replace(&record, Revision::default()) {
                Ok(revision) => {
                    return Ok(SessionWriter {
                        sessions: self.clone(),
                        stored: Stored {
                            value: record,
                            revision,
                        },
                        _lease: lease,
                    });
                }
                Err(StoreError::RevisionConflict { .. }) => drop(lease),
                Err(error) => return Err(error.into()),
            }
        }
        Err(SessionStoreError::IdAllocationExhausted)
    }

    /// Read a session without acquiring its writer ownership.
    pub fn get(&self, id: SessionId) -> Result<Option<SessionRecord>, SessionStoreError> {
        self.read_stored(id)
            .map(|stored| stored.map(|stored| stored.value))
    }

    /// Open the append-only durable fact journal for one existing session.
    /// Reading journals is deliberately lease-free so another BONE process can
    /// render a held conversation as read-only.
    pub fn journal(&self, id: SessionId) -> Result<SessionJournal, JournalError> {
        if self.get(id)?.is_none() {
            return Err(JournalError::SessionNotFound(id));
        }
        Ok(SessionJournal::new(id, self.journal_handle(id)))
    }

    /// Acquire the one writer for an existing session. The record is read
    /// after the lease is held, so the writer starts from the current CAS
    /// revision without exposing that revision to its caller.
    pub fn try_open_writer(&self, id: SessionId) -> Result<SessionWriter, SessionLeaseError> {
        let lease = match self.store.try_acquire_lease(keys::session_writer_lease(id)) {
            Ok(lease) => lease,
            Err(StoreError::Busy) => {
                return Err(SessionLeaseError::HeldElsewhere { session_id: id });
            }
            Err(error) => return Err(error.into()),
        };
        let stored = self
            .read_stored(id)
            .map_err(SessionLeaseError::Session)?
            .ok_or(SessionLeaseError::NotFound(id))?;
        Ok(SessionWriter {
            sessions: self.clone(),
            stored,
            _lease: lease,
        })
    }

    /// Read all valid records for this workspace. Individual damaged rows are
    /// reported in `issues` instead of aborting the complete listing.
    pub fn list(&self) -> Result<SessionListing, SessionStoreError> {
        let prefix = keys::session_prefix(self.workspace.id());
        let entries = self
            .store
            .list_documents::<SessionRecord>(keys::STATE_NAMESPACE, &prefix)?;
        let mut seen_ids = BTreeSet::new();
        let mut listing = SessionListing {
            records: Vec::with_capacity(entries.len()),
            issues: Vec::new(),
        };
        for entry in entries {
            let Some(suffix) = entry.key.key().strip_prefix(&prefix) else {
                listing.issues.push(SessionStoreIssue {
                    session_id: None,
                    message: "session document key is outside the requested workspace prefix"
                        .to_owned(),
                });
                continue;
            };
            let key_id = match SessionId::parse_str(suffix) {
                Ok(id) => id,
                Err(_) => {
                    listing.issues.push(SessionStoreIssue {
                        session_id: None,
                        message: "session document key suffix is not a UUID".to_owned(),
                    });
                    continue;
                }
            };
            let snapshot = match entry.snapshot {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    listing.issues.push(SessionStoreIssue {
                        session_id: Some(key_id),
                        message: error.to_string(),
                    });
                    continue;
                }
            };
            let Some(record) = snapshot.value else {
                listing.issues.push(SessionStoreIssue {
                    session_id: Some(key_id),
                    message: "listed session document is unexpectedly missing".to_owned(),
                });
                continue;
            };
            let record = match self.hydrate_for_id(key_id, record) {
                Ok(record) => record,
                Err(error) => {
                    listing.issues.push(SessionStoreIssue {
                        session_id: Some(key_id),
                        message: error.to_string(),
                    });
                    continue;
                }
            };
            if !seen_ids.insert(record.id) {
                listing.issues.push(SessionStoreIssue {
                    session_id: Some(key_id),
                    message: SessionStoreError::DuplicateSessionId(record.id).to_string(),
                });
                continue;
            }
            listing.records.push(record);
        }
        listing.records.sort_by(|left, right| {
            right
                .metadata
                .last_opened_at
                .cmp(&left.metadata.last_opened_at)
                .then_with(|| right.metadata.updated_at.cmp(&left.metadata.updated_at))
                .then_with(|| right.id.cmp(&left.id))
        });
        Ok(listing)
    }

    fn document(&self, id: SessionId) -> Document<SessionRecord> {
        self.store
            .document(keys::session_document(self.workspace.id(), id))
    }

    fn journal_handle(&self, id: SessionId) -> bone_store::Journal<JournalFact> {
        self.store
            .journal(keys::session_journal(self.workspace.id(), id))
    }

    fn read_stored(
        &self,
        id: SessionId,
    ) -> Result<Option<Stored<SessionRecord>>, SessionStoreError> {
        let snapshot = self.document(id).read()?;
        let Some(record) = snapshot.value else {
            return Ok(None);
        };
        let record = self.hydrate_for_id(id, record)?;
        Ok(Some(Stored {
            value: record,
            revision: snapshot.revision,
        }))
    }

    fn hydrate_for_id(
        &self,
        key_id: SessionId,
        record: SessionRecord,
    ) -> Result<SessionRecord, SessionStoreError> {
        if record.id != key_id {
            return Err(SessionStoreError::RecordKeyMismatch {
                key_id,
                record_id: record.id,
            });
        }
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
    ) -> Result<(), SessionStoreError> {
        self.validate_record_for_workspace(next)?;
        assert_immutable(current, next)?;
        if next.metadata.last_opened_at < current.metadata.last_opened_at {
            return Err(SessionStoreError::Record(
                super::RecordError::NonMonotonicTimestamp,
            ));
        }
        Ok(())
    }
}

/// The sole holder of a session's process-lifetime writer lease.
///
/// It also owns the private CAS revision, so callers cannot accidentally pass
/// a lease for one session with a record or revision from another.
#[must_use = "dropping a SessionWriter immediately releases writer ownership"]
pub struct SessionWriter {
    sessions: SessionStore,
    stored: Stored<SessionRecord>,
    _lease: Lease,
}

impl fmt::Debug for SessionWriter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionWriter")
            .field("session_id", &self.stored.value.id)
            .finish_non_exhaustive()
    }
}

impl SessionWriter {
    pub fn session_id(&self) -> SessionId {
        self.stored.value.id
    }

    pub fn record(&self) -> &SessionRecord {
        &self.stored.value
    }

    pub fn replace(&mut self, next: SessionRecord) -> Result<(), SessionStoreError> {
        let next = self.prepare_replace(next)?;
        let revision = self
            .sessions
            .document(next.id)
            .replace(&next, self.stored.revision)
            .map_err(|error| map_store_error(next.id, error))?;
        self.stored = Stored {
            value: next,
            revision,
        };
        Ok(())
    }

    pub fn update_draft(&mut self, draft: SessionDraft) -> Result<(), SessionStoreError> {
        draft.validate()?;
        self.mutate(|record| record.draft = draft)
    }

    pub fn update_status(&mut self, status: SessionStatus) -> Result<(), SessionStoreError> {
        self.mutate(|record| record.status = status)
    }

    pub fn rename(&mut self, title: impl Into<String>) -> Result<(), SessionStoreError> {
        self.mutate(|record| record.metadata.title = title.into())
    }

    pub fn mark_opened(&mut self) -> Result<(), SessionStoreError> {
        let opened_at = UnixMillis::now()?;
        self.mutate(|record| record.metadata.last_opened_at = opened_at)
    }

    pub fn set_solver_model_override(
        &mut self,
        selection: Option<ModelSelection>,
    ) -> Result<(), SessionStoreError> {
        if let Some(selection) = &selection {
            selection
                .validate()
                .map_err(|_| super::RecordError::InvalidModelOverride)?;
        }
        self.mutate(|record| record.metadata.solver_model_override = selection)
    }

    /// Append an independently durable fact. The writer's ownership makes a
    /// separate lease parameter unnecessary.
    pub fn append(&mut self, fact: JournalFact) -> Result<JournalEntry, JournalError> {
        fact.validate()?;
        let appended = self
            .sessions
            .journal_handle(self.session_id())
            .append(&fact)?;
        JournalEntry::from_append(fact, appended)
    }

    /// Atomically persist a changed session summary and the matching accepted
    /// user-turn fact. On failure neither mutation commits.
    pub fn accept_turn(
        &mut self,
        next: SessionRecord,
        fact: JournalFact,
    ) -> Result<JournalEntry, SessionStoreError> {
        fact.validate()
            .map_err(|error| SessionStoreError::InvalidTurn {
                message: error.to_string(),
            })?;
        let next = self.prepare_replace(next)?;
        let document = self.sessions.document(next.id);
        let journal = self.sessions.journal_handle(next.id);
        let expected = self.stored.revision;
        let (revision, appended) = self
            .sessions
            .store
            .transaction(|transaction| {
                let revision = transaction.replace(&document, &next, expected)?;
                let appended = transaction.append(&journal, &fact)?;
                Ok((revision, appended))
            })
            .map_err(|error| map_store_error(next.id, error))?;
        let entry = JournalEntry::from_append(fact, appended).map_err(|error| {
            SessionStoreError::InvalidTurn {
                message: error.to_string(),
            }
        })?;
        self.stored = Stored {
            value: next,
            revision,
        };
        Ok(entry)
    }

    fn mutate(
        &mut self,
        operation: impl FnOnce(&mut SessionRecord),
    ) -> Result<(), SessionStoreError> {
        let mut next = self.stored.value.clone();
        operation(&mut next);
        self.replace(next)
    }

    fn prepare_replace(&self, mut next: SessionRecord) -> Result<SessionRecord, SessionStoreError> {
        self.sessions.validate_replace(&self.stored.value, &next)?;
        next.metadata.updated_at = UnixMillis::now()?.max(self.stored.value.metadata.updated_at);
        next.validate()?;
        Ok(next)
    }
}

fn map_store_error(session_id: SessionId, error: StoreError) -> SessionStoreError {
    match error {
        StoreError::RevisionConflict { .. } => SessionStoreError::RevisionConflict { session_id },
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

    fn application() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        BoneStore,
        SessionStore,
    ) {
        let temporary = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(
            temporary.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let project = tempfile::tempdir().unwrap();
        let store =
            BoneStore::open_at(StoreRoots::new(temporary.path().join("data")).unwrap()).unwrap();
        let registry = WorkspaceRegistry::new(store.clone());
        let canonical = CanonicalPath::new(std::fs::canonicalize(project.path()).unwrap()).unwrap();
        let workspace = WorkspaceContext::from_canonical(
            registry.resolve_or_create_canonical(&canonical).unwrap(),
            canonical,
            project.path(),
        )
        .unwrap();
        let sessions = SessionStore::new(store.clone(), workspace);
        (temporary, project, store, sessions)
    }

    #[test]
    fn writer_keeps_its_revision_private_and_updates_its_own_record() {
        let (_temporary, _project, _store, sessions) = application();
        let mut writer = sessions.create_writer("New conversation").unwrap();
        let id = writer.session_id();
        writer.rename("Renamed").unwrap();

        assert_eq!(writer.record().metadata.title, "Renamed");
        assert_eq!(sessions.get(id).unwrap().unwrap(), writer.record().clone());
    }

    #[test]
    fn held_writer_is_reported_without_exposing_a_second_mutation_path() {
        let (_temporary, _project, _store, sessions) = application();
        let writer = sessions.create_writer("First conversation").unwrap();

        assert!(matches!(
            sessions.try_open_writer(writer.session_id()),
            Err(SessionLeaseError::HeldElsewhere { .. })
        ));
    }

    #[test]
    fn writer_appends_facts_and_readers_remain_lease_free() {
        let (_temporary, _project, _store, sessions) = application();
        let mut writer = sessions.create_writer("First conversation").unwrap();
        let id = writer.session_id();
        writer
            .append(JournalFact::RuntimeInterrupted {
                reason: "test shutdown".into(),
            })
            .unwrap();

        assert_eq!(
            sessions.journal(id).unwrap().read().unwrap().entries.len(),
            1
        );
    }

    #[test]
    fn accepted_turn_rolls_back_the_summary_when_the_fact_is_invalid() {
        let (_temporary, _project, _store, sessions) = application();
        let mut writer = sessions.create_writer("First conversation").unwrap();
        let before = writer.record().clone();
        let mut next = before.clone();
        next.draft = SessionDraft::empty();
        next.status.execution = SessionExecution::QueuedForRuntime;

        assert!(
            writer
                .accept_turn(
                    next,
                    JournalFact::UserTurnAccepted {
                        turn: 0,
                        text: "hello".into(),
                        runtime_fingerprint: "fingerprint".into(),
                        solver_model: "model".into(),
                    },
                )
                .is_err()
        );
        assert_eq!(writer.record(), &before);
        assert!(
            sessions
                .journal(before.id)
                .unwrap()
                .read()
                .unwrap()
                .entries
                .is_empty()
        );
    }

    #[test]
    fn listing_reports_a_key_payload_mismatch_without_hiding_healthy_sessions() {
        let (_temporary, _project, store, sessions) = application();
        let healthy = sessions.create_writer("Healthy conversation").unwrap();
        let mismatched_key = SessionId::new();
        let mismatched_record =
            SessionRecord::new(sessions.workspace(), "Mismatched conversation").unwrap();
        store
            .document::<SessionRecord>(keys::session_document(
                sessions.workspace.id(),
                mismatched_key,
            ))
            .replace(&mismatched_record, Revision::default())
            .unwrap();

        let listing = sessions.list().unwrap();
        assert_eq!(listing.records, vec![healthy.record().clone()]);
        assert_eq!(listing.issues.len(), 1);
        assert_eq!(listing.issues[0].session_id, Some(mismatched_key));
        assert!(listing.issues[0].message.contains("does not match"));
    }

    #[test]
    fn listing_reports_one_decode_error_and_returns_the_healthy_session() {
        let (_temporary, _project, store, sessions) = application();
        let healthy = sessions.create_writer("Healthy conversation").unwrap();
        let damaged_id = SessionId::new();
        store
            .document::<String>(keys::session_document(sessions.workspace.id(), damaged_id))
            .replace(&"not a session record".to_owned(), Revision::default())
            .unwrap();

        let listing = sessions.list().unwrap();
        assert_eq!(listing.records, vec![healthy.record().clone()]);
        assert_eq!(listing.issues.len(), 1);
        assert_eq!(listing.issues[0].session_id, Some(damaged_id));
        assert!(listing.issues[0].message.contains("decode"));
    }
}
