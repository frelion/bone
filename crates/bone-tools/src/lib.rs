//! Provider-independent built-in tools for coding agents.
//!
//! Every tool implements [`bone_agent::Tool`]. Local coding tools
//! capture an immutable workspace boundary plus execution limits; Bash also
//! captures its sanitized or explicitly configured child environment. The
//! config tool instead captures a registered [`bone_config::ConfigManager`]
//! and a model-output limit. Registration, authorization, approval, lifecycle
//! state, and provider translation remain outside this crate.
//! Native tool calls require an active Tokio runtime; [`bone_agent::Tool`]
//! describes BONE's execution contract, not executor independence.
//!
//! The workspace checks prevent ordinary path escape, but are not an operating
//! system sandbox or a defense against hostile concurrent path replacement.
//! Hosts that run untrusted commands must add a capability filesystem or OS
//! sandbox at the policy/execution layer.

mod bash;
mod config;
mod config_tool;
mod environment;
mod error;
mod glob;
mod grep;
mod patch;
mod read;
mod search_walk;
mod workspace;

pub use bash::{BashArgs, BashOutput, BashTool};
pub use config::ToolLimits;
pub use config_tool::{ConfigArgs, ConfigListEntry, ConfigOutput, ConfigTool, ConfigToolError};
pub use environment::ToolEnvironment;
pub use error::ToolError;
pub use glob::{GlobArgs, GlobOutput, GlobTool};
pub use grep::{GrepArgs, GrepMatch, GrepOutput, GrepTool};
pub use patch::{ApplyPatchArgs, ApplyPatchChange, ApplyPatchOutput, ApplyPatchTool};
pub use read::{ReadArgs, ReadOutput, ReadTool};
