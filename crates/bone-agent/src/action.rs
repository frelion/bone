use bone_provider::rig::{
    completion::{AssistantContent, CompletionResponse},
    message::{Message, ToolCall, UserContent},
    tool::ToolResult as ExecutionToolResult,
};

use crate::ActionError;

/// Whether an action can advance, is waiting for tools, or has ended.
///
/// This value is derived from the action's turns and outcome; it is never
/// stored as a second source of truth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionState {
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

/// One independently resumable piece of work.
///
/// Each action owns its model transcript. This keeps another action's messages
/// from being inserted between a tool call and its result.
pub struct Action {
    intent: String,
    context: Vec<Message>,
    turns: Vec<Turn>,
    outcome: Option<ActionOutcome>,
}

impl Action {
    #[cfg(test)]
    pub(crate) fn new(intent: impl Into<String>) -> Self {
        Self::with_context(intent, Vec::new())
    }

    pub(crate) fn with_context(intent: impl Into<String>, context: Vec<Message>) -> Self {
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

    pub fn outcome(&self) -> Option<&ActionOutcome> {
        self.outcome.as_ref()
    }

    pub fn output(&self) -> Option<&str> {
        self.outcome.as_ref().and_then(ActionOutcome::output)
    }

    pub fn error(&self) -> Option<&ActionError> {
        self.outcome.as_ref().and_then(ActionOutcome::error)
    }

    pub fn state(&self) -> ActionState {
        if self.outcome.is_some() {
            ActionState::Finished
        } else if self.turns.last().is_some_and(Turn::is_waiting) {
            ActionState::Waiting
        } else {
            ActionState::Ready
        }
    }

    pub(crate) fn messages(&self, instructions: Option<&str>) -> Vec<Message> {
        let mut messages = Vec::with_capacity(2 + self.context.len() + self.turns.len() * 2);
        if let Some(instructions) = instructions.filter(|text| !text.trim().is_empty()) {
            messages.push(Message::system(instructions));
        }
        messages.extend(self.context.iter().cloned());
        messages.push(Message::user(self.intent.clone()));

        for turn in &self.turns {
            if !turn.response.choice.is_empty() {
                messages.push(Message::Assistant {
                    id: turn.response.message_id.clone(),
                    content: turn.response.choice.clone(),
                });
            }

            if let Some(results) = turn.result_message() {
                messages.push(results);
            }
        }

        messages
    }

    pub(crate) fn tool_calls(&self) -> impl Iterator<Item = &ToolCall> {
        self.context.iter().flat_map(message_tool_calls).chain(
            self.turns
                .iter()
                .flat_map(|turn| turn.tools.iter().map(ToolExecution::call)),
        )
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

fn message_tool_calls(message: &Message) -> impl Iterator<Item = &ToolCall> {
    let content = match message {
        Message::Assistant { content, .. } => content.as_slice(),
        Message::System { .. } | Message::User { .. } => &[],
    };
    content.iter().filter_map(|content| match content {
        AssistantContent::ToolCall(call) => Some(call),
        _ => None,
    })
}

/// One model decision and the tool calls produced by that decision.
pub struct Turn {
    response: CompletionResponse,
    tools: Vec<ToolExecution>,
}

impl Turn {
    pub(crate) fn new(response: CompletionResponse, calls: Vec<ToolCall>) -> Self {
        Self {
            response,
            tools: calls.into_iter().map(ToolExecution::new).collect(),
        }
    }

    pub(crate) fn skipped(
        response: CompletionResponse,
        calls: Vec<ToolCall>,
        reason: &'static str,
    ) -> Self {
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

    pub fn response(&self) -> &CompletionResponse {
        &self.response
    }

    pub fn assistant(&self) -> &[AssistantContent] {
        &self.response.choice
    }

    pub fn tools(&self) -> &[ToolExecution] {
        &self.tools
    }

    pub fn is_waiting(&self) -> bool {
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

    fn result_message(&self) -> Option<Message> {
        if self.tools.is_empty() || self.is_waiting() {
            return None;
        }

        let content = self
            .tools
            .iter()
            .map(|execution| {
                let result = execution
                    .result
                    .as_ref()
                    .expect("a settled turn has every tool result");
                UserContent::tool_result_for(
                    execution.call.id.clone(),
                    execution.call.provider.clone(),
                    execution.call.function.name.clone(),
                    result.output().clone().into_content(),
                )
            })
            .collect();

        Some(Message::User { content })
    }
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

    pub fn result(&self) -> Option<&ExecutionToolResult> {
        self.result.as_ref()
    }
}
