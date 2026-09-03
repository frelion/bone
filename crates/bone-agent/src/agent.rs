use std::{collections::VecDeque, time::Duration};

use bone_provider::{
    Model,
    rig::{
        completion::{AssistantContent, CompletionModel, CompletionResponse, FinishReason},
        message::ToolCall,
        tool::{PortableDynamicTool, PortableTool, ToolResult as ExecutionToolResult},
        wasm_compat::{WasmBoxedFuture, timeout},
    },
};
use futures_util::{FutureExt, StreamExt, stream::FuturesUnordered};

use crate::{
    Action, ActionError, ActionState, AgentConfigError, Turn,
    tools::{Tools, missing_tool},
};

const DEFAULT_MAX_TURNS: usize = 32;
const DEFAULT_MAX_TOOL_CALLS_PER_TURN: usize = 16;
const DEFAULT_MODEL_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Copy)]
struct Limits {
    max_turns: usize,
    max_tool_calls_per_turn: usize,
    model_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_turns: DEFAULT_MAX_TURNS,
            max_tool_calls_per_turn: DEFAULT_MAX_TOOL_CALLS_PER_TURN,
            model_timeout: DEFAULT_MODEL_TIMEOUT,
        }
    }
}

/// A model and a small set of tools that advance actions.
pub struct Agent {
    model: Model,
    instructions: Option<String>,
    tools: Tools,
    limits: Limits,
}

impl Agent {
    pub fn new(model: Model) -> Self {
        Self {
            model,
            instructions: None,
            tools: Tools::default(),
            limits: Limits::default(),
        }
    }

    /// Set the system instructions shared by every action.
    pub fn instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    /// Register a typed Rig portable tool.
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

    /// Change the hard deadline for one model request.
    pub fn model_timeout(mut self, model_timeout: Duration) -> Result<Self, AgentConfigError> {
        if model_timeout.is_zero() {
            return Err(AgentConfigError::ZeroModelTimeout);
        }
        self.limits.model_timeout = model_timeout;
        Ok(self)
    }

    /// Advance one action until it completes or fails.
    ///
    /// The returned action always retains its complete trace. Inspect
    /// [`Action::outcome`] or the convenience [`Action::output`] and
    /// [`Action::error`] methods.
    pub async fn act(&self, intent: impl Into<String>) -> Action {
        self.run([Action::new(intent)])
            .await
            .pop()
            .expect("running one intent produces one action")
    }

    /// Run independent actions until each has completed or failed.
    ///
    /// Results preserve input order. A waiting action does not block later
    /// ready actions, and one action's failure does not stop the others.
    pub async fn run<I>(&self, actions: I) -> Vec<Action>
    where
        I: IntoIterator<Item = Action>,
    {
        drive(
            &self.model,
            self.instructions.as_deref(),
            &self.tools,
            self.limits,
            actions.into_iter().collect(),
        )
        .await
    }
}

struct FinishedTool {
    action: usize,
    turn: usize,
    tool: usize,
    result: ExecutionToolResult,
}

async fn drive<M>(
    model: &M,
    instructions: Option<&str>,
    tools: &Tools,
    limits: Limits,
    mut actions: Vec<Action>,
) -> Vec<Action>
where
    M: CompletionModel + Clone,
{
    let mut ready = (0..actions.len()).collect::<VecDeque<_>>();
    let mut running = FuturesUnordered::<WasmBoxedFuture<'static, FinishedTool>>::new();

    loop {
        while let Some(action_index) = ready.pop_front() {
            if actions[action_index].state() != ActionState::Ready {
                continue;
            }
            if actions[action_index].turns().len() >= limits.max_turns {
                actions[action_index].fail(ActionError::TurnLimit {
                    limit: limits.max_turns,
                });
                continue;
            }

            let messages = actions[action_index].messages(instructions);
            let definitions = tools.definitions();
            let completion = complete(model, messages, definitions, limits.model_timeout);
            tokio::pin!(completion);

            let response = loop {
                if running.is_empty() {
                    break completion.await;
                }

                tokio::select! {
                    response = &mut completion => break response,
                    Some(finished) = running.next() => {
                        let finished_action = finished.action;
                        if record_result(&mut actions, finished) {
                            ready.push_back(finished_action);
                        }
                    }
                }
            };

            let response = match response {
                Ok(response) => response,
                Err(error) => {
                    actions[action_index].fail(error);
                    continue;
                }
            };

            let finish_reason = response.finish_reason();
            if response.choice.is_empty() {
                actions[action_index].push_turn(Turn::new(response, Vec::new()));
                actions[action_index].fail(ActionError::Incomplete { finish_reason });
                continue;
            }

            let calls = response
                .choice
                .iter()
                .filter_map(|content| match content {
                    AssistantContent::ToolCall(call) => Some(call.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>();

            if finish_reason
                .as_ref()
                .is_some_and(FinishReason::truncated_output)
            {
                // A truncated tool call may be syntactically valid but still
                // incomplete. Preserve the model response, but never execute
                // side effects from it.
                actions[action_index].push_turn(Turn::skipped(
                    response,
                    calls,
                    "model response was truncated; tool was not executed",
                ));
                actions[action_index].fail(ActionError::Incomplete { finish_reason });
                continue;
            }

            if calls.len() > limits.max_tool_calls_per_turn {
                let requested = calls.len();
                actions[action_index].push_turn(Turn::skipped(
                    response,
                    calls,
                    "tool batch exceeded the per-turn limit; tool was not executed",
                ));
                actions[action_index].fail(ActionError::ToolCallLimit {
                    requested,
                    limit: limits.max_tool_calls_per_turn,
                });
                continue;
            }

            if calls.is_empty() {
                let output = final_text(&response);
                actions[action_index].push_turn(Turn::new(response, calls));
                match output {
                    Some(output) => actions[action_index].complete(output),
                    None => {
                        actions[action_index].fail(ActionError::Incomplete { finish_reason });
                    }
                }
                continue;
            }

            let turn_index = actions[action_index].push_turn(Turn::new(response, calls.clone()));
            for (tool_index, call) in calls.into_iter().enumerate() {
                running.push(start_tool(
                    action_index,
                    turn_index,
                    tool_index,
                    call,
                    tools,
                ));
            }
            poll_ready_tools(&mut actions, &mut ready, &mut running);
        }

        let Some(finished) = running.next().await else {
            break;
        };
        let action_index = finished.action;
        if record_result(&mut actions, finished) {
            ready.push_back(action_index);
        }
    }

    actions
}

async fn complete<M>(
    model: &M,
    mut messages: Vec<bone_provider::rig::message::Message>,
    tools: Vec<bone_provider::rig::completion::ToolDefinition>,
    deadline: Duration,
) -> Result<CompletionResponse, ActionError>
where
    M: CompletionModel + Clone,
{
    let prompt = messages
        .pop()
        .expect("an action transcript always contains its intent");
    let completion = model
        .completion_request(prompt)
        .messages(messages)
        .tools(tools)
        .send();
    match timeout(deadline, completion).await {
        Ok(response) => response.map_err(ActionError::Model),
        Err(_) => Err(ActionError::ModelTimeout { timeout: deadline }),
    }
}

fn start_tool(
    action: usize,
    turn: usize,
    tool: usize,
    call: ToolCall,
    tools: &Tools,
) -> WasmBoxedFuture<'static, FinishedTool> {
    let registered = tools.get(&call.function.name);
    Box::pin(async move {
        let result = match registered {
            Some(registered) => match registered.execute(call.function.arguments).await {
                Ok(output) => ExecutionToolResult::success(output),
                Err(error) => ExecutionToolResult::failed(error),
            },
            None => missing_tool(&call.function.name),
        };
        FinishedTool {
            action,
            turn,
            tool,
            result,
        }
    })
}

fn poll_ready_tools(
    actions: &mut [Action],
    ready: &mut VecDeque<usize>,
    running: &mut FuturesUnordered<WasmBoxedFuture<'static, FinishedTool>>,
) {
    while let Some(Some(finished)) = running.next().now_or_never() {
        let action_index = finished.action;
        if record_result(actions, finished) {
            ready.push_back(action_index);
        }
    }
}

fn record_result(actions: &mut [Action], finished: FinishedTool) -> bool {
    actions[finished.action].record_result(finished.turn, finished.tool, finished.result)
}

fn final_text(response: &CompletionResponse) -> Option<String> {
    let parts = response
        .choice
        .iter()
        .filter_map(|content| match content {
            AssistantContent::Text(text) if !text.text.trim().is_empty() => {
                Some(text.text.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join("\n"))
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
    use crate::ActionOutcome;

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
