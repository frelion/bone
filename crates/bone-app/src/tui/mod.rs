//! Full-screen terminal frontend and event export for the shared Agent API.

#![forbid(unsafe_code)]

mod agent_projection;
mod app;
mod command_effects;
pub mod commands;
mod config;
mod events;
mod runtime_driver;
mod session_controller;
mod terminal;
mod view;
mod workspace;

use std::io;

use self::app::{App, AppEvent};
use bone_agent::{HandleError, StartError};

pub use config::TuiConfig;
pub use events::write_events;
pub use workspace::run_workspace;

#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Agent(#[from] HandleError),
    #[error(transparent)]
    Start(#[from] StartError),
    #[error(transparent)]
    Workspace(#[from] crate::WorkspaceApplicationError),
    #[error(transparent)]
    SessionStore(#[from] crate::SessionStoreError),
}

/// Route an effect outcome through the sole presentation-state writer.
///
/// Runtime, durable-session, and command modules may produce user-facing
/// feedback, but none may mutate `App` fields directly.
pub(super) fn report_notice(app: &mut App, message: impl Into<String>) {
    let _ = app.reduce(AppEvent::Notice {
        message: message.into(),
    });
}
