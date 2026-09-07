//! The product application for BONE's terminal UI and one-shot CLI.
//!
//! `bone-agent` remains the execution engine. This crate owns the durable
//! workspace/session domain, the setting policies that span it, and the
//! terminal presentation that turns those facts into a product. The durable
//! and TUI implementations stay in separate modules so the package boundary
//! stays small without merging unrelated responsibilities.

#![forbid(unsafe_code)]

pub mod durable;
mod product_workspace;
mod settings;
pub mod tui;

pub use durable::{
    CanonicalPath, JournalEntry, JournalError, JournalFact, JournalRead, JournalSequence,
    RecordError, RegistryError, RuntimeAttachment, SessionAttention, SessionAvailability,
    SessionDraft, SessionExecution, SessionId, SessionJournal, SessionLeaseError, SessionLifecycle,
    SessionListing, SessionMetadata, SessionRecord, SessionStatus, SessionStore, SessionStoreError,
    SessionStoreIssue, SessionWriterLease, TurnOutcome, UnixMillis, WorkspaceContext,
    WorkspaceError, WorkspaceId, WorkspaceRegistry,
};

pub use product_workspace::{
    DraftDisposition, OpenDraft, OpenWriterDraft, WorkspaceApplication, WorkspaceApplicationError,
};
pub use settings::{
    ApplyBoundary, GlobalAgentSettings, GlobalSettings, ModelChange, ModelResolution,
    ModelSelection, ModelSelectionError, PUBLIC_SETTINGS, ResolvedModel, Scope, ScopeKind,
    SettingDescriptor, SettingKey, SettingKeyError, SettingSource, SettingsError, SettingsService,
    TuiDisplaySettings, WorkspaceSettings,
};
pub use tui::{TuiError, run_storage_repair, run_workspace, write_events};
