//! Durable, directory-scoped BONE workspace and session facts.
//!
//! This module is deliberately independent from Agent runtimes and terminal
//! presentation. It is part of `bone-app` because it has no consumers beyond
//! the product application, while its internal module boundaries preserve the
//! distinction between workspace identity, private storage, session metadata,
//! and append-only journals.

mod error;
mod journal;
mod registry;
mod session;
mod storage;
mod workspace_identity;

pub use error::{
    JournalError, RecordError, RegistryError, SessionLeaseError, SessionStoreError, StorageError,
    WorkspaceError,
};
pub use journal::{
    JournalEntry, JournalFact, JournalRead, JournalRecoveryIssue, JournalSequence, SessionJournal,
    TurnOutcome,
};
pub use registry::WorkspaceRegistry;
pub use session::{
    RuntimeAttachment, SessionAttention, SessionAvailability, SessionDraft, SessionExecution,
    SessionId, SessionLifecycle, SessionListing, SessionMetadata, SessionRecord, SessionRevision,
    SessionStatus, SessionStore, SessionStoreIssue, SessionWriterLease, UnixMillis,
};
pub use workspace_identity::{CanonicalPath, WorkspaceContext, WorkspaceId};
