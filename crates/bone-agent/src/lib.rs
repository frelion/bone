//! A small action-oriented conversational agent.
//!
//! A caller talks to [`Agent`]; the agent decides whether to reply or create
//! one or more [`Action`]s. Each action advances one model [`Turn`] at a time.
//! Tool calls from a turn run concurrently, and a waiting action does not stop
//! another ready action from advancing.
//!
//! This crate deliberately does not model conversations, exchanges, plans, or
//! teams.

#![forbid(unsafe_code)]

mod action;
mod agent;
mod error;
mod runtime;
mod tools;

pub use action::{Action, ActionOutcome, ToolExecution, Turn};
pub use agent::{Agent, AgentReply};
pub use error::{ActionError, AgentConfigError, AgentError};
