use std::time::Duration;

use bone_llm::{
    FinishReason, InputItem, InputSource, Model, Request, Response, ToolCall, ToolDefinition,
    ToolOutput,
};
use serde::Deserialize;
use serde_json::json;
use tokio::time::timeout;

use crate::{
    Action, AgentConfigError, AgentError, Tool,
    runtime::{Limits, drive},
    tools::{START_ACTION_TOOL, Tools},
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

const MAX_DECISIONS: usize = 32;
const MAX_ACTIONS_PER_DECISION: usize = 8;

pub(crate) const PARENT_AGENT_SOURCE: &str = "parent-agent";

#[derive(Clone, Default)]
struct AgentHistory {
    input: Vec<InputItem>,
    action_context: Vec<InputItem>,
}

impl AgentHistory {
    fn push_user(&mut self, text: String) {
        self.input
            .push(InputItem::external(InputSource::User, text.clone()));
        self.action_context
            .push(InputItem::external(InputSource::User, text));
    }

    fn push_response(&mut self, response: &Response) {
        if let Some(item) = response.clone().into_item() {
            self.input.push(item);
        }
        if let Some(text) = parent_response_text(response) {
            self.action_context.push(InputItem::external(
                InputSource::Named(PARENT_AGENT_SOURCE.to_owned()),
                text,
            ));
        }
    }

    fn push_tool_result(&mut self, call: &ToolCall, output: &ToolOutput) {
        self.input
            .push(InputItem::tool_result(call, output.clone()));
        let summary = output
            .as_text()
            .map(str::to_owned)
            .or_else(|| output.as_json().map(ToString::to_string))
            .expect("agent control results are text or JSON");
        self.action_context.push(InputItem::external(
            InputSource::Named(PARENT_AGENT_SOURCE.to_owned()),
            format!("Tool `{}` returned: {summary}", call.name()),
        ));
    }
}

fn parent_response_text(response: &Response) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(text) = response.text() {
        parts.push(text);
    }
    parts.extend(response.tool_calls().map(|call| {
        format!(
            "Requested tool `{}` with arguments {}",
            call.name(),
            call.arguments()
        )
    }));
    (!parts.is_empty()).then(|| parts.join("\n"))
}

/// A conversational agent that chooses and advances its own actions.
pub struct Agent {
    model: Model,
    instructions: Option<String>,
    tools: Tools,
    history: AgentHistory,
    limits: Limits,
}

impl Agent {
    pub fn new(model: Model) -> Self {
        Self {
            model,
            instructions: None,
            tools: Tools::default(),
            history: AgentHistory::default(),
            limits: Limits::default(),
        }
    }

    /// Set instructions shared by the agent and every action it creates.
    pub fn instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    /// Register a typed tool for actions created by this agent.
    ///
    /// The tool future must not block its executor thread. It may be dropped
    /// on cancellation or timeout and must not leave effects or cleanup that
    /// can conflict with a later Turn.
    pub fn tool<T>(mut self, tool: T) -> Result<Self, AgentConfigError>
    where
        T: Tool + 'static,
    {
        self.tools.register(tool)?;
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
    Accepted { call: ToolCall, action: usize },
    Rejected { call: ToolCall, output: ToolOutput },
}

async fn drive_agent(
    model: &Model,
    instructions: Option<&str>,
    tools: &Tools,
    limits: Limits,
    history: &mut AgentHistory,
    input: String,
) -> Result<AgentReply, AgentError> {
    history.push_user(input);
    let agent_instructions = combined_instructions(instructions, AGENT_PROTOCOL);
    let action_instructions = combined_instructions(instructions, ACTION_PROTOCOL);
    let mut completed_actions = Vec::new();

    for _ in 0..MAX_DECISIONS {
        let response = complete_agent(
            model,
            &history.input,
            &agent_instructions,
            limits.model_timeout,
        )
        .await?;
        let finish_reason = response.finish_reason().cloned();

        if response.items().is_empty()
            || finish_reason
                .as_ref()
                .is_some_and(FinishReason::truncated_output)
        {
            return Err(AgentError::Incomplete { finish_reason });
        }

        let calls = response.tool_calls().cloned().collect::<Vec<_>>();
        if calls.is_empty() {
            let text = response
                .text()
                .ok_or_else(|| AgentError::Incomplete { finish_reason })?;
            history.push_response(&response);
            return Ok(AgentReply {
                text,
                actions: completed_actions,
            });
        }

        if calls.len() > MAX_ACTIONS_PER_DECISION {
            return Err(AgentError::ActionLimit {
                requested: calls.len(),
                limit: MAX_ACTIONS_PER_DECISION,
            });
        }
        let mut actions = Vec::new();
        let starts = calls
            .into_iter()
            .map(
                |call| match action_from_call(&call, &history.action_context) {
                    Ok(action) => {
                        let index = actions.len();
                        actions.push(action);
                        ActionStart::Accepted {
                            call,
                            action: index,
                        }
                    }
                    Err(output) => ActionStart::Rejected { call, output },
                },
            )
            .collect::<Vec<_>>();
        history.push_response(&response);

        let actions = drive(model, Some(&action_instructions), tools, limits, actions).await;
        for start in starts {
            let (call, output) = match start {
                ActionStart::Accepted { call, action } => (call, action_result(&actions[action])),
                ActionStart::Rejected { call, output } => (call, output),
            };
            history.push_tool_result(&call, &output);
        }
        completed_actions.extend(actions);
    }

    Err(AgentError::DecisionLimit {
        limit: MAX_DECISIONS,
    })
}

fn action_from_call(call: &ToolCall, context: &[InputItem]) -> Result<Action, ToolOutput> {
    if call.name() != START_ACTION_TOOL {
        return Err(ToolOutput::text(format!(
            "tool `{}` is not registered",
            call.name()
        )));
    }

    let request =
        serde_json::from_value::<StartAction>(call.arguments().clone()).map_err(|_| {
            ToolOutput::text(
                "start_action arguments must contain only a non-empty string field named `intent`",
            )
        })?;
    let intent = request.intent.trim();
    if intent.is_empty() {
        return Err(ToolOutput::text("action intent must not be empty"));
    }

    Ok(Action::with_context(intent, context.to_vec()))
}

fn action_result(action: &Action) -> ToolOutput {
    let value = match action.result() {
        Ok(output) => json!({ "status": "completed", "output": output }),
        Err(error) => json!({ "status": "failed", "error": error.to_string() }),
    };
    ToolOutput::json(value)
}

async fn complete_agent(
    model: &Model,
    history: &[InputItem],
    instructions: &str,
    deadline: Duration,
) -> Result<Response, AgentError> {
    let request = Request::new(history.iter().cloned())
        .instructions(instructions)
        .tools([start_action_definition()]);
    let completion = model.complete(request);
    match timeout(deadline, completion).await {
        Ok(response) => response.map_err(AgentError::Model),
        Err(_) => Err(AgentError::ModelTimeout { timeout: deadline }),
    }
}

fn start_action_definition() -> ToolDefinition {
    ToolDefinition::new(
        START_ACTION_TOOL,
        "Start one independent action. An action owns one or more model turns, may use tools or reason without them, and returns one outcome to the parent agent. Start multiple actions in one response only when they can run independently.",
        json!({
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
    )
}

fn combined_instructions(custom: Option<&str>, protocol: &str) -> String {
    match custom.filter(|text| !text.trim().is_empty()) {
        Some(custom) => format!("{custom}\n\n{protocol}"),
        None => protocol.to_owned(),
    }
}

#[cfg(test)]
mod tests;
