use bone_llm::{InputItem, InputSource, Response, ToolCall};

use crate::agent::PARENT_AGENT_SOURCE;
use crate::{ActionError, ToolFailure, tools::ToolOutcome};

/// One independently advancing piece of work.
///
/// Each action owns its model transcript. This keeps another action's messages
/// from being inserted between a tool call and its result. Public callers
/// receive only settled actions through [`crate::AgentReply`].
pub struct Action {
    intent: String,
    context: Vec<InputItem>,
    turns: Vec<Turn>,
    result: Option<Result<String, ActionError>>,
}

impl Action {
    #[cfg(test)]
    pub(crate) fn new(intent: impl Into<String>) -> Self {
        Self::with_context(intent, Vec::new())
    }

    pub(crate) fn with_context(intent: impl Into<String>, context: Vec<InputItem>) -> Self {
        Self {
            intent: intent.into(),
            context,
            turns: Vec::new(),
            result: None,
        }
    }

    pub fn intent(&self) -> &str {
        &self.intent
    }

    pub fn turns(&self) -> &[Turn] {
        &self.turns
    }

    /// The action's terminal result.
    pub fn result(&self) -> Result<&str, &ActionError> {
        match self
            .result
            .as_ref()
            .expect("actions returned by Agent are settled")
        {
            Ok(output) => Ok(output),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn model_input(&self) -> Vec<InputItem> {
        let mut input = Vec::with_capacity(1 + self.context.len() + self.turns.len() * 2);
        input.extend(self.context.iter().cloned());
        input.push(InputItem::external(
            InputSource::Named(PARENT_AGENT_SOURCE.to_owned()),
            self.intent.clone(),
        ));

        for turn in &self.turns {
            if let Some(assistant) = turn.response.clone().into_item() {
                input.push(assistant);
            }
            input.extend(turn.result_items());
        }

        input
    }

    pub(crate) fn push_turn(&mut self, turn: Turn) -> usize {
        let index = self.turns.len();
        self.turns.push(turn);
        index
    }

    pub(crate) fn record_outcome(
        &mut self,
        turn: usize,
        tool: usize,
        outcome: ToolOutcome,
    ) -> bool {
        let turn = self
            .turns
            .get_mut(turn)
            .expect("pending tool points to its originating turn");
        turn.record_outcome(tool, outcome);
        !turn.is_waiting()
    }

    pub(crate) fn complete(&mut self, output: String) {
        self.result = Some(Ok(output));
    }

    pub(crate) fn fail(&mut self, error: ActionError) {
        self.result = Some(Err(error));
    }
}

/// One model decision and the tool calls produced by that decision.
pub struct Turn {
    response: Response,
    tools: Vec<ToolExecution>,
}

impl Turn {
    pub(crate) fn new(response: Response, calls: Vec<ToolCall>) -> Self {
        Self {
            response,
            tools: calls.into_iter().map(ToolExecution::new).collect(),
        }
    }

    pub(crate) fn skipped(response: Response, calls: Vec<ToolCall>, reason: &'static str) -> Self {
        Self {
            response,
            tools: calls
                .into_iter()
                .map(|call| ToolExecution {
                    call,
                    outcome: Some(ToolOutcome::skipped(reason)),
                })
                .collect(),
        }
    }

    pub fn response(&self) -> &Response {
        &self.response
    }

    pub fn tools(&self) -> &[ToolExecution] {
        &self.tools
    }

    pub(crate) fn is_waiting(&self) -> bool {
        self.tools.iter().any(|tool| tool.outcome.is_none())
    }

    pub(crate) fn record_outcome(&mut self, index: usize, outcome: ToolOutcome) {
        let execution = self
            .tools
            .get_mut(index)
            .expect("pending tool points to its originating turn");
        debug_assert!(
            execution.outcome.is_none(),
            "a tool outcome is recorded once"
        );
        execution.outcome = Some(outcome);
    }

    fn result_items(&self) -> Vec<InputItem> {
        if self.tools.is_empty() || self.is_waiting() {
            return Vec::new();
        }

        self.tools
            .iter()
            .map(|execution| {
                let outcome = execution
                    .outcome
                    .as_ref()
                    .expect("a settled turn has every tool result");
                InputItem::tool_result(&execution.call, outcome.model_output().clone())
            })
            .collect()
    }
}

/// One tool call and its eventual execution result.
pub struct ToolExecution {
    call: ToolCall,
    outcome: Option<ToolOutcome>,
}

impl ToolExecution {
    fn new(call: ToolCall) -> Self {
        Self {
            call,
            outcome: None,
        }
    }

    pub fn call(&self) -> &ToolCall {
        &self.call
    }

    /// Whether the tool completed successfully.
    pub fn is_success(&self) -> bool {
        matches!(self.settled_outcome(), ToolOutcome::Success(_))
    }

    /// Whether the tool was intentionally not executed.
    pub fn is_skipped(&self) -> bool {
        matches!(self.settled_outcome(), ToolOutcome::Skipped(_))
    }

    /// The failure when execution failed.
    pub fn failure(&self) -> Option<&ToolFailure> {
        match self.settled_outcome() {
            ToolOutcome::Failure(failure) => Some(failure),
            ToolOutcome::Success(_) | ToolOutcome::Skipped(_) => None,
        }
    }

    /// The exact observation returned to the model.
    pub fn model_output(&self) -> &bone_llm::ToolOutput {
        self.settled_outcome().model_output()
    }

    fn settled_outcome(&self) -> &ToolOutcome {
        self.outcome
            .as_ref()
            .expect("tool executions returned by Agent are settled")
    }
}
