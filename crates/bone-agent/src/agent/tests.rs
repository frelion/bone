use std::{
    collections::HashSet,
    convert::Infallible,
    ops::Deref,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use bone_llm::{FinishReason, Model, Protocol, testing};
use rig_core::test_utils::{MockCompletionModel, MockTurn};
use rig_core::{
    completion::{
        AssistantContent, CompletionError, CompletionModel, CompletionRequest, CompletionResponse,
        FinishReason as RigFinishReason, Usage,
    },
    message::{Message, ToolResultContent, UserContent},
    streaming::StreamingCompletionResponse,
    tool::{PortableDynamicTool, PortableTool, ToolErrorKind, ToolExecutionError, ToolOutput},
};
use tokio::sync::{Semaphore, mpsc};

use super::*;
use crate::{ActionError, ActionOutcome};

#[derive(Clone)]
struct ScriptedModel {
    inner: MockCompletionModel,
    model: Model,
}

impl ScriptedModel {
    fn new(turns: impl IntoIterator<Item = MockTurn>) -> Self {
        Self::from_inner(MockCompletionModel::new(turns))
    }

    fn text(text: impl Into<String>) -> Self {
        Self::from_inner(MockCompletionModel::text(text))
    }

    fn from_inner(inner: MockCompletionModel) -> Self {
        let model = testing::model(
            "bone-agent-test",
            Protocol::OpenAiResponses,
            "test-model",
            inner.clone(),
        )
        .expect("test model");
        Self { inner, model }
    }

    fn request_count(&self) -> usize {
        self.inner.request_count()
    }

    fn requests(&self) -> Vec<CompletionRequest> {
        self.inner.requests()
    }
}

impl Deref for ScriptedModel {
    type Target = Model;

    fn deref(&self) -> &Self::Target {
        &self.model
    }
}

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
    let model = ScriptedModel::new([
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
    let mut history = AgentHistory::default();

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
    assert!(action.turns()[0].tools()[0].result().is_success());

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
    let model = ScriptedModel::new([
        MockTurn::tool_call(
            "start-1",
            START_ACTION_TOOL,
            serde_json::json!({"intent": "compare the two designs"}),
        ),
        MockTurn::text("Design A has the smaller state surface."),
        MockTurn::text("Choose design A."),
    ]);
    let mut history = AgentHistory::default();

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
    let model = ScriptedModel::new([MockTurn::from_contents([
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
    let mut history = AgentHistory::default();

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

    assert!(matches!(
        error,
        AgentError::Model(error) if error.kind() == bone_llm::ErrorKind::Protocol
    ));
    assert_eq!(model.request_count(), 1);
}

#[tokio::test]
async fn direct_replies_create_no_action_and_extend_history() {
    let model = ScriptedModel::new([
        MockTurn::text("Hello."),
        MockTurn::text("Yes, I remember greeting you."),
    ]);
    let mut history = AgentHistory::default();

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
    assert_eq!(history.input.len(), 4);
    let requests = model.requests();
    assert_eq!(first_user_text(&requests[1].chat_history), "Hello");
    assert!(assistant_has_text(&requests[1].chat_history, "Hello."));
}

#[tokio::test]
async fn action_context_attributes_parent_output_as_external_input() {
    let model = ScriptedModel::new([
        MockTurn::text("A fact from the parent agent."),
        MockTurn::tool_call(
            "start-1",
            START_ACTION_TOOL,
            serde_json::json!({"intent": "use the earlier fact"}),
        ),
        MockTurn::text("The action used the fact."),
        MockTurn::text("Done."),
    ]);
    let mut history = AgentHistory::default();

    drive_agent(
        &model,
        None,
        &Tools::default(),
        Limits::default(),
        &mut history,
        "Remember this request.".to_owned(),
    )
    .await
    .expect("first reply");
    drive_agent(
        &model,
        None,
        &Tools::default(),
        Limits::default(),
        &mut history,
        "Use what you remember.".to_owned(),
    )
    .await
    .expect("action-backed reply");

    let requests = model.requests();
    let action_history = &requests[2].chat_history;
    assert!(
        action_history
            .iter()
            .all(|message| !matches!(message, Message::Assistant { .. })),
        "parent assistant state must not be replayed into an action"
    );
    assert!(action_history.iter().any(|message| {
        message.rag_text().as_deref()
            == Some(
                "<bone_external source=\"parent-agent\">\nA fact from the parent agent.\n</bone_external>",
            )
    }));
}

#[tokio::test]
async fn final_text_completes_action() {
    let model = ScriptedModel::text("done");
    let actions = drive(
        &model,
        Some("Keep it short."),
        &Tools::default(),
        Limits::default(),
        vec![Action::new("inspect the change")],
    )
    .await;

    assert_eq!(actions.len(), 1);
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
    let model = ScriptedModel::new([first, MockTurn::text("observed")]);
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
    assert!(actions[0].turns()[0].tools()[0].result().is_success());

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
    let model = ScriptedModel::new([
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
    let model = ScriptedModel::new([
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
            .is_error_kind(ToolErrorKind::Timeout)
    );
}

#[tokio::test]
async fn one_turn_runs_tools_concurrently_and_waits_for_the_whole_batch() {
    let model = ScriptedModel::new([
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
    let model = ScriptedModel::new([
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
    assert!(execution.result().is_error_kind(ToolErrorKind::NotFound));
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
async fn unsupported_rich_tool_output_becomes_an_explicit_failure() {
    let model = ScriptedModel::new([
        MockTurn::tool_call("call-1", "rich", serde_json::json!({})),
        MockTurn::text("recovered"),
    ]);
    let mut tools = Tools::default();
    tools
        .register_dynamic(PortableDynamicTool::new(
            "rich",
            "Return several content blocks",
            serde_json::json!({"type": "object"}),
            |_| {
                Box::pin(async {
                    Ok(ToolOutput::content(vec![
                        ToolResultContent::text("secret-first-block"),
                        ToolResultContent::text("secret-second-block"),
                    ])
                    .expect("fixture has content"))
                })
            },
        ))
        .expect("register rich tool");

    let actions = drive(
        &model,
        None,
        &tools,
        Limits::default(),
        vec![Action::new("try rich output")],
    )
    .await;

    assert_eq!(actions[0].output(), Some("recovered"));
    let result = actions[0].turns()[0].tools()[0].result();
    assert!(result.is_error_kind(ToolErrorKind::Other));
    let requests = model.requests();
    let visible = only_tool_result(&requests[1].chat_history[2]).content[0]
        .as_text()
        .expect("normalization failure is plain text");
    assert!(visible.contains("cannot be represented"));
    assert!(!visible.contains("secret-first-block"));
    assert!(!visible.contains("secret-second-block"));
}

#[tokio::test]
async fn a_failed_tool_still_waits_for_the_rest_of_its_batch() {
    let model = ScriptedModel::new([
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
            .is_error_kind(ToolErrorKind::NotFound)
    );
    assert!(executions[1].result().is_success());

    let requests = model.requests();
    let results = tool_results(&requests[1].chat_history[2]);
    assert_eq!(results[0].name, "missing");
    assert_eq!(results[1].name, "slow");
}

#[tokio::test]
async fn one_failed_action_does_not_end_the_agent_run() {
    let model = ScriptedModel::new([
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
        ActionOutcome::Failed(ActionError::Model(_))
    ));
    assert_eq!(actions[1].output(), Some("B done"));
    assert_eq!(model.request_count(), 2);
}

#[tokio::test]
async fn turn_limit_stops_an_unbounded_tool_loop() {
    let model = ScriptedModel::new([MockTurn::tool_call("call-1", "echo", serde_json::json!({}))]);
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
    let model = ScriptedModel::new([MockTurn::from_contents([
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
            .all(|tool| tool.result().is_skipped())
    );
}

#[tokio::test]
async fn duplicate_tool_call_ids_start_none_of_them() {
    let model = ScriptedModel::new([MockTurn::from_contents([
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
        Some(ActionError::Model(error)) if error.kind() == bone_llm::ErrorKind::Protocol
    ));
    assert!(actions[0].turns().is_empty());
}

#[tokio::test]
async fn model_timeout_does_not_starve_the_next_action() {
    let calls = Arc::new(AtomicUsize::new(0));
    let inner = FirstCallHangs {
        calls: Arc::clone(&calls),
    };
    let model = testing::model(
        "bone-agent-timeout-test",
        Protocol::OpenAiResponses,
        "test-model",
        inner,
    )
    .expect("test model");
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
    let model = ScriptedModel::new([
        MockTurn::from_content(AssistantContent::reasoning("still thinking")),
        MockTurn::text("partial").with_finish_reason(RigFinishReason::Length),
        MockTurn::tool_call("call-1", "must-not-run", serde_json::json!({}))
            .with_finish_reason(RigFinishReason::Length),
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
    assert!(actions[2].turns()[0].tools()[0].result().is_skipped());
    assert!(matches!(
        actions[3].error(),
        Some(ActionError::Incomplete {
            finish_reason: None
        })
    ));
    assert_eq!(actions[3].turns().len(), 1);
    assert!(actions[3].turns()[0].response().items().is_empty());
}

#[tokio::test]
async fn dropping_the_run_drops_every_pending_tool() {
    let model = ScriptedModel::new([MockTurn::from_contents([
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
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, CompletionError> {
        if self.calls.fetch_add(1, Ordering::Relaxed) == 0 {
            std::future::pending::<()>().await;
        }
        Ok(CompletionResponse::new(
            vec![AssistantContent::text("B done")],
            Usage::new(),
            "test",
        ))
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse, CompletionError> {
        Err(CompletionError::ProviderError(
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
    let text = messages
        .iter()
        .find_map(Message::rag_text)
        .expect("request contains its action intent");
    text.strip_prefix("<bone_external source=\"parent-agent\">\n")
        .and_then(|text| text.strip_suffix("\n</bone_external>"))
        .unwrap_or(&text)
        .to_owned()
}

fn only_tool_result(message: &Message) -> &rig_core::message::ToolResult {
    let results = tool_results(message);
    assert_eq!(results.len(), 1);
    results[0]
}

fn find_tool_result<'a>(
    messages: &'a [Message],
    name: &str,
) -> Option<&'a rig_core::message::ToolResult> {
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

fn tool_results(message: &Message) -> Vec<&rig_core::message::ToolResult> {
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
