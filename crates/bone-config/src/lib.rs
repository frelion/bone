//! Typed, non-secret configuration storage shared by human interfaces and
//! agent tools.
//!
//! Consumers own their [`ConfigSection`] types. This crate owns registration,
//! validation, immutable snapshots, compare-and-swap revisions, and atomic
//! persistence. Secret values use the separate [`CredentialStore`] and never
//! belong in configuration sections or model-visible tool calls.

#![forbid(unsafe_code)]

mod credential;
mod error;
mod manager;

pub use credential::{
    CredentialError, CredentialStatus, CredentialStore, SecretLease, SecretValue,
};
pub use error::ConfigError;
pub use manager::{
    ConfigChange, ConfigManager, ConfigManagerBuilder, ConfigRevision, ConfigSection,
    ConfigSectionInfo, ConfigSnapshot,
};
