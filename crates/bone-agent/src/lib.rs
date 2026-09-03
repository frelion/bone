//! A small action-oriented agent runtime.
//!
//! An [`Agent`] advances independent [`Action`]s one model [`Turn`] at a time.
//! Tool calls from a turn run concurrently. While one action is waiting for a
//! long-running tool, the agent can advance another ready action.
//!
//! This crate deliberately does not model conversations, exchanges, plans, or
//! teams. Those concerns can create actions and present their progress without
//! changing the execution core.

#![forbid(unsafe_code)]

mod action;
mod agent;
mod error;
mod tools;

pub use action::{Action, ActionOutcome, ActionState, ToolExecution, Turn};
pub use agent::Agent;
pub use error::{ActionError, AgentConfigError};
