//! Headless application layer for BONE.
//!
//! [`App`] owns configuration, credentials, persistence, and live sessions.
//! [`Session`] is the only execution handle a frontend needs. Terminal, web,
//! and one-shot clients consume the same snapshots and durable history.
#![forbid(unsafe_code)]

mod api;
mod app;
mod config;
mod credentials;
mod error;
mod persistence;
mod providers;
mod session;
mod storage;
mod tools;
mod workspace_changes;

pub use api::*;
pub use app::{App, LoginAttempt};
pub use config::*;
pub use credentials::{ApiKey, ApiKeyCredentialError};
pub use error::{Error, Result};
pub use session::Session;

pub use bone_adapters::{
    llm::{EndpointConfig, ModelOptions},
    tools::ToolLimits,
};
pub use bone_core::{
    AgentLimits, CallError, CallErrorKind, ExternalEffect, InputOutcome, OutcomeKind, ToolOutcome,
};

pub(crate) use persistence::{DataStore, SavedRuntime, SavedSession};
pub(crate) use providers::ProviderConnector;

#[cfg(test)]
mod tests;
