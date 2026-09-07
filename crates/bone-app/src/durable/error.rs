use std::{io, path::PathBuf};

use thiserror::Error;

use super::{SessionId, SessionRevision, WorkspaceId};

/// Failures while resolving the directory that defines a BONE workspace.
#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("workspace root could not be canonicalized at {path}: {source}")]
    Canonicalize {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("workspace root is not a directory: {path}")]
    NotDirectory { path: PathBuf },
    #[error("canonical workspace path must be absolute: {path}")]
    NonAbsoluteCanonicalPath { path: PathBuf },
    #[error(transparent)]
    Registry(#[from] RegistryError),
}

/// Failures in the private file primitives used by registries and sessions.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("storage path must be absolute: {path}")]
    RelativePath { path: PathBuf },
    #[error("storage path has no parent directory: {path}")]
    MissingParent { path: PathBuf },
    #[error("storage path has no file name: {path}")]
    MissingFileName { path: PathBuf },
    #[error("storage is busy: {path}")]
    Busy { path: PathBuf },
    #[error("unsafe private storage at {path}: {reason}")]
    UnsafeStorage { path: PathBuf, reason: String },
    #[error("storage document at {path} exceeds the {maximum_bytes}-byte limit")]
    DocumentTooLarge { path: PathBuf, maximum_bytes: usize },
    #[error("storage document at {path} is invalid: {message}")]
    InvalidDocument { path: PathBuf, message: String },
    #[error("failed to {operation} storage at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl StorageError {
    pub(crate) fn io(operation: &'static str, path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            operation,
            path: path.into(),
            source,
        }
    }
}

/// Failures while resolving or persisting a canonical workspace-to-ID mapping.
#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("workspace registry has an unsupported format version: {version}")]
    UnsupportedFormat { version: u32 },
    #[error("workspace registry contains an invalid workspace ID for key {key}")]
    InvalidWorkspaceId { key: String },
    #[error("workspace registry is too large; it may contain at most {maximum_entries} workspaces")]
    TooManyWorkspaces { maximum_entries: usize },
    #[error(transparent)]
    Storage(#[from] StorageError),
}

/// Invalid user-visible session metadata or durable session state.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RecordError {
    #[error("session title must not be empty")]
    EmptyTitle,
    #[error("{field} exceeds the {maximum_bytes}-byte limit")]
    ValueTooLong {
        field: &'static str,
        maximum_bytes: usize,
    },
    #[error("draft cursor must be a UTF-8 character boundary within the draft")]
    InvalidDraftCursor,
    #[error("session timestamp must not be before the Unix epoch")]
    InvalidTimestamp,
    #[error("session timestamps must not move backwards")]
    NonMonotonicTimestamp,
    #[error("session canonical workspace path must be absolute")]
    NonAbsoluteWorkspacePath,
    #[error("session ID must not be nil")]
    NilSessionId,
    #[error("workspace ID must not be nil")]
    NilWorkspaceId,
    #[error("system clock is before the Unix epoch")]
    ClockBeforeUnixEpoch,
    #[error("system clock cannot be represented as Unix milliseconds")]
    ClockOutOfRange,
    #[error("session revision cannot be incremented further")]
    RevisionExhausted,
}

/// Failures from the per-workspace durable session metadata store.
#[derive(Debug, Error)]
pub enum SessionStoreError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Record(#[from] RecordError),
    #[error("session does not exist: {0}")]
    NotFound(SessionId),
    #[error("session already exists: {0}")]
    AlreadyExists(SessionId),
    #[error("could not allocate a unique session ID after several attempts")]
    IdAllocationExhausted,
    #[error(
        "session {session_id} belongs to workspace {actual_workspace}, not this workspace {expected_workspace}"
    )]
    WorkspaceMismatch {
        session_id: SessionId,
        expected_workspace: WorkspaceId,
        actual_workspace: WorkspaceId,
    },
    #[error(
        "session {session_id} changed since it was read (expected revision {expected}, actual {actual})"
    )]
    RevisionConflict {
        session_id: SessionId,
        expected: SessionRevision,
        actual: SessionRevision,
    },
    #[error("session {session_id} attempted to change immutable field {field}")]
    ImmutableField {
        session_id: SessionId,
        field: &'static str,
    },
    #[error("session {session_id} is corrupt at {path}: {message}")]
    CorruptSession {
        session_id: SessionId,
        path: PathBuf,
        message: String,
    },
}

/// Failures while acquiring the process-lifetime writer lease for one
/// logical session.
///
/// A writer lease is deliberately distinct from the short metadata/journal
/// locks. The latter serialize one filesystem operation; this lease grants a
/// single BONE process ownership of runtime attachment and durable turn
/// writes for the lifetime of an active conversation.
#[derive(Debug, Error)]
pub enum SessionLeaseError {
    #[error("session does not exist: {0}")]
    NotFound(SessionId),
    #[error("conversation {session_id} is open for writing in another BONE process")]
    HeldElsewhere { session_id: SessionId },
    #[error("could not inspect a conversation before acquiring its writer lease: {0}")]
    Session(#[from] SessionStoreError),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

/// A journal exists beside each durable session record and holds ordered
/// conversation facts. A journal failure is deliberately distinct from a
/// metadata-store failure: callers must not clear a composer or retry an
/// external effect merely because the journal could not confirm a fact.
#[derive(Debug, Error)]
pub enum JournalError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Session(#[from] SessionStoreError),
    #[error("journal belongs to a session that does not exist: {0}")]
    SessionNotFound(SessionId),
    #[error(
        "journal for session {session_id} changed since it was read (expected next sequence {expected_next}, actual {actual_next})"
    )]
    SequenceConflict {
        session_id: SessionId,
        expected_next: crate::JournalSequence,
        actual_next: crate::JournalSequence,
    },
    #[error(
        "journal for session {session_id} needs recovery at {path}; append is disabled until the incomplete or corrupt tail is repaired"
    )]
    NeedsRecovery {
        session_id: SessionId,
        path: PathBuf,
    },
    #[error("journal entry is invalid: {message}")]
    InvalidEntry { message: String },
}
