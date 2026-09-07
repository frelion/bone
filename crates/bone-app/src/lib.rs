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
    CanonicalPath, JournalEntry, JournalError, JournalFact, JournalRead, JournalRecoveryIssue,
    JournalSequence, RecordError, RegistryError, RuntimeAttachment, SessionAttention,
    SessionAvailability, SessionDraft, SessionExecution, SessionId, SessionJournal,
    SessionLeaseError, SessionLifecycle, SessionListing, SessionMetadata, SessionRecord,
    SessionRevision, SessionStatus, SessionStore, SessionStoreError, SessionStoreIssue,
    SessionWriterLease, StorageError, TurnOutcome, UnixMillis, WorkspaceContext, WorkspaceError,
    WorkspaceId, WorkspaceRegistry,
};

pub use product_workspace::{
    DraftDisposition, OpenDraft, OpenWriterDraft, StateRoot, StateRootEnvironment, StateRootError,
    StateRootSource, WorkspaceApplication, WorkspaceApplicationError, resolve_state_root,
};
pub use settings::{
    ApplyBoundary, ApplyState, EffectiveRevision, ModelChange, ModelOverrides, ModelResolution,
    ModelSelection, ModelSelectionError, PUBLIC_SETTINGS, ResolvedModel, RevisionError, Scope,
    ScopeKind, SettingDescriptor, SettingKey, SettingKeyError, SettingSource, SettingsError,
    SettingsService, TuiDisplaySettings, TurnConfig, TurnConfigError, model_apply_state,
};
pub use tui::{TuiConfig, TuiError, run_workspace, write_events};
