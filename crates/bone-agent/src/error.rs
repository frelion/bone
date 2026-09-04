use std::{fmt, time::Duration};

use bone_llm::{Error as ModelError, FinishReason};
use thiserror::Error;

/// Why the agent could not produce a reply.
#[derive(Error)]
pub enum AgentError {
    /// The model request used to choose the next action failed.
    #[error("agent decision failed")]
    Model(#[source] ModelError),

    /// The model did not decide before the request deadline.
    #[error("agent decision timed out after {timeout:?}")]
    ModelTimeout { timeout: Duration },

    /// The model stopped without either starting an action or replying.
    #[error("agent stopped without a reply (finish reason: {finish_reason:?})")]
    Incomplete { finish_reason: Option<FinishReason> },

    /// The agent kept creating more work instead of returning to the user.
    #[error("agent exceeded its {limit}-decision limit")]
    DecisionLimit { limit: usize },

    /// One decision attempted to create an unsafe amount of parallel work.
    #[error("agent requested {requested} actions in one decision; the limit is {limit}")]
    ActionLimit { requested: usize, limit: usize },
}

impl fmt::Debug for AgentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

/// Why an action could not produce a final answer.
#[derive(Error)]
pub enum ActionError {
    /// The model request failed before a turn could be committed.
    #[error("model turn failed")]
    Model(#[source] ModelError),

    /// A model request did not finish before the agent deadline.
    #[error("model turn timed out after {timeout:?}")]
    ModelTimeout { timeout: Duration },

    /// The model stopped without producing a usable final answer.
    #[error("model stopped without a final answer (finish reason: {finish_reason:?})")]
    Incomplete { finish_reason: Option<FinishReason> },

    /// The action kept asking for more turns beyond the configured safety bound.
    #[error("action exceeded its {limit}-turn limit")]
    TurnLimit { limit: usize },

    /// The model requested an unsafe amount of parallel work in one turn.
    #[error("model requested {requested} tool calls in one turn; the limit is {limit}")]
    ToolCallLimit { requested: usize, limit: usize },
}

impl fmt::Debug for ActionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

/// Invalid agent construction.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AgentConfigError {
    /// Tool names are provider-facing identifiers and must be unique.
    #[error("tool `{0}` is already registered")]
    DuplicateTool(String),

    /// The name belongs to the Agent's private control protocol.
    #[error("tool name `start_action` is reserved by the agent")]
    ReservedTool,

    /// A zero turn limit would make every action impossible to run.
    #[error("max_turns must be greater than zero")]
    ZeroTurnLimit,

    /// A zero tool-call limit would reject every tool-using turn.
    #[error("max_tool_calls_per_turn must be greater than zero")]
    ZeroToolCallLimit,

    /// A zero model timeout would reject every model request.
    #[error("model_timeout must be greater than zero")]
    ZeroModelTimeout,

    /// A zero tool timeout would make every tool call fail immediately.
    #[error("tool_timeout must be greater than zero")]
    ZeroToolTimeout,
}

#[cfg(test)]
mod tests {
    use bone_llm::ErrorKind;

    use super::{ActionError, AgentError};

    #[test]
    fn model_error_display_does_not_expose_provider_details() {
        let model_error =
            bone_llm::testing::error(ErrorKind::Provider, "sensitive provider response");
        let error = ActionError::Model(model_error);

        assert_eq!(error.to_string(), "model turn failed");
        assert_eq!(format!("{error:?}"), "model turn failed");

        let model_error =
            bone_llm::testing::error(ErrorKind::Provider, "sensitive provider response");
        let error = AgentError::Model(model_error);
        assert_eq!(error.to_string(), "agent decision failed");
        assert_eq!(format!("{error:?}"), "agent decision failed");
    }
}
