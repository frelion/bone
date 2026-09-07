//! The product application for BONE's terminal UI and one-shot CLI.
//!
//! `bone-agent` remains the execution engine. This crate owns the durable
//! workspace/session domain, the setting policies that span it, and the
//! terminal presentation that turns those facts into a product. The durable
//! and TUI implementations stay in separate modules so the package boundary
//! stays small without merging unrelated responsibilities.

#![forbid(unsafe_code)]

mod credentials;
pub mod durable;
mod product_workspace;
mod profiles;
mod providers;
mod settings;
mod storage;
pub mod tui;

pub use credentials::{
    ApiKey, ApiKeyCredentialError, ApiKeyCredentials, ChatGptAuthLease, ChatGptCredentials,
    CredentialError,
};
pub use durable::{
    CanonicalPath, JournalEntry, JournalError, JournalFact, JournalRead, JournalSequence,
    RecordError, RegistryError, RuntimeAttachment, SessionAttention, SessionAvailability,
    SessionDraft, SessionExecution, SessionId, SessionJournal, SessionLeaseError, SessionLifecycle,
    SessionListing, SessionMetadata, SessionRecord, SessionStatus, SessionStore, SessionStoreError,
    SessionStoreIssue, SessionWriter, TurnOutcome, UnixMillis, WorkspaceContext, WorkspaceError,
    WorkspaceId, WorkspaceRegistry,
};

pub use product_workspace::{
    DraftDisposition, OpenDraft, OpenWriterDraft, WorkspaceApplication, WorkspaceApplicationError,
};
pub use profiles::{
    CHATGPT_PROFILE_ID, LlmProfile, LlmProfileError, LlmProfileId, LlmProfileIdError, LlmProfiles,
    LlmProfilesError,
};
pub use providers::{ProviderConnectError, ProviderConnector};
pub use settings::{
    ApplyBoundary, GlobalAgentSettings, GlobalSettings, ModelResolution, ModelSelection,
    ModelSelectionError, PUBLIC_SETTINGS, ResolvedModel, ResolvedRuntime, Scope, ScopeKind,
    SettingDescriptor, SettingKey, SettingKeyError, SettingSource, SettingsError, SettingsService,
    TuiDisplaySettings, WorkspaceSettings,
};
pub use storage::{AppStorageError, open_default_store};
pub use tui::{TuiError, run_storage_repair, run_workspace, write_events};
