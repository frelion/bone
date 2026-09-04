use bone_llm::{InputItem, InputSource, Response, ToolCall, ToolOutput as ModelToolOutput};
use rig_core::tool::ToolResult as ExecutionToolResult;

use crate::ActionError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActionState {
    Ready,
    Waiting,
    Finished,
}

/// The terminal result of an action.
pub enum ActionOutcome {
    Completed { output: String },
    Failed(ActionError),
}

impl ActionOutcome {
    /// The final model text when the action completed successfully.
    pub fn output(&self) -> Option<&str> {
        match self {
            Self::Completed { output } => Some(output),
            Self::Failed(_) => None,
        }
    }

    /// The terminal error when the action failed.
    pub fn error(&self) -> Option<&ActionError> {
        match self {
            Self::Completed { .. } => None,
            Self::Failed(error) => Some(error),
        }
    }
}

/// One independently advancing piece of work.
///
/// Each action owns its model transcript. This keeps another action's messages
/// from being inserted between a tool call and its result. Public callers
/// receive only settled actions through [`crate::AgentReply`].
pub struct Action {
    intent: String,
    context: Vec<InputItem>,
    turns: Vec<Turn>,
    outcome: Option<ActionOutcome>,
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
            outcome: None,
        }
    }

    pub fn intent(&self) -> &str {
        &self.intent
    }

    pub fn turns(&self) -> &[Turn] {
        &self.turns
    }

    /// The action's terminal outcome.
    pub fn outcome(&self) -> &ActionOutcome {
        self.outcome
            .as_ref()
            .expect("actions returned by Agent are settled")
    }

    pub fn output(&self) -> Option<&str> {
        self.outcome.as_ref().and_then(ActionOutcome::output)
    }

    pub fn error(&self) -> Option<&ActionError> {
        self.outcome.as_ref().and_then(ActionOutcome::error)
    }

    pub(crate) fn state(&self) -> ActionState {
        if self.outcome.is_some() {
            ActionState::Finished
        } else if self.turns.last().is_some_and(Turn::is_waiting) {
            ActionState::Waiting
        } else {
            ActionState::Ready
        }
    }

    pub(crate) fn model_input(&self) -> Vec<InputItem> {
        let mut input = Vec::with_capacity(1 + self.context.len() + self.turns.len() * 2);
        input.extend(self.context.iter().cloned());
        input.push(InputItem::external(
            InputSource::Named("parent-agent".to_owned()),
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

    pub(crate) fn record_result(
        &mut self,
        turn: usize,
        tool: usize,
        result: ExecutionToolResult,
    ) -> bool {
        let turn = self
            .turns
            .get_mut(turn)
            .expect("pending tool points to its originating turn");
        turn.record_result(tool, result);
        !turn.is_waiting()
    }

    pub(crate) fn complete(&mut self, output: String) {
        self.outcome = Some(ActionOutcome::Completed { output });
    }

    pub(crate) fn fail(&mut self, error: ActionError) {
        self.outcome = Some(ActionOutcome::Failed(error));
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
                    result: Some(ExecutionToolResult::skipped(reason)),
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
        self.tools.iter().any(|tool| tool.result.is_none())
    }

    pub(crate) fn record_result(&mut self, index: usize, result: ExecutionToolResult) {
        let execution = self
            .tools
            .get_mut(index)
            .expect("pending tool points to its originating turn");
        debug_assert!(execution.result.is_none(), "a tool result is recorded once");
        execution.result = Some(result);
    }

    fn result_items(&self) -> Vec<InputItem> {
        if self.tools.is_empty() || self.is_waiting() {
            return Vec::new();
        }

        self.tools
            .iter()
            .map(|execution| {
                let result = execution
                    .result
                    .as_ref()
                    .expect("a settled turn has every tool result");
                InputItem::tool_result(&execution.call, model_tool_output(result))
            })
            .collect()
    }
}

fn supported_model_tool_output(result: &ExecutionToolResult) -> Option<ModelToolOutput> {
    let output = result.output();
    if let Some(text) = output.as_text() {
        Some(ModelToolOutput::text(text))
    } else {
        output
            .as_json()
            .map(|value| ModelToolOutput::json(value.clone()))
    }
}

pub(crate) fn normalize_tool_result(result: ExecutionToolResult) -> ExecutionToolResult {
    if supported_model_tool_output(&result).is_some() {
        result
    } else {
        ExecutionToolResult::failed(rig_core::tool::ToolExecutionError::other(
            "tool output cannot be represented by this Agent; return one plain text or JSON value",
        ))
    }
}

pub(crate) fn model_tool_output(result: &ExecutionToolResult) -> ModelToolOutput {
    supported_model_tool_output(result)
        .expect("every stored tool result is normalized before entering an Action")
}

/// One tool call and its eventual execution result.
pub struct ToolExecution {
    call: ToolCall,
    result: Option<ExecutionToolResult>,
}

impl ToolExecution {
    fn new(call: ToolCall) -> Self {
        Self { call, result: None }
    }

    pub fn call(&self) -> &ToolCall {
        &self.call
    }

    /// The terminal execution result.
    pub fn result(&self) -> &ExecutionToolResult {
        self.result
            .as_ref()
            .expect("tool executions returned by Agent are settled")
    }
}
