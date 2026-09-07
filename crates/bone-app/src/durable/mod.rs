//! Durable, directory-scoped BONE workspace and session facts.
//!
//! This module is deliberately independent from Agent runtimes and terminal
//! presentation. It is part of `bone-app` because it has no consumers beyond
//! the product application, while its internal module boundaries preserve the
//! distinction between workspace identity, typed SQLite documents, session
//! metadata, and append-only journals.

mod error;
mod journal;
mod registry;
mod session;
mod workspace_identity;

pub use error::{
    JournalError, RecordError, RegistryError, SessionLeaseError, SessionStoreError, WorkspaceError,
};
pub use journal::{
    JournalEntry, JournalFact, JournalRead, JournalSequence, SessionJournal, TurnOutcome,
};
pub use registry::WorkspaceRegistry;
pub use session::{
    RuntimeAttachment, SessionAttention, SessionAvailability, SessionDraft, SessionExecution,
    SessionId, SessionLifecycle, SessionListing, SessionMetadata, SessionRecord, SessionStatus,
    SessionStore, SessionStoreIssue, SessionWriterLease, UnixMillis,
};
pub use workspace_identity::{CanonicalPath, WorkspaceContext, WorkspaceId};
