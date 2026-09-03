use std::time::Duration;

use bone_model::{
    Model,
    rig::{
        completion::{
            AssistantContent, CompletionModel, CompletionResponse, FinishReason, ToolDefinition,
        },
        message::{Message, ToolCall, UserContent},
        tool::{
            PortableDynamicTool, PortableTool, ToolExecutionError, ToolOutput,
            ToolResult as ExecutionToolResult,
        },
        wasm_compat::timeout,
    },
};
use serde::Deserialize;
use serde_json::json;

use crate::{
    Action, ActionOutcome, AgentConfigError, AgentError,
    runtime::{Limits, drive, final_text, unique_call_ids},
    tools::{START_ACTION_TOOL, Tools, missing_tool},
};

const AGENT_PROTOCOL: &str = "\
You decide how to respond to the user. For any work that benefits from isolated \
reasoning or tools, call start_action with a concise, self-contained intent. You \
may start several independent actions in one response. Actions advance independently, \
and their work may overlap while tools are pending. Completed action outcomes will \
return as tool results. Start further actions when needed. Give the user a final answer \
only when you have enough information. Never claim that an action or tool succeeded \
before its result is returned.";

const ACTION_PROTOCOL: &str = "\
You are carrying out one action selected by the parent agent. Complete only that \
action. Use the available tools when useful; an action may also be pure reasoning. \
Tool failures are observations you may recover from. When the action is complete, \
return a concise result for the parent agent. Do not address the user directly and \
do not create further actions.";

/// A conversational agent that chooses and advances its own actions.
pub struct Agent {
    model: Model,
    instructions: Option<String>,
    tools: Tools,
    history: Vec<Message>,
    limits: Limits,
}

impl Agent {
    pub fn new(model: Model) -> Self {
        Self {
            model,
            instructions: None,
            tools: Tools::default(),
            history: Vec::new(),
            limits: Limits::default(),
        }
    }

    /// Set instructions shared by the agent and every action it creates.
    pub fn instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    /// Register a typed Rig portable tool.
    ///
    /// The tool future must not block its executor thread. It may be dropped
    /// on cancellation or timeout and must not leave effects or cleanup that
    /// can conflict with a later Turn.
    pub fn tool<T>(mut self, tool: T) -> Result<Self, AgentConfigError>
    where
        T: PortableTool + 'static,
        T::Args: 'static,
        T::Output: 'static,
    {
        self.tools.register(tool)?;
        Ok(self)
    }

    /// Register an already type-erased Rig portable tool.
    ///
    /// The tool future must not block its executor thread. It may be dropped
    /// on cancellation or timeout and must not leave effects or cleanup that
    /// can conflict with a later Turn.
    pub fn dynamic_tool(mut self, tool: PortableDynamicTool) -> Result<Self, AgentConfigError> {
        self.tools.register_dynamic(tool)?;
        Ok(self)
    }

    /// Change the per-action loop safety bound.
    pub fn max_turns(mut self, max_turns: usize) -> Result<Self, AgentConfigError> {
        if max_turns == 0 {
            return Err(AgentConfigError::ZeroTurnLimit);
        }
        self.limits.max_turns = max_turns;
        Ok(self)
    }

    /// Change the maximum number of tool calls accepted from one model turn.
    pub fn max_tool_calls_per_turn(
        mut self,
        max_tool_calls_per_turn: usize,
    ) -> Result<Self, AgentConfigError> {
        if max_tool_calls_per_turn == 0 {
            return Err(AgentConfigError::ZeroToolCallLimit);
        }
        self.limits.max_tool_calls_per_turn = max_tool_calls_per_turn;
        Ok(self)
    }

    /// Change the runtime timeout for one model request.
    pub fn model_timeout(mut self, model_timeout: Duration) -> Result<Self, AgentConfigError> {
        if model_timeout.is_zero() {
            return Err(AgentConfigError::ZeroModelTimeout);
        }
        self.limits.model_timeout = model_timeout;
        Ok(self)
    }

    /// Change the runtime timeout for one tool execution.
    pub fn tool_timeout(mut self, tool_timeout: Duration) -> Result<Self, AgentConfigError> {
        if tool_timeout.is_zero() {
            return Err(AgentConfigError::ZeroToolTimeout);
        }
        self.limits.tool_timeout = tool_timeout;
        Ok(self)
    }

    /// Respond to one user message, creating actions whenever the model needs
    /// independent reasoning or tool-backed work.
    ///
    /// History is committed only after a final reply is produced. Dropping the
    /// returned future cancels the in-progress response and leaves the prior
    /// history unchanged. External effects already performed by tools cannot
    /// be rolled back by this history rule.
    pub async fn chat(&mut self, input: impl Into<String>) -> Result<AgentReply, AgentError> {
        let mut history = self.history.clone();
        let reply = drive_agent(
            &self.model,
            self.instructions.as_deref(),
            &self.tools,
            self.limits,
            &mut history,
            input.into(),
        )
        .await?;
        self.history = history;
        Ok(reply)
    }
}

/// One final user-facing reply and the actions that informed it.
pub struct AgentReply {
    text: String,
    actions: Vec<Action>,
}

impl AgentReply {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn actions(&self) -> &[Action] {
        &self.actions
    }

    pub fn into_parts(self) -> (String, Vec<Action>) {
        (self.text, self.actions)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartAction {
    intent: String,
}

enum ActionStart {
    Accepted {
        call: ToolCall,
        action: usize,
    },
    Rejected {
        call: ToolCall,
        result: ExecutionToolResult,
    },
}

async fn drive_agent<M>(
    model: &M,
    instructions: Option<&str>,
    tools: &Tools,
    limits: Limits,
    history: &mut Vec<Message>,
    input: String,
) -> Result<AgentReply, AgentError>
where
    M: CompletionModel + Clone,
{
    history.push(Message::user(input));
    let agent_instructions = combined_instructions(instructions, AGENT_PROTOCOL);
    let action_instructions = combined_instructions(instructions, ACTION_PROTOCOL);
    let mut completed_actions = Vec::new();

    for _ in 0..limits.max_decisions {
        let action_context = history.clone();
        let response =
            complete_agent(model, history, &agent_instructions, limits.model_timeout).await?;
        let finish_reason = response.finish_reason();

        if response.choice.is_empty()
            || finish_reason
                .as_ref()
                .is_some_and(FinishReason::truncated_output)
        {
            return Err(AgentError::Incomplete { finish_reason });
        }

        let calls = tool_calls(&response);
        if calls.is_empty() {
            let text =
                final_text(&response).ok_or_else(|| AgentError::Incomplete { finish_reason })?;
            history.push(assistant_message(&response));
            return Ok(AgentReply {
                text,
                actions: completed_actions,
            });
        }

        if calls.len() > limits.max_actions_per_decision {
            return Err(AgentError::ActionLimit {
                requested: calls.len(),
                limit: limits.max_actions_per_decision,
            });
        }
        if !unique_call_ids(history_tool_calls(history).chain(calls.iter())) {
            return Err(AgentError::DuplicateToolCall);
        }

        history.push(assistant_message(&response));
        let mut actions = Vec::new();
        let starts = calls
            .into_iter()
            .map(|call| match action_from_call(&call, &action_context) {
                Ok(action) => {
                    let index = actions.len();
                    actions.push(action);
                    ActionStart::Accepted {
                        call,
                        action: index,
                    }
                }
                Err(result) => ActionStart::Rejected { call, result },
            })
            .collect::<Vec<_>>();

        let actions = drive(model, Some(&action_instructions), tools, limits, actions).await;
        let results = starts
            .into_iter()
            .map(|start| match start {
                ActionStart::Accepted { call, action } => {
                    action_result_message(call, action_result(&actions[action]))
                }
                ActionStart::Rejected { call, result } => action_result_message(call, result),
            })
            .collect();
        history.push(Message::User { content: results });
        completed_actions.extend(actions);
    }

    Err(AgentError::DecisionLimit {
        limit: limits.max_decisions,
    })
}

fn action_from_call(call: &ToolCall, context: &[Message]) -> Result<Action, ExecutionToolResult> {
    if call.function.name != START_ACTION_TOOL {
        return Err(missing_tool(&call.function.name));
    }

    let request =
        serde_json::from_value::<StartAction>(call.function.arguments.clone()).map_err(|_| {
            ExecutionToolResult::failed(ToolExecutionError::invalid_args(
                "start_action arguments must contain only a non-empty string field named `intent`",
            ))
        })?;
    let intent = request.intent.trim();
    if intent.is_empty() {
        return Err(ExecutionToolResult::failed(
            ToolExecutionError::invalid_args("action intent must not be empty"),
        ));
    }

    Ok(Action::with_context(intent, context.to_vec()))
}

fn action_result(action: &Action) -> ExecutionToolResult {
    let value = match action.outcome() {
        ActionOutcome::Completed { output } => {
            json!({ "status": "completed", "output": output })
        }
        ActionOutcome::Failed(error) => {
            json!({ "status": "failed", "error": error.to_string() })
        }
    };
    ExecutionToolResult::success(ToolOutput::json(value))
}

fn action_result_message(call: ToolCall, result: ExecutionToolResult) -> UserContent {
    UserContent::tool_result_for(
        call.id,
        call.provider,
        call.function.name,
        result.output().clone().into_content(),
    )
}

async fn complete_agent<M>(
    model: &M,
    history: &[Message],
    instructions: &str,
    deadline: Duration,
) -> Result<CompletionResponse, AgentError>
where
    M: CompletionModel + Clone,
{
    let mut messages = Vec::with_capacity(history.len() + 1);
    messages.push(Message::system(instructions));
    messages.extend(history.iter().cloned());
    let prompt = messages
        .pop()
        .expect("chat history always contains the current input");
    let completion = model
        .completion_request(prompt)
        .messages(messages)
        .tools(vec![start_action_definition()])
        .send();
    match timeout(deadline, completion).await {
        Ok(response) => response.map_err(AgentError::Model),
        Err(_) => Err(AgentError::ModelTimeout { timeout: deadline }),
    }
}

fn start_action_definition() -> ToolDefinition {
    ToolDefinition {
        name: START_ACTION_TOOL.to_owned(),
        description: "Start one independent action. An action owns one or more model turns, may use tools or reason without them, and returns one outcome to the parent agent. Start multiple actions in one response only when they can run independently."
            .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {
                "intent": {
                    "type": "string",
                    "description": "A concise, self-contained description of the result this action should produce"
                }
            },
            "required": ["intent"],
            "additionalProperties": false
        }),
    }
}

fn combined_instructions(custom: Option<&str>, protocol: &str) -> String {
    match custom.filter(|text| !text.trim().is_empty()) {
        Some(custom) => format!("{custom}\n\n{protocol}"),
        None => protocol.to_owned(),
    }
}

fn assistant_message(response: &CompletionResponse) -> Message {
    Message::Assistant {
        id: response.message_id.clone(),
        content: response.choice.clone(),
    }
}

fn tool_calls(response: &CompletionResponse) -> Vec<ToolCall> {
    response
        .choice
        .iter()
        .filter_map(|content| match content {
            AssistantContent::ToolCall(call) => Some(call.clone()),
            _ => None,
        })
        .collect()
}

fn history_tool_calls(history: &[Message]) -> impl Iterator<Item = &ToolCall> {
    history.iter().flat_map(|message| {
        let content = match message {
            Message::Assistant { content, .. } => content.as_slice(),
            Message::System { .. } | Message::User { .. } => &[],
        };
        content.iter().filter_map(|content| match content {
            AssistantContent::ToolCall(call) => Some(call),
            _ => None,
        })
    })
}

#[cfg(test)]
mod tests;
