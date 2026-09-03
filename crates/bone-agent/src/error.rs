use std::time::Duration;

use bone_provider::{CompletionError, rig::completion::FinishReason};
use thiserror::Error;

/// Why an action could not produce a final answer.
#[derive(Debug, Error)]
pub enum ActionError {
    /// The model request failed before a turn could be committed.
    #[error("model turn failed")]
    Model(#[source] CompletionError),

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

/// Invalid agent construction.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AgentConfigError {
    /// Tool names are provider-facing identifiers and must be unique.
    #[error("tool `{0}` is already registered")]
    DuplicateTool(String),

    /// A zero turn limit would make every action impossible to run.
    #[error("max_turns must be greater than zero")]
    ZeroTurnLimit,

    /// A zero tool-call limit would reject every tool-using turn.
    #[error("max_tool_calls_per_turn must be greater than zero")]
    ZeroToolCallLimit,

    /// A zero model timeout would reject every model request.
    #[error("model_timeout must be greater than zero")]
    ZeroModelTimeout,
}

#[cfg(test)]
mod tests {
    use bone_provider::CompletionError;

    use super::ActionError;

    #[test]
    fn model_error_display_does_not_expose_provider_details() {
        let error = ActionError::Model(CompletionError::ProviderError(
            "sensitive provider response".to_owned(),
        ));

        assert_eq!(error.to_string(), "model turn failed");
    }
}
