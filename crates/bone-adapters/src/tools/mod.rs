//! Provider-independent built-in tools for coding agents.
//!
//! Every tool implements [`Tool`]. Local coding tools
//! capture an immutable workspace boundary plus validated execution limits;
//! Bash also captures its sanitized or explicitly configured child
//! environment. Settings storage, authorization, approval, lifecycle state,
//! and provider translation remain outside this crate.
//! Native tool calls require an active Tokio runtime; [`Tool`]
//! describes BONE's execution contract, not executor independence.
//!
//! The workspace checks prevent ordinary path escape, but are not an operating
//! system sandbox or a defense against hostile concurrent path replacement.
//! Hosts that run untrusted commands must add a capability filesystem or OS
//! sandbox at the policy/execution layer.

mod bash;
mod config;
mod environment;
mod error;
mod glob;
mod grep;
mod patch;
pub mod presentation;
mod read;
mod search_walk;
mod tool;
mod workspace;

pub use bash::{BashArgs, BashOutput, BashTool};
pub use config::{ToolLimits, ToolLimitsError};
pub use environment::ToolEnvironment;
pub use error::ToolError;
pub use glob::{GlobArgs, GlobOutput, GlobTool};
pub use grep::{GrepArgs, GrepMatch, GrepOutput, GrepTool};
pub use patch::{ApplyPatchArgs, ApplyPatchChange, ApplyPatchOutput, ApplyPatchTool};
pub use read::{ReadArgs, ReadOutput, ReadTool};
pub use tool::{Tool, ToolFailure, ToolFailureKind};
