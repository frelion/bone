use std::time::Duration;

use bone_provider::{
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
    runtime::{drive, final_text, unique_call_ids},
    tools::{Tools, missing_tool},
};

pub(crate) const START_ACTION_TOOL: &str = "start_action";
const DEFAULT_MAX_DECISIONS: usize = 32;
const DEFAULT_MAX_ACTIONS_PER_DECISION: usize = 8;
const DEFAULT_MAX_TURNS: usize = 32;
const DEFAULT_MAX_TOOL_CALLS_PER_TURN: usize = 16;
const DEFAULT_MODEL_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(900);

const AGENT_PROTOCOL: &str = "\
You decide how to respond to the user. For any work that benefits from isolated \
reasoning or tools, call start_action with a concise, self-contained intent. You \
may start several independent actions in one response; they run concurrently. \
Completed action outcomes will return as tool results. Start further actions when \
needed. Give the user a final answer only when you have enough information. Never \
claim that an action or tool succeeded before its result is returned.";

const ACTION_PROTOCOL: &str = "\
You are carrying out one action selected by the parent agent. Complete only that \
action. Use the available tools when useful; an action may also be pure reasoning. \
Tool failures are observations you may recover from. When the action is complete, \
return a concise result for the parent agent. Do not address the user directly and \
do not create further actions.";

#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub(crate) max_decisions: usize,
    pub(crate) max_actions_per_decision: usize,
    pub(crate) max_turns: usize,
    pub(crate) max_tool_calls_per_turn: usize,
    pub(crate) model_timeout: Duration,
    pub(crate) tool_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_decisions: DEFAULT_MAX_DECISIONS,
            max_actions_per_decision: DEFAULT_MAX_ACTIONS_PER_DECISION,
            max_turns: DEFAULT_MAX_TURNS,
            max_tool_calls_per_turn: DEFAULT_MAX_TOOL_CALLS_PER_TURN,
            model_timeout: DEFAULT_MODEL_TIMEOUT,
            tool_timeout: DEFAULT_TOOL_TIMEOUT,
        }
    }
}

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
        Some(ActionOutcome::Completed { output }) => {
            json!({ "status": "completed", "output": output })
        }
        Some(ActionOutcome::Failed(error)) => {
            json!({ "status": "failed", "error": error.to_string() })
        }
        None => unreachable!("the action driver returns only settled actions"),
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
mod tests {
    use std::{
        collections::HashSet,
        convert::Infallible,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use bone_provider::rig::{
        completion::FinishReason,
        message::{Message, UserContent},
        tool::{PortableDynamicTool, PortableTool, ToolErrorKind, ToolExecutionError, ToolOutput},
    };
    use rig_core::test_utils::{MockCompletionModel, MockTurn};
    use tokio::sync::{Semaphore, mpsc};

    use super::*;
    use crate::{ActionError, ActionOutcome, ActionState};

    struct Echo;

    impl PortableTool for Echo {
        const NAME: &'static str = "echo";
        type Args = serde_json::Value;
        type Output = serde_json::Value;
        type Error = Infallible;

        fn description(&self) -> String {
            "Echo JSON".to_owned()
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        async fn call(&self, arguments: Self::Args) -> Result<Self::Output, Self::Error> {
            Ok(arguments)
        }
    }

    #[tokio::test]
    async fn agent_creates_a_tool_using_action_before_it_replies() {
        let model = MockCompletionModel::new([
            MockTurn::tool_call(
                "start-1",
                START_ACTION_TOOL,
                serde_json::json!({"intent": "inspect the supplied value"}),
            ),
            MockTurn::tool_call("echo-1", "echo", serde_json::json!({"value": "hello"})),
            MockTurn::text("The inspected value is hello."),
            MockTurn::text("I inspected it; the value is hello."),
        ]);
        let mut tools = Tools::default();
        tools.register(Echo).expect("register echo");
        let mut history = Vec::new();

        let reply = drive_agent(
            &model,
            Some("Verify facts before answering."),
            &tools,
            Limits::default(),
            &mut history,
            "What is the supplied value?".to_owned(),
        )
        .await
        .expect("agent should reply");

        assert_eq!(reply.text(), "I inspected it; the value is hello.");
        assert_eq!(reply.actions().len(), 1);
        let action = &reply.actions()[0];
        assert_eq!(action.intent(), "inspect the supplied value");
        assert_eq!(action.turns().len(), 2);
        assert_eq!(action.output(), Some("The inspected value is hello."));
        assert!(
            action.turns()[0].tools()[0]
                .result()
                .is_some_and(ExecutionToolResult::is_success)
        );

        let requests = model.requests();
        assert_eq!(requests.len(), 4);
        assert_eq!(
            requests[0]
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            [START_ACTION_TOOL]
        );
        assert_eq!(
            requests[1]
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["echo"]
        );
        let action_result = find_tool_result(&requests[3].chat_history, START_ACTION_TOOL)
            .expect("parent agent receives the action outcome");
        assert_eq!(
            action_result.content[0].as_json(),
            Some(&serde_json::json!({
                "status": "completed",
                "output": "The inspected value is hello."
            }))
        );
    }

    #[tokio::test]
    async fn an_action_can_be_pure_reasoning() {
        let model = MockCompletionModel::new([
            MockTurn::tool_call(
                "start-1",
                START_ACTION_TOOL,
                serde_json::json!({"intent": "compare the two designs"}),
            ),
            MockTurn::text("Design A has the smaller state surface."),
            MockTurn::text("Choose design A."),
        ]);
        let mut history = Vec::new();

        let reply = drive_agent(
            &model,
            None,
            &Tools::default(),
            Limits::default(),
            &mut history,
            "Which design is cleaner?".to_owned(),
        )
        .await
        .expect("agent should reply");

        assert_eq!(reply.text(), "Choose design A.");
        assert_eq!(reply.actions().len(), 1);
        assert_eq!(reply.actions()[0].turns().len(), 1);
        assert!(reply.actions()[0].turns()[0].tools().is_empty());
        assert_eq!(
            reply.actions()[0].output(),
            Some("Design A has the smaller state surface.")
        );
    }

    #[tokio::test]
    async fn duplicate_action_call_ids_start_no_action() {
        let model = MockCompletionModel::new([MockTurn::from_contents([
            AssistantContent::tool_call(
                "duplicate",
                START_ACTION_TOOL,
                serde_json::json!({"intent": "first"}),
            ),
            AssistantContent::tool_call(
                "duplicate",
                START_ACTION_TOOL,
                serde_json::json!({"intent": "second"}),
            ),
        ])]);
        let mut history = Vec::new();

        let result = drive_agent(
            &model,
            None,
            &Tools::default(),
            Limits::default(),
            &mut history,
            "do both".to_owned(),
        )
        .await;
        let error = match result {
            Ok(_) => panic!("duplicate identifiers unexpectedly started actions"),
            Err(error) => error,
        };

        assert!(matches!(error, AgentError::DuplicateToolCall));
        assert_eq!(model.request_count(), 1);
    }

    #[tokio::test]
    async fn direct_replies_create_no_action_and_extend_history() {
        let model = MockCompletionModel::new([
            MockTurn::text("Hello."),
            MockTurn::text("Yes, I remember greeting you."),
        ]);
        let mut history = Vec::new();

        let first = drive_agent(
            &model,
            None,
            &Tools::default(),
            Limits::default(),
            &mut history,
            "Hello".to_owned(),
        )
        .await
        .expect("first reply");
        let second = drive_agent(
            &model,
            None,
            &Tools::default(),
            Limits::default(),
            &mut history,
            "Do you remember me?".to_owned(),
        )
        .await
        .expect("second reply");

        assert!(first.actions().is_empty());
        assert!(second.actions().is_empty());
        assert_eq!(history.len(), 4);
        let requests = model.requests();
        assert_eq!(first_user_text(&requests[1].chat_history), "Hello");
        assert!(assistant_has_text(&requests[1].chat_history, "Hello."));
    }

    #[tokio::test]
    async fn final_text_completes_action() {
        let model = MockCompletionModel::text("done");
        let actions = drive(
            &model,
            Some("Keep it short."),
            &Tools::default(),
            Limits::default(),
            vec![Action::new("inspect the change")],
        )
        .await;

        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].state(), ActionState::Finished);
        assert_eq!(actions[0].output(), Some("done"));
        assert_eq!(actions[0].turns().len(), 1);

        let requests = model.requests();
        assert_eq!(requests.len(), 1);
        assert!(matches!(
            requests[0].chat_history.as_slice(),
            [Message::System { content }, Message::User { .. }]
                if content == "Keep it short."
        ));
    }

    #[tokio::test]
    async fn tool_result_drives_the_next_turn_with_full_identity() {
        let first = MockTurn::from_content(AssistantContent::tool_call_with_call_id(
            "item-1",
            "call-1".to_owned(),
            "echo",
            serde_json::json!({"value": "hello"}),
        ))
        .with_message_id("message-1");
        let model = MockCompletionModel::new([first, MockTurn::text("observed")]);
        let mut tools = Tools::default();
        tools.register(Echo).expect("register echo");

        let actions = drive(
            &model,
            None,
            &tools,
            Limits::default(),
            vec![Action::new("echo a value")],
        )
        .await;

        assert_eq!(actions[0].output(), Some("observed"));
        assert_eq!(actions[0].turns().len(), 2);
        assert!(
            actions[0].turns()[0].tools()[0]
                .result()
                .is_some_and(|result| result.is_success())
        );

        let requests = model.requests();
        assert_eq!(requests.len(), 2);
        assert!(matches!(
            &requests[1].chat_history[1],
            Message::Assistant { id: Some(id), .. } if id == "message-1"
        ));
        let result = only_tool_result(&requests[1].chat_history[2]);
        assert_eq!(result.call, "call-1");
        assert_eq!(result.name, "echo");
        let provider = result.provider.as_ref().expect("provider identity");
        assert_eq!(provider.call_id, "call-1");
        assert_eq!(provider.item_id.as_deref(), Some("item-1"));
        assert_eq!(
            result.content[0].as_json(),
            Some(&serde_json::json!({"value": "hello"}))
        );
    }

    #[tokio::test]
    async fn a_waiting_action_does_not_block_the_next_action() {
        let model = MockCompletionModel::new([
            MockTurn::tool_call("call-a", "gate", serde_json::json!({})),
            MockTurn::text("B done"),
            MockTurn::text("A done"),
        ]);
        let release = Arc::new(Semaphore::new(0));
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let (finished_tx, _finished_rx) = mpsc::unbounded_channel();
        let mut tools = Tools::default();
        tools
            .register_dynamic(gate_tool(
                "gate",
                Arc::clone(&release),
                started_tx,
                finished_tx,
            ))
            .expect("register gate");

        let run_model = model.clone();
        let run = tokio::spawn(async move {
            drive(
                &run_model,
                None,
                &tools,
                Limits::default(),
                vec![Action::new("A"), Action::new("B")],
            )
            .await
        });

        assert_eq!(recv(&mut started_rx).await, "gate");
        wait_until(|| model.request_count() >= 2).await;
        assert!(!run.is_finished(), "A is still waiting for its tool");

        let requests = model.requests();
        assert_eq!(first_user_text(&requests[0].chat_history), "A");
        assert_eq!(first_user_text(&requests[1].chat_history), "B");

        release.add_permits(1);
        let actions = run.await.expect("agent run should not panic");
        assert_eq!(actions[0].output(), Some("A done"));
        assert_eq!(actions[1].output(), Some("B done"));
        assert_eq!(model.request_count(), 3);
    }

    #[tokio::test]
    async fn a_tool_timeout_is_an_observation_the_action_can_recover_from() {
        let model = MockCompletionModel::new([
            MockTurn::tool_call("call-1", "hang", serde_json::json!({})),
            MockTurn::text("recovered after timeout"),
        ]);
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let (dropped_tx, mut dropped_rx) = mpsc::unbounded_channel();
        let mut tools = Tools::default();
        tools
            .register_dynamic(hanging_tool("hang", started_tx, dropped_tx))
            .expect("register hanging tool");

        let actions = drive(
            &model,
            None,
            &tools,
            Limits {
                tool_timeout: Duration::from_millis(20),
                ..Limits::default()
            },
            vec![Action::new("recover from a hung tool")],
        )
        .await;

        assert_eq!(recv(&mut started_rx).await, "hang");
        assert_eq!(recv(&mut dropped_rx).await, "hang");
        assert_eq!(actions[0].output(), Some("recovered after timeout"));
        assert!(
            actions[0].turns()[0].tools()[0]
                .result()
                .is_some_and(|result| result.is_error_kind(ToolErrorKind::Timeout))
        );
    }

    #[tokio::test]
    async fn one_turn_runs_tools_concurrently_and_waits_for_the_whole_batch() {
        let model = MockCompletionModel::new([
            MockTurn::from_contents([
                AssistantContent::tool_call("call-a", "slow-a", serde_json::json!({})),
                AssistantContent::tool_call("call-b", "slow-b", serde_json::json!({})),
            ]),
            MockTurn::text("both done"),
        ]);
        let release_a = Arc::new(Semaphore::new(0));
        let release_b = Arc::new(Semaphore::new(0));
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let (finished_tx, mut finished_rx) = mpsc::unbounded_channel();
        let mut tools = Tools::default();
        tools
            .register_dynamic(gate_tool(
                "slow-a",
                Arc::clone(&release_a),
                started_tx.clone(),
                finished_tx.clone(),
            ))
            .expect("register slow-a");
        tools
            .register_dynamic(gate_tool(
                "slow-b",
                Arc::clone(&release_b),
                started_tx,
                finished_tx,
            ))
            .expect("register slow-b");

        let run_model = model.clone();
        let run = tokio::spawn(async move {
            drive(
                &run_model,
                None,
                &tools,
                Limits::default(),
                vec![Action::new("run both")],
            )
            .await
        });

        let started = HashSet::from([recv(&mut started_rx).await, recv(&mut started_rx).await]);
        assert_eq!(started, HashSet::from(["slow-a", "slow-b"]));

        release_b.add_permits(1);
        assert_eq!(recv(&mut finished_rx).await, "slow-b");
        tokio::task::yield_now().await;
        assert_eq!(
            model.request_count(),
            1,
            "one result must not start the next model turn"
        );

        release_a.add_permits(1);
        assert_eq!(recv(&mut finished_rx).await, "slow-a");
        let actions = run.await.expect("agent run should not panic");
        assert_eq!(actions[0].output(), Some("both done"));

        let requests = model.requests();
        let results = tool_results(&requests[1].chat_history[2]);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].name, "slow-a");
        assert_eq!(results[0].content[0].as_text(), Some("slow-a"));
        assert_eq!(results[1].name, "slow-b");
        assert_eq!(results[1].content[0].as_text(), Some("slow-b"));
    }

    #[tokio::test]
    async fn tool_failure_is_an_observation_the_model_can_recover_from() {
        let model = MockCompletionModel::new([
            MockTurn::tool_call("call-1", "missing", serde_json::json!({})),
            MockTurn::text("recovered"),
        ]);

        let actions = drive(
            &model,
            None,
            &Tools::default(),
            Limits::default(),
            vec![Action::new("try a tool")],
        )
        .await;

        assert_eq!(actions[0].output(), Some("recovered"));
        let execution = &actions[0].turns()[0].tools()[0];
        assert!(
            execution
                .result()
                .is_some_and(|result| { result.is_error_kind(ToolErrorKind::NotFound) })
        );
        assert_eq!(model.request_count(), 2);

        let requests = model.requests();
        let result = only_tool_result(&requests[1].chat_history[2]);
        assert_eq!(result.call, "call-1");
        assert_eq!(
            result.content[0].as_text(),
            Some("tool `missing` is not registered")
        );
    }

    #[tokio::test]
    async fn a_failed_tool_still_waits_for_the_rest_of_its_batch() {
        let model = MockCompletionModel::new([
            MockTurn::from_contents([
                AssistantContent::tool_call("call-a", "missing", serde_json::json!({})),
                AssistantContent::tool_call("call-b", "slow", serde_json::json!({})),
            ]),
            MockTurn::text("recovered from the batch"),
        ]);
        let release = Arc::new(Semaphore::new(0));
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let (finished_tx, _finished_rx) = mpsc::unbounded_channel();
        let mut tools = Tools::default();
        tools
            .register_dynamic(gate_tool(
                "slow",
                Arc::clone(&release),
                started_tx,
                finished_tx,
            ))
            .expect("register slow tool");

        let run_model = model.clone();
        let run = tokio::spawn(async move {
            drive(
                &run_model,
                None,
                &tools,
                Limits::default(),
                vec![Action::new("mixed batch")],
            )
            .await
        });

        assert_eq!(recv(&mut started_rx).await, "slow");
        tokio::task::yield_now().await;
        assert_eq!(model.request_count(), 1);
        assert!(!run.is_finished());

        release.add_permits(1);
        let actions = run.await.expect("agent run should not panic");
        assert_eq!(actions[0].output(), Some("recovered from the batch"));
        let executions = actions[0].turns()[0].tools();
        assert!(
            executions[0]
                .result()
                .is_some_and(|result| result.is_error_kind(ToolErrorKind::NotFound))
        );
        assert!(
            executions[1]
                .result()
                .is_some_and(|result| result.is_success())
        );

        let requests = model.requests();
        let results = tool_results(&requests[1].chat_history[2]);
        assert_eq!(results[0].name, "missing");
        assert_eq!(results[1].name, "slow");
    }

    #[tokio::test]
    async fn one_failed_action_does_not_end_the_agent_run() {
        let model = MockCompletionModel::new([
            MockTurn::error("provider unavailable"),
            MockTurn::text("B done"),
        ]);
        let actions = drive(
            &model,
            None,
            &Tools::default(),
            Limits::default(),
            vec![Action::new("A"), Action::new("B")],
        )
        .await;

        assert!(matches!(
            actions[0].outcome(),
            Some(ActionOutcome::Failed(ActionError::Model(_)))
        ));
        assert_eq!(actions[1].output(), Some("B done"));
        assert_eq!(model.request_count(), 2);
    }

    #[tokio::test]
    async fn turn_limit_stops_an_unbounded_tool_loop() {
        let model = MockCompletionModel::new([MockTurn::tool_call(
            "call-1",
            "echo",
            serde_json::json!({}),
        )]);
        let mut tools = Tools::default();
        tools.register(Echo).expect("register echo");

        let actions = drive(
            &model,
            None,
            &tools,
            Limits {
                max_turns: 1,
                ..Limits::default()
            },
            vec![Action::new("loop")],
        )
        .await;

        assert!(matches!(
            actions[0].error(),
            Some(ActionError::TurnLimit { limit: 1 })
        ));
        assert_eq!(actions[0].turns().len(), 1);
        assert_eq!(model.request_count(), 1);
    }

    #[tokio::test]
    async fn too_many_tool_calls_start_none_of_them() {
        let model = MockCompletionModel::new([MockTurn::from_contents([
            AssistantContent::tool_call("call-a", "count", serde_json::json!({})),
            AssistantContent::tool_call("call-b", "count", serde_json::json!({})),
        ])]);
        let calls = Arc::new(AtomicUsize::new(0));
        let tool_calls = Arc::clone(&calls);
        let mut tools = Tools::default();
        tools
            .register_dynamic(PortableDynamicTool::new(
                "count",
                "Count executions",
                serde_json::json!({"type": "object"}),
                move |_| {
                    let tool_calls = Arc::clone(&tool_calls);
                    Box::pin(async move {
                        tool_calls.fetch_add(1, Ordering::Relaxed);
                        Ok(ToolOutput::text("ran"))
                    })
                },
            ))
            .expect("register counter");

        let actions = drive(
            &model,
            None,
            &tools,
            Limits {
                max_tool_calls_per_turn: 1,
                ..Limits::default()
            },
            vec![Action::new("unsafe fan-out")],
        )
        .await;

        assert!(matches!(
            actions[0].error(),
            Some(ActionError::ToolCallLimit {
                requested: 2,
                limit: 1
            })
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(actions[0].turns()[0].tools().len(), 2);
        assert!(
            actions[0].turns()[0]
                .tools()
                .iter()
                .all(|tool| tool.result().is_some_and(|result| result.is_skipped()))
        );
    }

    #[tokio::test]
    async fn duplicate_tool_call_ids_start_none_of_them() {
        let model = MockCompletionModel::new([MockTurn::from_contents([
            AssistantContent::tool_call("duplicate", "count", serde_json::json!({})),
            AssistantContent::tool_call("duplicate", "count", serde_json::json!({})),
        ])]);
        let calls = Arc::new(AtomicUsize::new(0));
        let tool_calls = Arc::clone(&calls);
        let mut tools = Tools::default();
        tools
            .register_dynamic(PortableDynamicTool::new(
                "count",
                "Count executions",
                serde_json::json!({"type": "object"}),
                move |_| {
                    let tool_calls = Arc::clone(&tool_calls);
                    Box::pin(async move {
                        tool_calls.fetch_add(1, Ordering::Relaxed);
                        Ok(ToolOutput::text("ran"))
                    })
                },
            ))
            .expect("register counter");

        let actions = drive(
            &model,
            None,
            &tools,
            Limits::default(),
            vec![Action::new("reject ambiguous calls")],
        )
        .await;

        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert!(matches!(
            actions[0].error(),
            Some(ActionError::DuplicateToolCall)
        ));
        assert!(
            actions[0].turns()[0]
                .tools()
                .iter()
                .all(|tool| tool.result().is_some_and(|result| result.is_skipped()))
        );
    }

    #[tokio::test]
    async fn model_timeout_does_not_starve_the_next_action() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model = FirstCallHangs {
            calls: Arc::clone(&calls),
        };
        let actions = drive(
            &model,
            None,
            &Tools::default(),
            Limits {
                model_timeout: Duration::from_millis(20),
                ..Limits::default()
            },
            vec![Action::new("A"), Action::new("B")],
        )
        .await;

        assert!(matches!(
            actions[0].error(),
            Some(ActionError::ModelTimeout { .. })
        ));
        assert_eq!(actions[1].output(), Some("B done"));
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn answerless_or_truncated_responses_are_not_success() {
        let model = MockCompletionModel::new([
            MockTurn::from_content(AssistantContent::reasoning("still thinking")),
            MockTurn::text("partial").with_finish_reason(FinishReason::Length),
            MockTurn::tool_call("call-1", "must-not-run", serde_json::json!({}))
                .with_finish_reason(FinishReason::Length),
            MockTurn::from_contents([]),
        ]);
        let calls = Arc::new(AtomicUsize::new(0));
        let tool_calls = Arc::clone(&calls);
        let mut tools = Tools::default();
        tools
            .register_dynamic(PortableDynamicTool::new(
                "must-not-run",
                "A side effect used to test truncated calls",
                serde_json::json!({"type": "object"}),
                move |_| {
                    let tool_calls = Arc::clone(&tool_calls);
                    Box::pin(async move {
                        tool_calls.fetch_add(1, Ordering::Relaxed);
                        Ok(ToolOutput::text("ran"))
                    })
                },
            ))
            .expect("register side effect");
        let actions = drive(
            &model,
            None,
            &tools,
            Limits::default(),
            vec![
                Action::new("reasoning only"),
                Action::new("truncated text"),
                Action::new("truncated tool call"),
                Action::new("empty response"),
            ],
        )
        .await;

        assert!(matches!(
            actions[0].error(),
            Some(ActionError::Incomplete { .. })
        ));
        assert!(matches!(
            actions[1].error(),
            Some(ActionError::Incomplete {
                finish_reason: Some(FinishReason::Length)
            })
        ));
        assert!(matches!(
            actions[2].error(),
            Some(ActionError::Incomplete {
                finish_reason: Some(FinishReason::Length)
            })
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert!(
            actions[2].turns()[0].tools()[0]
                .result()
                .is_some_and(|result| result.is_skipped())
        );
        assert!(matches!(
            actions[3].error(),
            Some(ActionError::Incomplete {
                finish_reason: None
            })
        ));
        assert_eq!(actions[3].turns().len(), 1);
        assert!(actions[3].turns()[0].assistant().is_empty());
    }

    #[tokio::test]
    async fn dropping_the_run_drops_every_pending_tool() {
        let model = MockCompletionModel::new([MockTurn::from_contents([
            AssistantContent::tool_call("call-a", "hang-a", serde_json::json!({})),
            AssistantContent::tool_call("call-b", "hang-b", serde_json::json!({})),
        ])]);
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let (dropped_tx, mut dropped_rx) = mpsc::unbounded_channel();
        let mut tools = Tools::default();
        tools
            .register_dynamic(hanging_tool(
                "hang-a",
                started_tx.clone(),
                dropped_tx.clone(),
            ))
            .expect("register hang-a");
        tools
            .register_dynamic(hanging_tool("hang-b", started_tx, dropped_tx))
            .expect("register hang-b");

        let run = tokio::spawn(async move {
            drive(
                &model,
                None,
                &tools,
                Limits::default(),
                vec![Action::new("wait forever")],
            )
            .await
        });

        let started = HashSet::from([recv(&mut started_rx).await, recv(&mut started_rx).await]);
        assert_eq!(started, HashSet::from(["hang-a", "hang-b"]));

        run.abort();
        let join_error = match run.await {
            Ok(_) => panic!("cancelled run unexpectedly completed"),
            Err(error) => error,
        };
        assert!(join_error.is_cancelled());
        let dropped = HashSet::from([recv(&mut dropped_rx).await, recv(&mut dropped_rx).await]);
        assert_eq!(dropped, HashSet::from(["hang-a", "hang-b"]));
    }

    #[test]
    fn duplicate_tool_names_are_rejected() {
        let mut tools = Tools::default();
        tools.register(Echo).expect("first registration");
        assert_eq!(
            tools.register(Echo),
            Err(AgentConfigError::DuplicateTool("echo".to_owned()))
        );
    }

    #[test]
    fn start_action_is_reserved_for_the_agent_protocol() {
        let mut tools = Tools::default();
        let result = tools.register_dynamic(PortableDynamicTool::new(
            START_ACTION_TOOL,
            "Must never be exposed inside an Action",
            serde_json::json!({"type": "object"}),
            |_| Box::pin(async { Ok(ToolOutput::text("unreachable")) }),
        ));

        assert_eq!(result, Err(AgentConfigError::ReservedTool));
    }

    fn gate_tool(
        name: &'static str,
        release: Arc<Semaphore>,
        started: mpsc::UnboundedSender<&'static str>,
        finished: mpsc::UnboundedSender<&'static str>,
    ) -> PortableDynamicTool {
        PortableDynamicTool::new(
            name,
            "Wait until released by the test",
            serde_json::json!({"type": "object"}),
            move |_| {
                let release = Arc::clone(&release);
                let started = started.clone();
                let finished = finished.clone();
                Box::pin(async move {
                    started.send(name).expect("test receives start");
                    let permit = release.acquire_owned().await.map_err(|_| {
                        ToolExecutionError::cancelled("test gate closed before release")
                    })?;
                    permit.forget();
                    finished.send(name).expect("test receives finish");
                    Ok(ToolOutput::text(name))
                })
            },
        )
    }

    fn hanging_tool(
        name: &'static str,
        started: mpsc::UnboundedSender<&'static str>,
        dropped: mpsc::UnboundedSender<&'static str>,
    ) -> PortableDynamicTool {
        PortableDynamicTool::new(
            name,
            "Wait forever",
            serde_json::json!({"type": "object"}),
            move |_| {
                let started = started.clone();
                let dropped = dropped.clone();
                Box::pin(async move {
                    let _drop_probe = DropProbe { name, dropped };
                    started.send(name).expect("test receives start");
                    std::future::pending::<()>().await;
                    Ok(ToolOutput::text("unreachable"))
                })
            },
        )
    }

    struct DropProbe {
        name: &'static str,
        dropped: mpsc::UnboundedSender<&'static str>,
    }

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.dropped.send(self.name).expect("test receives drop");
        }
    }

    #[derive(Clone)]
    struct FirstCallHangs {
        calls: Arc<AtomicUsize>,
    }

    impl CompletionModel for FirstCallHangs {
        async fn completion(
            &self,
            _request: bone_provider::CompletionRequest,
        ) -> Result<CompletionResponse, bone_provider::CompletionError> {
            if self.calls.fetch_add(1, Ordering::Relaxed) == 0 {
                std::future::pending::<()>().await;
            }
            Ok(CompletionResponse::new(
                vec![AssistantContent::text("B done")],
                bone_provider::rig::completion::Usage::new(),
                "test",
            ))
        }

        async fn stream(
            &self,
            _request: bone_provider::CompletionRequest,
        ) -> Result<bone_provider::StreamingCompletionResponse, bone_provider::CompletionError>
        {
            Err(bone_provider::CompletionError::ProviderError(
                "streaming is not used in this test".to_owned(),
            ))
        }
    }

    async fn recv(receiver: &mut mpsc::UnboundedReceiver<&'static str>) -> &'static str {
        tokio::time::timeout(Duration::from_secs(2), receiver.recv())
            .await
            .expect("test event timed out")
            .expect("test event channel closed")
    }

    async fn wait_until(condition: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !condition() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("condition timed out");
    }

    fn first_user_text(messages: &[Message]) -> String {
        messages
            .iter()
            .find_map(Message::rag_text)
            .expect("request contains its action intent")
    }

    fn only_tool_result(message: &Message) -> &bone_provider::rig::message::ToolResult {
        let results = tool_results(message);
        assert_eq!(results.len(), 1);
        results[0]
    }

    fn find_tool_result<'a>(
        messages: &'a [Message],
        name: &str,
    ) -> Option<&'a bone_provider::rig::message::ToolResult> {
        messages.iter().find_map(|message| match message {
            Message::User { content } => content.iter().find_map(|content| match content {
                UserContent::ToolResult(result) if result.name == name => Some(result),
                _ => None,
            }),
            _ => None,
        })
    }

    fn assistant_has_text(messages: &[Message], expected: &str) -> bool {
        messages.iter().any(|message| match message {
            Message::Assistant { content, .. } => content.iter().any(
                |content| matches!(content, AssistantContent::Text(text) if text.text == expected),
            ),
            _ => false,
        })
    }

    fn tool_results(message: &Message) -> Vec<&bone_provider::rig::message::ToolResult> {
        match message {
            Message::User { content } => content
                .iter()
                .filter_map(|content| match content {
                    UserContent::ToolResult(result) => Some(result),
                    _ => None,
                })
                .collect(),
            _ => panic!("expected tool results in a user message"),
        }
    }
}
