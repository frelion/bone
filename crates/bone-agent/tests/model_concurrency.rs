//! Hold actual provider futures to verify role selection and isolated context.
mod support;
use bone_agent::*;
use bone_llm::{Model, Protocol, testing};
use futures_util::future::BoxFuture;
use rig_core::{
    completion::{
        AssistantContent, CompletionError, CompletionModel, CompletionRequest, CompletionResponse,
        Usage,
    },
    message::{Message, UserContent},
    streaming::StreamingCompletionResponse,
};
use serde_json::{Value, json};
use std::sync::Arc;
use support::{answer, create, input, notice, receive, update};
use tokio::sync::{mpsc, oneshot};

struct PendingCall {
    request: CompletionRequest,
    reply: oneshot::Sender<CompletionResponse>,
}
impl PendingCall {
    fn payload(&self) -> Value {
        let text = self
            .request
            .chat_history
            .iter()
            .find_map(|message| match message {
                Message::User { content } => content.iter().find_map(|part| match part {
                    UserContent::Text(text) => Some(text.text.as_str()),
                    _ => None,
                }),
                _ => None,
            })
            .unwrap();
        let (_, body) = text.split_once('\n').unwrap();
        let (body, _) = body.rsplit_once("\n</bone_external>").unwrap();
        serde_json::from_str(body).unwrap()
    }
    fn respond(self, name: &str, value: Value) {
        self.reply
            .send(CompletionResponse::new(
                vec![AssistantContent::tool_call_with_call_id(
                    "fc_result",
                    "call_result".into(),
                    name,
                    value,
                )],
                Usage::default(),
                "controlled",
            ))
            .unwrap();
    }
    fn work(self, proposal: WorkProposal) {
        self.respond("submit_work", serde_json::to_value(proposal).unwrap());
    }
    fn route(self, changes: Vec<JobChange>) {
        self.respond(
            "submit_kernel",
            serde_json::to_value(KernelDecision {
                changes,
                ..Default::default()
            })
            .unwrap(),
        );
    }
}

#[derive(Clone)]
struct ControlledProvider(mpsc::UnboundedSender<PendingCall>);
impl CompletionModel for ControlledProvider {
    async fn completion(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, CompletionError> {
        let (reply, response) = oneshot::channel();
        self.0.send(PendingCall { request, reply }).unwrap();
        response
            .await
            .map_err(|_| CompletionError::ProviderError("test response dropped".into()))
    }
    async fn stream(
        &self,
        _: CompletionRequest,
    ) -> Result<StreamingCompletionResponse, CompletionError> {
        unreachable!("one complete request per invocation")
    }
}
fn model(id: &str) -> (Model, mpsc::UnboundedReceiver<PendingCall>) {
    let (sender, receiver) = mpsc::unbounded_channel();
    (
        testing::model(
            "controlled",
            Protocol::OpenAiResponses,
            id,
            ControlledProvider(sender),
        )
        .unwrap(),
        receiver,
    )
}
fn spawn(adapter: Arc<ModelAdapter>, tools: Vec<Arc<dyn ToolPort>>) -> AgentHandle {
    Runtime::spawn(
        adapter,
        tools,
        KernelConfig::default(),
        RuntimeConfig::default(),
    )
    .unwrap()
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn shared_or_separate_models_keep_kernel_and_full_workers_independent() {
    for shared in [false, true] {
        let (kernel, mut routing) = model("kernel");
        let (solver, mut work) = model("solver");
        let solver = if shared { kernel.clone() } else { solver };
        let agent = spawn(Arc::new(ModelAdapter::new(kernel, solver)), vec![]);
        agent.post(input(1, "A original")).await.unwrap();
        let first = receive(&mut routing).await;
        assert_eq!(first.request.tools[0].name, "submit_kernel");
        first.route(vec![create("A", vec![InputId(1)])]);
        let first = if shared {
            receive(&mut routing).await
        } else {
            receive(&mut work).await
        };
        assert_eq!(first.request.tools[0].name, "submit_work");
        assert_eq!(first.payload()["messages"][0]["text"], "A original");
        first.work(WorkProposal {
            next: Next::Continue,
            ..Default::default()
        });
        let held = if shared {
            receive(&mut routing).await
        } else {
            receive(&mut work).await
        };
        agent
            .post(input(2, "B substantive original"))
            .await
            .unwrap();
        let route = receive(&mut routing).await;
        assert_eq!(route.request.tools.len(), 1);
        assert_eq!(route.request.tools[0].name, "submit_kernel");
        assert_eq!(
            route.payload()["inputs"][0]["text"],
            "B substantive original"
        );
        assert!(!held.reply.is_closed());
        route.route(vec![create("B", vec![InputId(2)])]);
        let b = if shared {
            receive(&mut routing).await
        } else {
            receive(&mut work).await
        };
        assert_eq!(b.request.tools[0].name, "submit_work");
        assert_eq!(b.payload()["messages"][0]["text"], "B substantive original");
        assert!(b.request.additional_params.is_none());
        let mut notices = agent.subscribe();
        b.work(answer("B solver answer"));
        notice(&mut notices, |notice| {
            matches!(notice, Notice::InputFinished { id: InputId(2), .. })
        })
        .await;
        assert!(!held.reply.is_closed());
        assert!(routing.try_recv().is_err());
        assert!(work.try_recv().is_err());
        agent.shutdown().await.unwrap();
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn changed_goal_reaches_worker_as_original_words_without_old_answer_publication() {
    let (kernel, mut routing) = model("kernel");
    let (solver, mut work) = model("solver");
    let agent = spawn(Arc::new(ModelAdapter::new(kernel, solver)), vec![]);
    agent.post(input(1, "A original")).await.unwrap();
    receive(&mut routing)
        .await
        .route(vec![create("A", vec![InputId(1)])]);
    let old = receive(&mut work).await;
    agent
        .post(input(2, "not A: use the exact B requirements"))
        .await
        .unwrap();
    let route = receive(&mut routing).await;
    old.work(answer("OBSOLETE"));
    route.route(vec![update(
        JobId(1),
        Some("B"),
        JobAction::Keep,
        vec![InputId(2)],
        true,
    )]);
    let replacement = receive(&mut work).await;
    assert!(
        replacement.payload()["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|input| input["text"] == "not A: use the exact B requirements")
    );
    let mut notices = agent.subscribe();
    replacement.work(answer("B delivered"));
    notice(&mut notices, |notice| {
        matches!(notice, Notice::InputFinished { id: InputId(2), .. })
    })
    .await;
    assert!(!agent.snapshot().await.unwrap().record.iter().any(|entry| matches!(&entry.kind, RecordKind::Notice(Notice::Reply { text, .. }) if text == "OBSOLETE")));
    assert!(
        routing.try_recv().is_err(),
        "worker answers do not require a further Kernel call"
    );
    agent.shutdown().await.unwrap();
}

struct LargeResultTool;
impl ToolPort for LargeResultTool {
    fn specification(&self) -> ToolSpec {
        ToolSpec {
            name: "source".into(),
            description: "TOOL_SCHEMA_SENTINEL".into(),
            parameters: json!({"type":"object"}),
            effect: ToolEffect::ReadOnly,
        }
    }
    fn run(&self, _: Value, _: CallContext) -> BoxFuture<'static, CallOutcome> {
        Box::pin(async {
            CallOutcome::artifact(json!({"content":"LARGE_FILE_RESULT_SENTINEL".repeat(10_000)}))
        })
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn kernel_context_is_small_and_excludes_tool_arguments_schemas_and_results() {
    let (kernel, mut routing) = model("kernel");
    let (solver, mut work) = model("solver");
    let agent = spawn(
        Arc::new(ModelAdapter::new(kernel, solver)),
        vec![Arc::new(LargeResultTool)],
    );
    agent.post(input(1, "inspect original")).await.unwrap();
    receive(&mut routing)
        .await
        .route(vec![create("inspect source", vec![InputId(1)])]);
    receive(&mut work).await.work(WorkProposal {
        note: "PUBLIC_NOTE_SENTINEL".into(),
        operation: Some(ToolCall::new(
            "source",
            json!({"path":"CALL_ARGUMENTS_SENTINEL"}),
        )),
        ..Default::default()
    });
    let held = receive(&mut work).await;
    assert!(
        held.payload()
            .to_string()
            .contains("LARGE_FILE_RESULT_SENTINEL")
    );
    agent.post(input(2, "status")).await.unwrap();
    let route = receive(&mut routing).await;
    let context = route.payload().to_string();
    assert!(context.contains("inspect source"));
    assert!(context.contains("PUBLIC_NOTE_SENTINEL"));
    for secret in [
        "LARGE_FILE_RESULT_SENTINEL",
        "TOOL_SCHEMA_SENTINEL",
        "CALL_ARGUMENTS_SENTINEL",
    ] {
        assert!(!context.contains(secret));
    }
    assert!(context.len() < 5000);
    agent.stop().await.unwrap();
    agent.shutdown().await.unwrap();
    assert!(held.reply.is_closed());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn unrelated_workers_do_not_receive_other_jobs_material_without_explicit_references() {
    let (kernel, mut routing) = model("kernel");
    let (solver, mut work) = model("solver");
    let agent = spawn(
        Arc::new(ModelAdapter::new(kernel, solver)),
        vec![Arc::new(LargeResultTool)],
    );
    agent.post(input(1, "A private")).await.unwrap();
    receive(&mut routing)
        .await
        .route(vec![create("A", vec![InputId(1)])]);
    receive(&mut work).await.work(WorkProposal {
        operation: Some(ToolCall::new("source", json!({}))),
        ..Default::default()
    });
    let a = receive(&mut work).await;
    agent.post(input(2, "independent B")).await.unwrap();
    receive(&mut routing)
        .await
        .route(vec![create("B", vec![InputId(2)])]);
    let b = receive(&mut work).await;
    assert!(
        !b.payload()
            .to_string()
            .contains("LARGE_FILE_RESULT_SENTINEL")
    );
    let mut notices = agent.subscribe();
    b.work(answer("B"));
    notice(&mut notices, |notice| {
        matches!(notice, Notice::InputFinished { id: InputId(2), .. })
    })
    .await;
    agent.post(input(3, "C uses A result")).await.unwrap();
    receive(&mut routing)
        .await
        .route(vec![JobChange::Create(JobSpec {
            goal: "C".into(),
            inputs: vec![InputId(3)],
            parent: None,
            references: vec![JobId(1)],
        })]);
    let c = receive(&mut work).await;
    assert!(
        c.payload()
            .to_string()
            .contains("LARGE_FILE_RESULT_SENTINEL")
    );
    assert!(!a.reply.is_closed());
    agent.shutdown().await.unwrap();
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn kernel_cannot_submit_business_work_or_smuggle_operations() {
    for wrong_function in [false, true] {
        let (kernel, mut routing) = model("kernel");
        let (solver, mut work) = model("solver");
        let agent = spawn(Arc::new(ModelAdapter::new(kernel, solver)), vec![]);
        let mut notices = agent.subscribe();
        agent.post(input(1, "route A")).await.unwrap();
        let route = receive(&mut routing).await;
        if wrong_function {
            route.work(answer("forged"));
        } else {
            let mut payload = serde_json::to_value(KernelDecision::default()).unwrap();
            payload["operation"] = json!({"name":"read","arguments":{}});
            route.respond("submit_kernel", payload);
        }
        notice(&mut notices, |notice| {
            matches!(notice, Notice::InputRoutingFailed { .. })
        })
        .await;
        assert!(agent.snapshot().await.unwrap().jobs.is_empty());
        assert!(work.try_recv().is_err());
        assert!(!agent.snapshot().await.unwrap().record.iter().any(|entry| matches!(&entry.kind, RecordKind::Notice(Notice::Reply { text, .. }) if text == "forged")));
        agent.shutdown().await.unwrap();
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn stop_cancels_both_provider_requests_and_a_later_job_is_independent() {
    let (kernel, mut routing) = model("kernel");
    let (solver, mut work) = model("solver");
    let agent = spawn(Arc::new(ModelAdapter::new(kernel, solver)), vec![]);
    agent.post(input(1, "A")).await.unwrap();
    receive(&mut routing)
        .await
        .route(vec![create("A", vec![InputId(1)])]);
    let mut a = receive(&mut work).await;
    agent.post(input(2, "pending route")).await.unwrap();
    let mut route = receive(&mut routing).await;
    agent.stop().await.unwrap();
    a.reply.closed().await;
    route.reply.closed().await;
    agent.post(input(3, "Thanks")).await.unwrap();
    receive(&mut routing)
        .await
        .route(vec![create("thanks", vec![InputId(3)])]);
    let thanks = receive(&mut work).await;
    assert_eq!(thanks.payload()["messages"], json!([input(3, "Thanks")]));
    let mut notices = agent.subscribe();
    thanks.work(answer("welcome"));
    notice(&mut notices, |notice| {
        matches!(notice, Notice::InputFinished { id: InputId(3), .. })
    })
    .await;
    assert!(matches!(
        agent.snapshot().await.unwrap().jobs[0].state,
        JobState::Paused | JobState::Cancelled
    ));
    agent.shutdown().await.unwrap();
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn sharing_one_adapter_across_sessions_does_not_share_history_or_jobs() {
    let (model, mut calls) = model("shared");
    let adapter = Arc::new(ModelAdapter::new(model.clone(), model));
    let alpha = spawn(adapter.clone(), vec![]);
    let beta = spawn(adapter, vec![]);
    alpha.post(input(1, "alpha-private")).await.unwrap();
    let first = receive(&mut calls).await;
    beta.post(input(1, "beta-private")).await.unwrap();
    let second = receive(&mut calls).await;
    assert!(first.payload().to_string().contains("alpha-private"));
    assert!(!first.payload().to_string().contains("beta-private"));
    assert!(second.payload().to_string().contains("beta-private"));
    assert!(!second.payload().to_string().contains("alpha-private"));
    first.route(vec![create("alpha", vec![InputId(1)])]);
    let a = receive(&mut calls).await;
    second.route(vec![create("beta", vec![InputId(1)])]);
    let b = receive(&mut calls).await;
    assert!(!a.payload().to_string().contains("beta-private"));
    assert!(!b.payload().to_string().contains("alpha-private"));
    let mut a_notices = alpha.subscribe();
    let mut b_notices = beta.subscribe();
    a.work(answer("alpha"));
    b.work(answer("beta"));
    notice(&mut a_notices, |notice| {
        matches!(notice, Notice::InputFinished { .. })
    })
    .await;
    notice(&mut b_notices, |notice| {
        matches!(notice, Notice::InputFinished { .. })
    })
    .await;
    alpha.shutdown().await.unwrap();
    beta.shutdown().await.unwrap();
}
