//! Hold real adapter/provider futures to make input interleavings deterministic.

use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use bone_agent::{
    AgentHandle, Autonomy, InputDisposition, InputReview, JobContext, JobOutcome, JobRequest,
    KernelConfig, Next, Notice, Operation, RecordKind, Runtime, RuntimeConfig, ToolCall,
    ToolEffect, ToolPort, ToolSpec, WorkResult,
};
use bone_cli::{Effort, ModelAdapter};
use bone_llm::{Model, Protocol, testing};
use rig_core::{
    completion::{
        AssistantContent, CompletionError, CompletionModel, CompletionRequest, CompletionResponse,
        Usage,
    },
    message::{Message, UserContent},
    streaming::StreamingCompletionResponse,
};
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc, oneshot};

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
                "controlled-provider",
            ))
            .unwrap();
    }

    fn work(self, work: WorkResult) {
        self.respond("submit_work", serde_json::to_value(work).unwrap());
    }
    fn review(self, disposition: InputDisposition, reply: Option<&str>) {
        self.respond(
            "submit_input_review",
            serde_json::to_value(InputReview {
                disposition,
                reply: reply.map(str::to_owned),
                note: "input classification".into(),
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
        unreachable!("one complete provider request per invocation")
    }
}

fn controlled_model(id: &str) -> (Model, mpsc::UnboundedReceiver<PendingCall>) {
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

async fn receive(calls: &mut mpsc::UnboundedReceiver<PendingCall>) -> PendingCall {
    tokio::time::timeout(Duration::from_secs(1), calls.recv())
        .await
        .expect("call starts independently")
        .expect("provider available")
}

async fn notice(
    notices: &mut broadcast::Receiver<Notice>,
    predicate: impl Fn(&Notice) -> bool,
) -> Notice {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let event = notices.recv().await.unwrap();
            if predicate(&event) {
                return event;
            }
        }
    })
    .await
    .expect("expected notice")
}

fn answer(text: &str) -> WorkResult {
    WorkResult {
        reply: Some(text.into()),
        next: Next::Finish,
        ..Default::default()
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn keep_answers_busy_input_without_restarting_work_for_shared_or_separate_models() {
    for share_model in [false, true] {
        let (reviewer, mut reviews) = controlled_model("reviewer");
        let (separate_solver, mut work) = controlled_model("solver");
        let solver = if share_model {
            reviewer.clone()
        } else {
            separate_solver
        };
        let agent = spawn(
            Arc::new(
                ModelAdapter::new(reviewer, solver)
                    .with_efforts(Some(Effort::Low), Some(Effort::High)),
            ),
            vec![],
        );
        let mut notices = agent.subscribe();
        agent
            .post("Investigate the original requirement A")
            .await
            .unwrap();
        let held = if share_model {
            receive(&mut reviews).await
        } else {
            receive(&mut work).await
        };
        assert_eq!(held.request.tools[0].name, "submit_work");
        assert_eq!(
            held.request.additional_params.as_ref().unwrap()["reasoning"]["effort"],
            "high"
        );
        let original = agent.snapshot().await.unwrap().work.unwrap();
        for question in ["status one", "status two", "status three"] {
            agent.post(question).await.unwrap();
            let review = receive(&mut reviews).await;
            assert_eq!(review.request.tools.len(), 1);
            assert_eq!(review.request.tools[0].name, "submit_input_review");
            assert_eq!(
                review.request.additional_params.as_ref().unwrap()["reasoning"]["effort"],
                "low"
            );
            let payload = review.payload();
            assert_eq!(payload["messages"][0]["text"], question);
            assert_eq!(
                payload["user_context"][0]["text"],
                "Investigate the original requirement A"
            );
            review.review(InputDisposition::Keep, Some(question));
            notice(
                &mut notices,
                |event| matches!(event, Notice::Reply { text, .. } if text == question),
            )
            .await;
            assert_eq!(agent.snapshot().await.unwrap().work, Some(original));
            assert!(!held.reply.is_closed());
        }
        held.work(answer("Solver answer"));
        notice(&mut notices, |event| {
            matches!(event, Notice::Finished { .. })
        })
        .await;
        let snapshot = agent.snapshot().await.unwrap();
        assert_eq!(
            snapshot
                .jobs
                .iter()
                .filter(|job| matches!(job.request, JobRequest::Work { .. }))
                .count(),
            1
        );
        assert_eq!(
            snapshot
                .jobs
                .iter()
                .filter(|job| matches!(job.request, JobRequest::ReviewInput { .. }))
                .count(),
            3
        );
        assert!(reviews.try_recv().is_err());
        assert!(work.try_recv().is_err());
        assert!(agent.shutdown().await.unwrap().unresolved_jobs.is_empty());
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn work_that_returns_before_review_is_held_then_original_messages_reach_replacement() {
    let (reviewer, mut reviews) = controlled_model("reviewer");
    let (solver, mut work) = controlled_model("solver");
    let agent = spawn(Arc::new(ModelAdapter::new(reviewer, solver)), vec![]);
    let mut notices = agent.subscribe();
    let first = agent.post("Study A").await.unwrap();
    let original = receive(&mut work).await;
    let id = agent.snapshot().await.unwrap().work.unwrap();
    let changed = agent
        .post("How is it going? Also abandon A; consider B.")
        .await
        .unwrap();
    let review = receive(&mut reviews).await;
    original.work(WorkResult {
        note: "Earlier A material".into(),
        requirement: Some("A".into()),
        ..answer("OLD A ANSWER")
    });
    notice(
        &mut notices,
        |event| matches!(event, Notice::JobFinished { id: finished, .. } if *finished == id),
    )
    .await;
    let snapshot = agent.snapshot().await.unwrap();
    assert_eq!(snapshot.candidate, Some(id));
    assert!(snapshot.requirement.is_none());
    assert!(!snapshot.record.iter().any(|entry| matches!(&entry.kind, RecordKind::Notice(Notice::Reply { text, .. }) if text == "OLD A ANSWER")));
    review.review(InputDisposition::Reconsider, None);
    let replacement = receive(&mut work).await;
    let batch = &replacement.payload()["task"]["Work"]["messages"];
    assert_eq!(batch.as_array().unwrap().len(), 2);
    assert_eq!(batch[0]["id"], first.id.0);
    assert_eq!(batch[1]["id"], changed.id.0);
    assert_eq!(
        batch[1]["text"],
        "How is it going? Also abandon A; consider B."
    );
    assert!(
        replacement
            .payload()
            .to_string()
            .contains("Earlier A material")
    );
    replacement.work(WorkResult {
        requirement: Some("B".into()),
        autonomy: Autonomy::Run,
        ..answer("NEW B ANSWER")
    });
    notice(&mut notices, |event| {
        matches!(event, Notice::Finished { .. })
    })
    .await;
    assert_eq!(
        agent.snapshot().await.unwrap().requirement.as_deref(),
        Some("B")
    );
    assert!(
        reviews.try_recv().is_err(),
        "forwarded messages are not reviewed twice"
    );
    assert!(agent.shutdown().await.unwrap().unresolved_jobs.is_empty());
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
    fn run(&self, _: Value, _: JobContext) -> Pin<Box<dyn Future<Output = JobOutcome> + Send>> {
        Box::pin(async {
            JobOutcome::artifact(json!({"content":"LARGE_FILE_RESULT_SENTINEL".repeat(10_000)}))
        })
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn review_projection_excludes_tool_schemas_arguments_and_large_result_contents() {
    let (reviewer, mut reviews) = controlled_model("reviewer");
    let (solver, mut work) = controlled_model("solver");
    let agent = spawn(
        Arc::new(ModelAdapter::new(reviewer, solver)),
        vec![Arc::new(LargeResultTool)],
    );
    agent
        .post("Inspect the source and solve the problem")
        .await
        .unwrap();
    receive(&mut work).await.work(WorkResult {
        note: "Inspecting the source before solving".into(),
        requirement: Some("Solve the original problem".into()),
        autonomy: Autonomy::Run,
        operation: Some(Operation::Tool(ToolCall::new(
            "source",
            json!({"path":"CALL_ARGUMENTS_SENTINEL"}),
        ))),
        ..Default::default()
    });
    let held = receive(&mut work).await;
    assert!(
        held.payload()
            .to_string()
            .contains("LARGE_FILE_RESULT_SENTINEL")
    );
    agent.post("How far have you got?").await.unwrap();
    let review = receive(&mut reviews).await;
    let context = review.payload().to_string();
    assert!(context.contains("Solve the original problem"));
    assert!(context.contains("Inspecting the source before solving"));
    assert!(context.contains("Inspect the source and solve the problem"));
    assert!(!context.contains("LARGE_FILE_RESULT_SENTINEL"));
    assert!(!context.contains("TOOL_SCHEMA_SENTINEL"));
    assert!(!context.contains("CALL_ARGUMENTS_SENTINEL"));
    assert!(
        context.len() < 5000,
        "the reviewer receives metadata, not the full snapshot"
    );
    review.review(
        InputDisposition::Keep,
        Some("The source has returned; work continues."),
    );
    agent.stop().await.unwrap();
    assert!(agent.shutdown().await.unwrap().unresolved_jobs.is_empty());
    assert!(held.reply.is_closed());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn review_cannot_submit_work_or_smuggle_operations_into_its_result() {
    for wrong_function in [false, true] {
        let (reviewer, mut reviews) = controlled_model("reviewer");
        let (solver, mut work) = controlled_model("solver");
        let agent = spawn(Arc::new(ModelAdapter::new(reviewer, solver)), vec![]);
        let mut notices = agent.subscribe();
        agent.post("Solve A").await.unwrap();
        let held = receive(&mut work).await;
        agent.post("status").await.unwrap();
        let review = receive(&mut reviews).await;
        if wrong_function {
            review.work(answer("forged solver answer"));
        } else {
            review.respond(
                "submit_input_review",
                json!({"disposition":"Keep","reply":null,"note":"status","operation":{"Cancel":1}}),
            );
        }
        notice(&mut notices, |event| matches!(event, Notice::Paused)).await;
        let snapshot = agent.snapshot().await.unwrap();
        assert!(!snapshot.autonomous);
        assert!(
            !snapshot
                .jobs
                .iter()
                .any(|job| matches!(job.request, JobRequest::Tool(_)))
        );
        assert!(!snapshot.record.iter().any(|entry| matches!(&entry.kind, RecordKind::Notice(Notice::Reply { text, .. }) if text == "forged solver answer")));
        assert!(agent.shutdown().await.unwrap().unresolved_jobs.is_empty());
        assert!(held.reply.is_closed());
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_pause_review_preserves_later_resume_input_for_the_solver() {
    let (reviewer, mut reviews) = controlled_model("reviewer");
    let (solver, mut work) = controlled_model("solver");
    let agent = spawn(Arc::new(ModelAdapter::new(reviewer, solver)), vec![]);
    let mut notices = agent.subscribe();
    agent.post("Solve A").await.unwrap();
    let held = receive(&mut work).await;
    agent.post("Pause this work").await.unwrap();
    let review = receive(&mut reviews).await;
    let resume = agent.post("Now continue with B").await.unwrap();
    review.review(InputDisposition::Pause, Some("Paused the earlier work."));
    let replacement = receive(&mut work).await;
    let payload = replacement.payload();
    assert_eq!(
        payload["task"]["Work"]["messages"],
        json!([{"id":resume.id,"text":"Now continue with B"}])
    );
    assert!(held.reply.is_closed());
    replacement.work(WorkResult {
        autonomy: Autonomy::Run,
        ..answer("B completed")
    });
    notice(&mut notices, |event| {
        matches!(event, Notice::Finished { .. })
    })
    .await;
    assert!(reviews.try_recv().is_err());
    assert!(agent.shutdown().await.unwrap().unresolved_jobs.is_empty());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn stop_cancels_both_provider_calls_and_thanks_does_not_restart_old_work() {
    let (reviewer, mut reviews) = controlled_model("reviewer");
    let (solver, mut work) = controlled_model("solver");
    let agent = spawn(Arc::new(ModelAdapter::new(reviewer, solver)), vec![]);
    let mut notices = agent.subscribe();
    agent.post("Solve A").await.unwrap();
    let held_work = receive(&mut work).await;
    agent.post("status").await.unwrap();
    let held_review = receive(&mut reviews).await;
    agent.stop().await.unwrap();
    notice(&mut notices, |event| matches!(event, Notice::Stopped)).await;
    let thanks = agent.post("Thanks").await.unwrap();
    let new_work = receive(&mut work).await;
    assert_eq!(
        new_work.payload()["task"]["Work"]["messages"],
        json!([{"id":thanks.id,"text":"Thanks"}])
    );
    assert!(held_work.reply.is_closed());
    assert!(held_review.reply.is_closed());
    new_work.work(answer("You're welcome."));
    notice(&mut notices, |event| {
        matches!(event, Notice::Finished { .. })
    })
    .await;
    assert!(!agent.snapshot().await.unwrap().autonomous);
    assert!(reviews.try_recv().is_err());
    assert!(agent.shutdown().await.unwrap().unresolved_jobs.is_empty());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_shared_model_does_not_share_history_or_work_ownership_between_sessions() {
    let (model, mut calls) = controlled_model("shared-model");
    let adapter = Arc::new(ModelAdapter::new(model.clone(), model));
    let alpha = spawn(adapter.clone(), vec![]);
    let beta = spawn(adapter, vec![]);
    let mut alpha_notices = alpha.subscribe();
    let mut beta_notices = beta.subscribe();
    alpha.post("alpha-private-task").await.unwrap();
    let first = receive(&mut calls).await;
    beta.post("beta-private-task").await.unwrap();
    let second = receive(&mut calls).await;
    assert!(first.payload().to_string().contains("alpha-private-task"));
    assert!(!first.payload().to_string().contains("beta-private-task"));
    assert!(second.payload().to_string().contains("beta-private-task"));
    assert!(!second.payload().to_string().contains("alpha-private-task"));
    first.work(answer("alpha done"));
    second.work(answer("beta done"));
    notice(&mut alpha_notices, |event| {
        matches!(event, Notice::Finished { .. })
    })
    .await;
    notice(&mut beta_notices, |event| {
        matches!(event, Notice::Finished { .. })
    })
    .await;
    assert!(alpha.shutdown().await.unwrap().unresolved_jobs.is_empty());
    assert!(beta.shutdown().await.unwrap().unresolved_jobs.is_empty());
}
