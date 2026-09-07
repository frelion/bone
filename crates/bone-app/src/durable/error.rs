use std::{io, path::PathBuf};

use bone_store::StoreError;
use thiserror::Error;

use super::{SessionId, WorkspaceId};

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

/// Failures while resolving or persisting the canonical workspace-to-ID map.
#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("workspace registry contains an invalid workspace ID for key {key}")]
    InvalidWorkspaceId { key: String },
    #[error("workspace registry contains an invalid canonical path key {key}")]
    InvalidWorkspaceKey { key: String },
    #[error("workspace registry maps more than one canonical path to workspace {id}")]
    DuplicateWorkspaceId { id: WorkspaceId },
    #[error("workspace registry is too large; it may contain at most {maximum_entries} workspaces")]
    TooManyWorkspaces { maximum_entries: usize },
    #[error(transparent)]
    Store(#[from] StoreError),
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
    #[error("session solver model override is invalid")]
    InvalidModelOverride,
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
}

/// Failures from the workspace-bound logical-session repository.
#[derive(Debug, Error)]
pub enum SessionStoreError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Record(#[from] RecordError),
    #[error("session does not exist: {0}")]
    NotFound(SessionId),
    #[error("session document key {key_id} does not match stored record ID {record_id}")]
    RecordKeyMismatch {
        key_id: SessionId,
        record_id: SessionId,
    },
    #[error("workspace session list contains duplicate stored session ID {0}")]
    DuplicateSessionId(SessionId),
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
    #[error("session {session_id} changed before it could be saved")]
    RevisionConflict { session_id: SessionId },
    #[error("session {session_id} attempted to change immutable field {field}")]
    ImmutableField {
        session_id: SessionId,
        field: &'static str,
    },
    #[error("session turn cannot be persisted: {message}")]
    InvalidTurn { message: String },
}

/// Failures while acquiring the process-lifetime writer lease for one
/// logical session. A lease is deliberately distinct from SQLite document
/// writes: it represents runtime ownership, not ordinary mutation locking.
#[derive(Debug, Error)]
pub enum SessionLeaseError {
    #[error("session does not exist: {0}")]
    NotFound(SessionId),
    #[error("conversation {session_id} is open for writing in another BONE process")]
    HeldElsewhere { session_id: SessionId },
    #[error("could not inspect a conversation before acquiring its writer lease: {0}")]
    Session(#[from] SessionStoreError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// A journal is SQLite-backed and append-only. Failure to append is separate
/// from a record mutation because the UI must not clear a composer or send an
/// Agent message until its durable acceptance fact exists.
#[derive(Debug, Error)]
pub enum JournalError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Session(#[from] SessionStoreError),
    #[error("journal belongs to a session that does not exist: {0}")]
    SessionNotFound(SessionId),
    #[error("journal entry is invalid: {message}")]
    InvalidEntry { message: String },
}
