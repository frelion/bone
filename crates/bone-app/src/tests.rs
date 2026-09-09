use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use bone_agent::{
    Assignment, CallContext, CallError, CheckpointDraft, CompactInput, Completion, CoordinateInput,
    JobChange, JobSpec, KernelDecision, ModelPort, PortFuture, ToolCall, ToolEffect, ToolPort,
    ToolSpec, WorkInput, WorkProposal, WorkStep,
};
use bone_llm::{EndpointConfig, service::chatgpt_subscription::ChatGptAuthCache};
use serde_json::json;
use tokio::sync::{Notify, Semaphore};

use crate::{persistence::ResolveWriteResult, *};

mod persistence_faults;

struct CompletingModel {
    work_calls: AtomicUsize,
}

impl ModelPort for CompletingModel {
    fn coordinate(
        &self,
        input: CoordinateInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<KernelDecision, CallError>> {
        let inputs = input.inputs.iter().map(|input| input.id).collect();
        let mut assignment = Assignment::new(JobSpec::new("answer", "session", "answered"));
        assignment.inputs = inputs;
        Box::pin(async move {
            Ok(KernelDecision::Apply {
                changes: vec![JobChange::Create(assignment)],
                constraints: None,
            })
        })
    }

    fn work(
        &self,
        _: WorkInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<WorkProposal, CallError>> {
        let call = self.work_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(WorkProposal::new(if call == 0 {
                WorkStep::Reply("the answer".into())
            } else {
                WorkStep::Finish(Completion::new("finished"))
            }))
        })
    }

    fn compact(
        &self,
        _: CompactInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<CheckpointDraft, CallError>> {
        panic!("test does not compact")
    }
}

struct PatchingModel {
    work_calls: AtomicUsize,
}

impl ModelPort for PatchingModel {
    fn coordinate(
        &self,
        input: CoordinateInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<KernelDecision, CallError>> {
        let mut assignment = Assignment::new(JobSpec::new("patch", "workspace", "file exists"));
        assignment.inputs = input.inputs.iter().map(|input| input.id).collect();
        Box::pin(async move {
            Ok(KernelDecision::Apply {
                changes: vec![JobChange::Create(assignment)],
                constraints: None,
            })
        })
    }

    fn work(
        &self,
        _: WorkInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<WorkProposal, CallError>> {
        let call = self.work_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(WorkProposal::new(if call == 0 {
                WorkStep::Tool(ToolCall::new(
                    "apply_patch",
                    json!({
                        "patch": "*** Begin Patch\n*** Add File: result.txt\n+written\n*** End Patch"
                    }),
                ))
            } else {
                WorkStep::Finish(Completion::new("patched"))
            }))
        })
    }

    fn compact(
        &self,
        _: CompactInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<CheckpointDraft, CallError>> {
        panic!("test does not compact")
    }
}

#[derive(Clone, Copy)]
enum FirstRoutingResult {
    Fail,
    Clarify,
}

struct PausedRoutingModel {
    first: FirstRoutingResult,
    calls: AtomicUsize,
    continue_second: Arc<Notify>,
}

impl ModelPort for PausedRoutingModel {
    fn coordinate(
        &self,
        _: CoordinateInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<KernelDecision, CallError>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            let result = match self.first {
                FirstRoutingResult::Fail => Err(CallError::failed("routing failed")),
                FirstRoutingResult::Clarify => Ok(KernelDecision::Clarify("which target?".into())),
            };
            return Box::pin(async move { result });
        }
        let gate = Arc::clone(&self.continue_second);
        Box::pin(async move {
            gate.notified().await;
            Err(CallError::failed("test routing stopped"))
        })
    }

    fn work(
        &self,
        _: WorkInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<WorkProposal, CallError>> {
        panic!("test does not start work")
    }

    fn compact(
        &self,
        _: CompactInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<CheckpointDraft, CallError>> {
        panic!("test does not compact")
    }
}

struct BlockingBashModel {
    command: String,
    work_calls: AtomicUsize,
}

struct AlwaysToolModel;

struct SelectiveBarrierModel;

impl ModelPort for SelectiveBarrierModel {
    fn coordinate(
        &self,
        input: CoordinateInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<KernelDecision, CallError>> {
        let goal = input.inputs[0].text.clone();
        let mut assignment = Assignment::new(JobSpec::new(goal, "session", "finished"));
        assignment.inputs = input.inputs.iter().map(|input| input.id).collect();
        Box::pin(async move {
            Ok(KernelDecision::Apply {
                changes: vec![JobChange::Create(assignment)],
                constraints: None,
            })
        })
    }

    fn work(
        &self,
        input: WorkInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<WorkProposal, CallError>> {
        Box::pin(async move {
            let step = if input.spec.goal == "block" && input.calls.is_empty() {
                WorkStep::Tool(ToolCall::new("shutdown_barrier", json!({})))
            } else {
                WorkStep::Finish(Completion::new("finished"))
            };
            Ok(WorkProposal::new(step))
        })
    }

    fn compact(
        &self,
        _: CompactInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<CheckpointDraft, CallError>> {
        panic!("test does not compact")
    }
}

struct PausableWorkModel {
    barrier: Arc<ShutdownBarrier>,
}

impl ModelPort for PausableWorkModel {
    fn coordinate(
        &self,
        input: CoordinateInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<KernelDecision, CallError>> {
        let mut assignment = Assignment::new(JobSpec::new("resume", "session", "finished"));
        assignment.inputs = input.inputs.iter().map(|input| input.id).collect();
        Box::pin(async move {
            Ok(KernelDecision::Apply {
                changes: vec![JobChange::Create(assignment)],
                constraints: None,
            })
        })
    }

    fn work(
        &self,
        _: WorkInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<WorkProposal, CallError>> {
        let barrier = Arc::clone(&self.barrier);
        Box::pin(async move {
            barrier.started.fetch_add(1, Ordering::SeqCst);
            barrier.started_changed.notify_one();
            barrier.release.acquire().await.unwrap().forget();
            Ok(WorkProposal::new(WorkStep::Finish(Completion::new(
                "finished",
            ))))
        })
    }

    fn compact(
        &self,
        _: CompactInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<CheckpointDraft, CallError>> {
        panic!("test does not compact")
    }
}

impl ModelPort for AlwaysToolModel {
    fn coordinate(
        &self,
        input: CoordinateInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<KernelDecision, CallError>> {
        let mut assignment = Assignment::new(JobSpec::new("wait", "session", "released"));
        assignment.inputs = input.inputs.iter().map(|input| input.id).collect();
        Box::pin(async move {
            Ok(KernelDecision::Apply {
                changes: vec![JobChange::Create(assignment)],
                constraints: None,
            })
        })
    }

    fn work(
        &self,
        _: WorkInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<WorkProposal, CallError>> {
        Box::pin(async {
            Ok(WorkProposal::new(WorkStep::Tool(ToolCall::new(
                "shutdown_barrier",
                json!({}),
            ))))
        })
    }

    fn compact(
        &self,
        _: CompactInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<CheckpointDraft, CallError>> {
        panic!("test does not compact")
    }
}

struct ShutdownBarrier {
    started: AtomicUsize,
    started_changed: Notify,
    cancelled: AtomicUsize,
    cancelled_changed: Notify,
    release: Semaphore,
}

impl Default for ShutdownBarrier {
    fn default() -> Self {
        Self {
            started: AtomicUsize::new(0),
            started_changed: Notify::new(),
            cancelled: AtomicUsize::new(0),
            cancelled_changed: Notify::new(),
            release: Semaphore::new(0),
        }
    }
}

impl ShutdownBarrier {
    async fn wait_for_started(&self, expected: usize) {
        self.wait_for(&self.started, &self.started_changed, expected)
            .await;
    }

    async fn wait_for_cancelled(&self, expected: usize) {
        self.wait_for(&self.cancelled, &self.cancelled_changed, expected)
            .await;
    }

    async fn wait_for(&self, count: &AtomicUsize, changed: &Notify, expected: usize) {
        loop {
            let notified = changed.notified();
            if count.load(Ordering::SeqCst) >= expected {
                return;
            }
            notified.await;
        }
    }
}

struct ShutdownBarrierTool {
    barrier: Arc<ShutdownBarrier>,
}

impl ToolPort for ShutdownBarrierTool {
    fn specification(&self) -> ToolSpec {
        ToolSpec {
            name: "shutdown_barrier".into(),
            description: "Hold an external write until the test releases it.".into(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false
            }),
            effect: ToolEffect::ExternalWrite,
        }
    }

    fn run(&self, _: serde_json::Value, mut context: CallContext) -> PortFuture<ToolOutcome> {
        let barrier = Arc::clone(&self.barrier);
        Box::pin(async move {
            barrier.started.fetch_add(1, Ordering::SeqCst);
            barrier.started_changed.notify_one();
            context.wait_for_cancellation().await;
            barrier.cancelled.fetch_add(1, Ordering::SeqCst);
            barrier.cancelled_changed.notify_one();
            let permit = barrier.release.acquire().await.unwrap();
            permit.forget();
            ToolOutcome {
                result: Err(CallError {
                    kind: CallErrorKind::Cancelled,
                    message: "shutdown".into(),
                }),
                external_effect: ExternalEffect::Unknown,
            }
        })
    }
}

impl ModelPort for BlockingBashModel {
    fn coordinate(
        &self,
        input: CoordinateInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<KernelDecision, CallError>> {
        let mut assignment = Assignment::new(JobSpec::new("write", "workspace", "file exists"));
        assignment.inputs = input.inputs.iter().map(|input| input.id).collect();
        Box::pin(async move {
            Ok(KernelDecision::Apply {
                changes: vec![JobChange::Create(assignment)],
                constraints: None,
            })
        })
    }

    fn work(
        &self,
        _: WorkInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<WorkProposal, CallError>> {
        let call = self.work_calls.fetch_add(1, Ordering::SeqCst);
        let command = self.command.clone();
        Box::pin(async move {
            Ok(WorkProposal::new(if call == 0 {
                WorkStep::Tool(ToolCall::new("bash", json!({ "command": command })))
            } else {
                WorkStep::Finish(Completion::new("finished"))
            }))
        })
    }

    fn compact(
        &self,
        _: CompactInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<CheckpointDraft, CallError>> {
        panic!("test does not compact")
    }
}

async fn configure_test_model(app: &App) {
    let profile = Profile::new(
        ProfileId::new("test").unwrap(),
        "Test",
        EndpointConfig::OpenAiResponses { base_url: None },
    )
    .unwrap();
    app.save_profile(profile).await.unwrap();
    app.update_config(
        ConfigScope::User,
        ConfigChange::Worker(Some(
            ModelSelection::new(ProfileId::new("test").unwrap(), "test-model").unwrap(),
        )),
    )
    .await
    .unwrap();
}

async fn app_with_model(
    data: &Path,
    workspace: &Path,
    model: Arc<dyn ModelPort>,
) -> (App, WorkspaceInfo) {
    let app = App::with_model(AppOptions::new(data), model).await.unwrap();
    configure_test_model(&app).await;
    let workspace = app.open_workspace(workspace).await.unwrap();
    (app, workspace)
}

async fn configured_app() -> (tempfile::TempDir, App, WorkspaceInfo) {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    let app = App::with_ports(
        AppOptions::new(temporary.path().join("data")),
        Arc::new(CompletingModel {
            work_calls: AtomicUsize::new(0),
        }),
        Vec::new(),
    )
    .await
    .unwrap();
    let profile = Profile::new(
        ProfileId::new("test").unwrap(),
        "Test",
        EndpointConfig::OpenAiResponses { base_url: None },
    )
    .unwrap();
    app.save_profile(profile).await.unwrap();
    app.update_config(
        ConfigScope::User,
        ConfigChange::Worker(Some(
            ModelSelection::new(ProfileId::new("test").unwrap(), "test-model").unwrap(),
        )),
    )
    .await
    .unwrap();
    let workspace = app.open_workspace(workspace_root).await.unwrap();
    (temporary, app, workspace)
}

async fn wait_for_input(session: &Session, input: InputId) {
    let mut view = session.observe();
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut cursor = SessionSeq(0);
        loop {
            let page = session.history(cursor, 256).await.unwrap();
            if page.items.iter().any(|entry| {
                matches!(
                    entry.event,
                    SessionEvent::InputFinished { input: finished, .. }
                        | SessionEvent::InputRejected { input: finished, .. }
                        | SessionEvent::InputCancelled { input: finished }
                        if finished == input
                )
            }) {
                return;
            }
            cursor = page.next_cursor;
            if !page.has_more {
                view.changed().await.unwrap();
            }
        }
    })
    .await
    .unwrap();
}

async fn wait_for_input_state(
    session: &Session,
    input: InputId,
    matches: impl Fn(&InputState) -> bool,
) -> InputView {
    let mut view = session.observe();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(input) = view
                .borrow()
                .inputs
                .iter()
                .find(|candidate| candidate.id == input && matches(&candidate.state))
                .cloned()
            {
                return input;
            }
            view.changed().await.unwrap();
        }
    })
    .await
    .unwrap()
}

async fn wait_for_tool_finished(session: &Session, name: &str) -> (CallRef, ToolOutcome) {
    let mut view = session.observe();
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut cursor = SessionSeq(0);
        loop {
            let page = session.history(cursor, 256).await.unwrap();
            if let Some(finished) = page.items.iter().find_map(|entry| match &entry.event {
                SessionEvent::ToolFinished {
                    call,
                    tool,
                    outcome,
                    ..
                } if tool == name => Some((*call, outcome.clone())),
                _ => None,
            }) {
                return finished;
            }
            cursor = page.next_cursor;
            if !page.has_more {
                view.changed().await.unwrap();
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn headless_session_runs_and_persists_public_history() {
    let (_temporary, app, workspace) = configured_app().await;
    let session = app
        .create_session(workspace.id, "Test session")
        .await
        .unwrap();
    let receipt = session.submit(SubmitInput::new("do it")).await.unwrap();

    wait_for_input(&session, receipt.input).await;

    let history = session.history(SessionSeq(0), 256).await.unwrap();
    assert!(history.items.iter().any(|entry| matches!(
        entry.event,
        SessionEvent::InputSubmitted { input, .. } if input == receipt.input
    )));
    assert!(history.items.iter().any(|entry| matches!(
        &entry.event,
        SessionEvent::Reply { text, .. } if text == "the answer"
    )));
    assert!(history.items.iter().any(|entry| matches!(
        entry.event,
        SessionEvent::InputFinished { input, .. } if input == receipt.input
    )));
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn request_id_is_an_idempotency_key_not_a_second_input() {
    let (_temporary, app, workspace) = configured_app().await;
    let session = app.create_session(workspace.id, "Dedupe").await.unwrap();
    let input = SubmitInput::new("once");
    let first = session.submit(input.clone()).await.unwrap();
    let second = session.submit(input).await.unwrap();
    assert_eq!(first, second);

    let conflict = SubmitInput {
        request_id: RequestId::new(),
        text: "first".into(),
        reply_to: None,
    };
    session.submit(conflict.clone()).await.unwrap();
    let reused = SubmitInput {
        text: "different".into(),
        ..conflict
    };
    assert!(matches!(
        session.submit(reused).await,
        Err(Error::RequestConflict)
    ));
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn input_limit_keeps_the_agent_record_inside_the_journal_envelope() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    let continue_second = Arc::new(Notify::new());
    let (app, workspace) = app_with_model(
        &temporary.path().join("data"),
        &workspace_root,
        Arc::new(PausedRoutingModel {
            first: FirstRoutingResult::Fail,
            calls: AtomicUsize::new(0),
            continue_second,
        }),
    )
    .await;
    let session = app
        .create_session(workspace.id, "Large input")
        .await
        .unwrap();
    let receipt = session
        .submit(SubmitInput::new("\0".repeat(1024 * 1024)))
        .await
        .unwrap();
    wait_for_input_state(&session, receipt.input, |state| {
        matches!(state, InputState::RoutingFailed { .. })
    })
    .await;
    assert!(
        app.test_store()
            .session(session.id())
            .unwrap()
            .unwrap()
            .agent_through
            > 0
    );
    session.close_runtime().await.unwrap();
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn oversized_input_is_rejected_before_it_is_saved() {
    let (_temporary, app, workspace) = configured_app().await;
    let session = app
        .create_session(workspace.id, "Input limit")
        .await
        .unwrap();
    assert!(matches!(
        session
            .submit(SubmitInput::new("x".repeat(1024 * 1024 + 1)))
            .await,
        Err(Error::InvalidState(_))
    ));
    assert!(
        session
            .history(SessionSeq(0), 10)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_history_cursor_beyond_the_storage_range_is_an_empty_page() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    let (app, workspace) = app_with_model(
        &temporary.path().join("data"),
        &workspace_root,
        Arc::new(PausedRoutingModel {
            first: FirstRoutingResult::Clarify,
            calls: AtomicUsize::new(0),
            continue_second: Arc::new(Notify::new()),
        }),
    )
    .await;
    let session = app.create_session(workspace.id, "Cursor").await.unwrap();
    let input = session.submit(SubmitInput::new("wait")).await.unwrap();
    wait_for_input_state(&session, input.input, |state| {
        matches!(state, InputState::WaitingForUser { .. })
    })
    .await;

    let page = session.history(SessionSeq(u64::MAX), 10).await.unwrap();
    assert!(page.items.is_empty());
    assert_eq!(page.next_cursor, SessionSeq(u64::MAX));
    assert!(!page.has_more);
    assert!(
        session
            .snapshot()
            .await
            .unwrap()
            .inputs
            .iter()
            .any(|saved| {
                saved.id == input.input && matches!(saved.state, InputState::WaitingForUser { .. })
            })
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn oversized_frontend_metadata_does_not_stop_a_runtime() {
    let (_temporary, app, workspace) = configured_app().await;
    let session = app.create_session(workspace.id, "Bounds").await.unwrap();
    let input = session.submit(SubmitInput::new("start")).await.unwrap();
    wait_for_input(&session, input.input).await;
    let before = session.snapshot().await.unwrap().runtime;
    assert!(matches!(before, RuntimeState::Running { .. }));

    assert!(matches!(
        session.save_draft("x".repeat(1024 * 1024 + 1)).await,
        Err(Error::InvalidState(_))
    ));
    assert!(matches!(
        session
            .resolve_write(
                CallRef {
                    runtime: RuntimeId::new(),
                    id: 1,
                },
                WriteResolution {
                    external_effect: ExternalEffect::None,
                    evidence: "x".repeat(1024 * 1024 + 1),
                },
            )
            .await,
        Err(Error::InvalidState(_))
    ));
    assert_eq!(session.snapshot().await.unwrap().runtime, before);
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn retrying_a_routing_failure_restores_the_input_to_accepted() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    let continue_second = Arc::new(Notify::new());
    let (app, workspace) = app_with_model(
        &temporary.path().join("data"),
        &workspace_root,
        Arc::new(PausedRoutingModel {
            first: FirstRoutingResult::Fail,
            calls: AtomicUsize::new(0),
            continue_second: Arc::clone(&continue_second),
        }),
    )
    .await;
    let session = app.create_session(workspace.id, "Retry").await.unwrap();
    let receipt = session.submit(SubmitInput::new("try it")).await.unwrap();
    wait_for_input_state(&session, receipt.input, |state| {
        matches!(state, InputState::RoutingFailed { .. })
    })
    .await;

    assert_eq!(
        session.retry(receipt.input).await.unwrap(),
        CommandReceipt::Applied
    );
    let view = session.snapshot().await.unwrap();
    assert!(view.inputs.iter().any(|input| {
        input.id == receipt.input && matches!(input.state, InputState::Accepted { .. })
    }));

    continue_second.notify_one();
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn answering_a_clarification_immediately_clears_the_question_state() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    let continue_second = Arc::new(Notify::new());
    let (app, workspace) = app_with_model(
        &temporary.path().join("data"),
        &workspace_root,
        Arc::new(PausedRoutingModel {
            first: FirstRoutingResult::Clarify,
            calls: AtomicUsize::new(0),
            continue_second: Arc::clone(&continue_second),
        }),
    )
    .await;
    let session = app
        .create_session(workspace.id, "Clarification")
        .await
        .unwrap();
    let original = session.submit(SubmitInput::new("ambiguous")).await.unwrap();
    let waiting = wait_for_input_state(&session, original.input, |state| {
        matches!(state, InputState::WaitingForUser { .. })
    })
    .await;
    let InputState::WaitingForUser { question, .. } = waiting.state else {
        unreachable!("wait helper matched the question state")
    };

    session
        .submit(SubmitInput::new("the library").answer(question))
        .await
        .unwrap();
    let view = session.snapshot().await.unwrap();
    assert!(view.inputs.iter().any(|input| {
        input.id == original.input && matches!(input.state, InputState::Accepted { .. })
    }));
    assert!(
        view.inputs
            .iter()
            .all(|input| !matches!(input.state, InputState::WaitingForUser { .. }))
    );

    continue_second.notify_one();
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn config_changes_apply_to_the_running_runtime() {
    let (_temporary, app, workspace) = configured_app().await;
    let session = app.create_session(workspace.id, "Config").await.unwrap();
    let receipt = session.submit(SubmitInput::new("start")).await.unwrap();
    wait_for_input(&session, receipt.input).await;
    let RuntimeState::Running { id, .. } = session.snapshot().await.unwrap().runtime else {
        panic!("runtime did not start")
    };

    app.update_config(
        ConfigScope::Session(session.id()),
        ConfigChange::Worker(Some(
            ModelSelection::new(ProfileId::new("test").unwrap(), "new-model").unwrap(),
        )),
    )
    .await
    .unwrap();
    let changed = app.resolved_config(session.id()).await.unwrap();
    let desired = changed.desired.unwrap();
    assert_eq!(desired.worker.selection.model, "new-model");
    assert_eq!(changed.running, Some(desired.clone()));
    assert!(matches!(
        session.snapshot().await.unwrap().runtime,
        RuntimeState::Running { id: current, .. } if current == id
    ));
    assert_eq!(
        app.test_store()
            .session(session.id())
            .unwrap()
            .unwrap()
            .runtime
            .unwrap()
            .config,
        desired
    );
    assert!(
        session
            .history(SessionSeq(0), 32)
            .await
            .unwrap()
            .items
            .iter()
            .any(|entry| matches!(
                &entry.event,
                SessionEvent::RuntimeReconfigured { runtime, config }
                    if *runtime == id && config.worker.selection.model == "new-model"
            ))
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn workspace_config_only_reconfigures_sessions_in_that_workspace() {
    let (temporary, app, first_workspace) = configured_app().await;
    let second_root = temporary.path().join("other-workspace");
    std::fs::create_dir(&second_root).unwrap();
    let second_workspace = app.open_workspace(second_root).await.unwrap();
    let first = app
        .create_session(first_workspace.id, "First workspace")
        .await
        .unwrap();
    let second = app
        .create_session(second_workspace.id, "Second workspace")
        .await
        .unwrap();
    let first_input = first.submit(SubmitInput::new("start first")).await.unwrap();
    let second_input = second
        .submit(SubmitInput::new("start second"))
        .await
        .unwrap();
    wait_for_input(&first, first_input.input).await;
    wait_for_input(&second, second_input.input).await;

    let limits = AgentLimits {
        background_workers: 5,
        ..AgentLimits::default()
    };
    app.update_config(
        ConfigScope::Workspace(first_workspace.id),
        ConfigChange::Limits(Some(limits.clone())),
    )
    .await
    .unwrap();

    assert_eq!(
        app.resolved_config(first.id())
            .await
            .unwrap()
            .running
            .unwrap()
            .limits,
        limits
    );
    assert_eq!(
        app.resolved_config(second.id())
            .await
            .unwrap()
            .running
            .unwrap()
            .limits,
        AgentLimits::default()
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn profile_changes_reconfigure_sessions_that_use_the_profile() {
    let (_temporary, app, workspace) = configured_app().await;
    let session = app.create_session(workspace.id, "Profile").await.unwrap();
    let input = session.submit(SubmitInput::new("start")).await.unwrap();
    wait_for_input(&session, input.input).await;
    let RuntimeState::Running { id, .. } = session.snapshot().await.unwrap().runtime else {
        panic!("runtime did not start")
    };

    app.save_profile(
        Profile::new(
            ProfileId::new("test").unwrap(),
            "Renamed test profile",
            EndpointConfig::OpenAiResponses { base_url: None },
        )
        .unwrap(),
    )
    .await
    .unwrap();

    let running = app
        .resolved_config(session.id())
        .await
        .unwrap()
        .running
        .unwrap();
    assert_eq!(running.worker.profile.label, "Renamed test profile");
    assert!(matches!(
        session.snapshot().await.unwrap().runtime,
        RuntimeState::Running { id: current, .. } if current == id
    ));
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_unusable_live_config_is_not_reported_as_applied() {
    let (_temporary, app, workspace) = configured_app().await;
    let session = app
        .create_session(workspace.id, "Invalid config")
        .await
        .unwrap();
    let input = session.submit(SubmitInput::new("start")).await.unwrap();
    wait_for_input(&session, input.input).await;
    let running = app
        .resolved_config(session.id())
        .await
        .unwrap()
        .running
        .unwrap();

    assert!(matches!(
        app.update_config(ConfigScope::User, ConfigChange::Worker(None))
            .await,
        Err(Error::Configuration(ConfigProblem::NeedsModel))
    ));

    let resolved = app.resolved_config(session.id()).await.unwrap();
    assert_eq!(resolved.desired, Err(ConfigProblem::NeedsModel));
    assert_eq!(resolved.running, Some(running));
    assert_eq!(
        session.snapshot().await.unwrap().problem,
        Some(AppProblem::Configuration(ConfigProblem::NeedsModel))
    );

    let blocked = session
        .submit(SubmitInput::new("wait for a model"))
        .await
        .unwrap();
    let view = session.snapshot().await.unwrap();
    assert!(view.inputs.iter().any(|input| {
        input.id == blocked.input
            && matches!(
                input.state,
                InputState::Queued {
                    problem: Some(ConfigProblem::NeedsModel)
                }
            )
    }));
    assert!(
        !session
            .history(SessionSeq(0), 32)
            .await
            .unwrap()
            .items
            .iter()
            .any(|entry| matches!(
                entry.event,
                SessionEvent::InputAccepted { input, .. } if input == blocked.input
            ))
    );

    app.update_config(
        ConfigScope::User,
        ConfigChange::Worker(Some(
            ModelSelection::new(ProfileId::new("test").unwrap(), "test-model").unwrap(),
        )),
    )
    .await
    .unwrap();
    wait_for_input(&session, blocked.input).await;
    assert_eq!(session.snapshot().await.unwrap().problem, None);
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn reload_config_recovers_after_credentials_are_repaired() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    let credentials =
        crate::credentials::ChatGptCredentials::at(temporary.path().join("chatgpt-credentials"))
            .unwrap();
    let providers = ProviderConnector::with_chatgpt_credentials(credentials.clone());
    let app =
        App::with_provider_connector(AppOptions::new(temporary.path().join("data")), providers)
            .await
            .unwrap();
    let profile_id = ProfileId::chatgpt();
    app.save_profile(Profile::chatgpt()).await.unwrap();
    app.update_config(
        ConfigScope::User,
        ConfigChange::Worker(Some(
            ModelSelection::new(profile_id.clone(), "test-model").unwrap(),
        )),
    )
    .await
    .unwrap();
    let workspace = app.open_workspace(workspace_root).await.unwrap();
    let session = app
        .create_session(workspace.id, "Credential recovery")
        .await
        .unwrap();
    let input = session
        .submit(SubmitInput::new("wait for credentials"))
        .await
        .unwrap();

    let mut observed = session.observe();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let view = observed.borrow().clone();
            if view.problem == Some(AppProblem::LoginRequired(profile_id.clone()))
                && view.inputs.iter().any(|candidate| {
                    candidate.id == input.input
                        && matches!(candidate.state, InputState::Queued { .. })
                })
            {
                return;
            }
            observed.changed().await.unwrap();
        }
    })
    .await
    .expect("missing credentials should leave the desired configuration blocked");

    let desired = app
        .resolved_config(session.id())
        .await
        .unwrap()
        .desired
        .unwrap();
    let auth = credentials.acquire().unwrap();
    std::fs::write(
        auth.auth_file(),
        br#"{"access_token":"offline-test-token","expires_at":4102444800,"account_id":"offline-account"}"#,
    )
    .unwrap();
    drop(auth);
    session.reload_config().await.unwrap();

    let resumed = tokio::time::timeout(Duration::from_secs(3), async {
        let mut cursor = SessionSeq(0);
        loop {
            let page = session.history(cursor, 32).await.unwrap();
            if page.items.iter().any(|entry| {
                matches!(
                    entry.event,
                    SessionEvent::InputAccepted { input: accepted, .. }
                        if accepted == input.input
                )
            }) {
                return;
            }
            cursor = page.next_cursor;
            observed.changed().await.unwrap();
        }
    })
    .await;
    assert!(
        resumed.is_ok(),
        "reloading the unchanged desired configuration should resume the input: {:?}",
        session.snapshot().await.unwrap()
    );

    assert_eq!(
        app.resolved_config(session.id()).await.unwrap().running,
        Some(desired)
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_rejected_agent_limit_change_can_resume_the_same_job() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    let barrier = Arc::new(ShutdownBarrier::default());
    let app = App::with_ports(
        AppOptions::new(temporary.path().join("data")),
        Arc::new(PausableWorkModel {
            barrier: Arc::clone(&barrier),
        }),
        Vec::new(),
    )
    .await
    .unwrap();
    configure_test_model(&app).await;
    let workspace = app.open_workspace(workspace_root).await.unwrap();
    let session = app.create_session(workspace.id, "Resume").await.unwrap();
    let input = session
        .submit(SubmitInput::new("keep the job"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), barrier.wait_for_started(1))
        .await
        .unwrap();
    let job = session.snapshot().await.unwrap().jobs[0].id;

    assert!(matches!(
        app.update_config(
            ConfigScope::Session(session.id()),
            ConfigChange::Limits(Some(AgentLimits {
                context_bytes: 1,
                item_bytes: 1,
                ..AgentLimits::default()
            })),
        )
        .await,
        Err(Error::Configuration(ConfigProblem::Invalid(_)))
    ));
    app.update_config(
        ConfigScope::Session(session.id()),
        ConfigChange::Limits(Some(AgentLimits::default())),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), barrier.wait_for_started(2))
        .await
        .expect("the suspended job should restart after valid limits are restored");
    assert_eq!(session.snapshot().await.unwrap().jobs[0].id, job);

    barrier.release.add_permits(1);
    wait_for_input(&session, input.input).await;
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn closing_a_config_blocked_runtime_allows_a_clean_restart() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    let (app, workspace) = app_with_model(
        &temporary.path().join("data"),
        &workspace_root,
        Arc::new(CompletingModel {
            work_calls: AtomicUsize::new(0),
        }),
    )
    .await;
    let session = app.create_session(workspace.id, "Restart").await.unwrap();
    let first = session.submit(SubmitInput::new("start")).await.unwrap();
    wait_for_input(&session, first.input).await;

    std::fs::remove_dir(&workspace_root).unwrap();
    let result = app
        .update_config(
            ConfigScope::Session(session.id()),
            ConfigChange::Tools(Some(ToolSettings {
                mode: ToolMode::WorkspaceWrite,
                limits: ToolLimits {
                    default_bash_timeout: Duration::from_secs(1),
                    max_bash_timeout: Duration::from_secs(1),
                    ..ToolLimits::default()
                },
            })),
        )
        .await;
    assert!(matches!(result, Err(Error::Tools(_))), "{result:?}");
    std::fs::create_dir(&workspace_root).unwrap();

    session.close_runtime().await.unwrap();
    let second = session.submit(SubmitInput::new("restart")).await.unwrap();
    wait_for_input(&session, second.input).await;
    assert_eq!(session.snapshot().await.unwrap().problem, None);
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn resolved_config_does_not_report_a_crashed_runtime_as_live() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    let data = temporary.path().join("data");
    std::fs::create_dir(&root).unwrap();
    let session = {
        let store = DataStore::open(&data).unwrap();
        let workspace = store.workspace(&root).unwrap();
        let session = store
            .create_session(workspace.id, "crashed".into())
            .unwrap();
        let profile = Profile::new(
            ProfileId::new("test").unwrap(),
            "Test",
            EndpointConfig::OpenAiResponses { base_url: None },
        )
        .unwrap();
        let selection = ModelSelection::new(profile.id.clone(), "test-model").unwrap();
        store.save_profile(profile.clone()).unwrap();
        store
            .update_config(
                ConfigScope::User,
                ConfigChange::Worker(Some(selection.clone())),
            )
            .unwrap();
        let config = resolve_runtime(
            &RuntimeSettings {
                worker: Some(selection),
                ..RuntimeSettings::default()
            },
            None,
            None,
            &[profile],
            workspace.root,
        )
        .unwrap();
        store
            .start_runtime(
                session.info.id,
                SavedRuntime {
                    id: RuntimeId::new(),
                    config,
                },
            )
            .unwrap();
        session.info.id
    };
    let app = App::with_ports(
        AppOptions::new(data),
        Arc::new(CompletingModel {
            work_calls: AtomicUsize::new(0),
        }),
        Vec::new(),
    )
    .await
    .unwrap();

    let config = app.resolved_config(session).await.unwrap();
    assert!(config.desired.is_ok());
    assert_eq!(config.running, None);
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn archiving_a_session_does_not_close_its_running_runtime() {
    let (_temporary, app, workspace) = configured_app().await;
    let session = app.create_session(workspace.id, "Archive").await.unwrap();
    let receipt = session.submit(SubmitInput::new("start")).await.unwrap();
    wait_for_input(&session, receipt.input).await;
    let before = session.snapshot().await.unwrap();
    let RuntimeState::Running { id, .. } = before.runtime else {
        panic!("submitting an input should start the runtime")
    };

    session.archive(true).await.unwrap();
    let after = session.snapshot().await.unwrap();
    assert!(after.session.archived);
    assert!(matches!(after.runtime, RuntimeState::Running { id: current, .. } if current == id));
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn app_shutdown_is_dispatched_to_all_sessions_before_waiting_for_any_one() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    let barrier = Arc::new(ShutdownBarrier::default());
    let app = App::with_ports(
        AppOptions::new(temporary.path().join("data")),
        Arc::new(AlwaysToolModel),
        vec![Arc::new(ShutdownBarrierTool {
            barrier: Arc::clone(&barrier),
        })],
    )
    .await
    .unwrap();
    configure_test_model(&app).await;
    app.update_config(
        ConfigScope::User,
        ConfigChange::Limits(Some(AgentLimits {
            shutdown_grace: Duration::from_secs(60),
            ..AgentLimits::default()
        })),
    )
    .await
    .unwrap();
    let workspace = app.open_workspace(workspace_root).await.unwrap();
    let first = app.create_session(workspace.id, "First").await.unwrap();
    let second = app.create_session(workspace.id, "Second").await.unwrap();
    first.submit(SubmitInput::new("wait")).await.unwrap();
    second.submit(SubmitInput::new("wait")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), barrier.wait_for_started(2))
        .await
        .unwrap();

    let shutting_down = tokio::spawn(async move { app.shutdown().await });
    tokio::time::timeout(Duration::from_secs(3), barrier.wait_for_cancelled(2))
        .await
        .expect("both session shutdowns must be dispatched before either tool returns");
    assert_eq!(barrier.cancelled.load(Ordering::SeqCst), 2);

    barrier.release.add_permits(2);
    tokio::time::timeout(Duration::from_secs(3), shutting_down)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn config_reload_reaches_idle_sessions_without_waiting_for_a_closing_session() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    let barrier = Arc::new(ShutdownBarrier::default());
    let app = App::with_ports(
        AppOptions::new(temporary.path().join("data")),
        Arc::new(SelectiveBarrierModel),
        vec![Arc::new(ShutdownBarrierTool {
            barrier: Arc::clone(&barrier),
        })],
    )
    .await
    .unwrap();
    configure_test_model(&app).await;
    let workspace = app.open_workspace(workspace_root).await.unwrap();
    let first = app.create_session(workspace.id, "First").await.unwrap();
    let second = app.create_session(workspace.id, "Second").await.unwrap();
    let (closing, idle) = if first.id() < second.id() {
        (first, second)
    } else {
        (second, first)
    };

    closing.submit(SubmitInput::new("block")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), barrier.wait_for_started(1))
        .await
        .unwrap();
    let idle_input = idle.submit(SubmitInput::new("idle")).await.unwrap();
    wait_for_input(&idle, idle_input.input).await;

    let closing_task = {
        let closing = closing.clone();
        tokio::spawn(async move { closing.close_runtime().await })
    };
    tokio::time::timeout(Duration::from_secs(3), barrier.wait_for_cancelled(1))
        .await
        .unwrap();

    let limits = AgentLimits {
        background_workers: 5,
        ..AgentLimits::default()
    };
    let updating = {
        let app = app.clone();
        let limits = limits.clone();
        tokio::spawn(async move {
            app.update_config(ConfigScope::User, ConfigChange::Limits(Some(limits)))
                .await
        })
    };

    let mut idle_view = idle.observe();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if matches!(
                &idle_view.borrow().runtime,
                RuntimeState::Running { config, .. } if config.limits == limits
            ) {
                return;
            }
            idle_view.changed().await.unwrap();
        }
    })
    .await
    .expect("an idle session should reload while another session is closing");
    assert!(!updating.is_finished());
    let resolving = {
        let app = app.clone();
        let session = idle.id();
        tokio::spawn(async move { app.resolved_config(session).await })
    };
    tokio::task::yield_now().await;
    assert!(
        !resolving.is_finished(),
        "resolved_config must not cross an unfinished configuration barrier"
    );

    updating.abort();
    assert!(updating.await.unwrap_err().is_cancelled());
    tokio::task::yield_now().await;
    assert!(
        !resolving.is_finished(),
        "cancelling the caller must not cancel an update that was already persisted"
    );

    barrier.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(3), closing_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let resolved = tokio::time::timeout(Duration::from_secs(3), resolving)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(resolved.desired.unwrap().limits, limits);
    assert_eq!(resolved.running.unwrap().limits, limits);
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn app_shutdown_linearizes_with_a_write_waiting_to_record_its_intent() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    let (app, workspace) = app_with_model(
        &temporary.path().join("data"),
        &workspace_root,
        Arc::new(PatchingModel {
            work_calls: AtomicUsize::new(0),
        }),
    )
    .await;
    app.update_config(
        ConfigScope::User,
        ConfigChange::Limits(Some(AgentLimits {
            tool_timeout: Duration::from_secs(601),
            shutdown_grace: Duration::from_millis(10),
            ..AgentLimits::default()
        })),
    )
    .await
    .unwrap();
    app.update_config(
        ConfigScope::User,
        ConfigChange::Tools(Some(ToolSettings {
            mode: ToolMode::WorkspaceWrite,
            limits: ToolLimits::default(),
        })),
    )
    .await
    .unwrap();
    let session = app
        .create_session(workspace.id, "Write race")
        .await
        .unwrap();
    let gate = app.test_write_gate(workspace.id).await;
    let guard = gate.lock.lock().await;
    session.submit(SubmitInput::new("write")).await.unwrap();
    let mut view = session.observe();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if view.borrow().activity.iter().any(|activity| {
                matches!(
                    &activity.kind,
                    ActivityKind::Tool { name } if name == "apply_patch"
                )
            }) {
                break;
            }
            view.changed().await.unwrap();
        }
    })
    .await
    .unwrap();

    let closing = app.clone();
    let mut shutdown = tokio::spawn(async move { closing.shutdown().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut shutdown)
            .await
            .is_err(),
        "shutdown must synchronize with a write between cancellation and intent recording"
    );
    drop(guard);
    let report = tokio::time::timeout(Duration::from_secs(3), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(report.unresolved_writes.is_empty());
    assert!(!workspace_root.join("result.txt").exists());
}

#[tokio::test]
async fn close_runtime_linearizes_with_a_write_waiting_to_record_its_intent() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    let (app, workspace) = app_with_model(
        &temporary.path().join("data"),
        &workspace_root,
        Arc::new(PatchingModel {
            work_calls: AtomicUsize::new(0),
        }),
    )
    .await;
    app.update_config(
        ConfigScope::User,
        ConfigChange::Limits(Some(AgentLimits {
            tool_timeout: Duration::from_secs(601),
            shutdown_grace: Duration::from_millis(10),
            ..AgentLimits::default()
        })),
    )
    .await
    .unwrap();
    app.update_config(
        ConfigScope::User,
        ConfigChange::Tools(Some(ToolSettings {
            mode: ToolMode::WorkspaceWrite,
            limits: ToolLimits::default(),
        })),
    )
    .await
    .unwrap();
    let session = app
        .create_session(workspace.id, "Runtime close race")
        .await
        .unwrap();
    let gate = app.test_write_gate(workspace.id).await;
    let guard = gate.lock.lock().await;
    session.submit(SubmitInput::new("write")).await.unwrap();
    let mut view = session.observe();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if view.borrow().activity.iter().any(|activity| {
                matches!(
                    &activity.kind,
                    ActivityKind::Tool { name } if name == "apply_patch"
                )
            }) {
                break;
            }
            view.changed().await.unwrap();
        }
    })
    .await
    .unwrap();

    let closing = session.clone();
    let mut close = tokio::spawn(async move { closing.close_runtime().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut close)
            .await
            .is_err(),
        "runtime close must synchronize with a write before intent recording"
    );
    drop(guard);
    let report = tokio::time::timeout(Duration::from_secs(3), close)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(report.unresolved_writes.is_empty());
    assert!(!workspace_root.join("result.txt").exists());
    app.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn a_detached_write_keeps_the_session_lease_until_execution_finishes() {
    let temporary = tempfile::tempdir().unwrap();
    let data = temporary.path().join("data");
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    for name in ["started.pipe", "release.pipe"] {
        let status = std::process::Command::new("mkfifo")
            .arg(workspace_root.join(name))
            .status()
            .unwrap();
        assert!(status.success());
    }
    let command = "printf 'started\\n' > started.pipe\n\
                   IFS= read -r _ < release.pipe\n\
                   printf 'written\\n' > result.txt";
    let (app, workspace) = app_with_model(
        &data,
        &workspace_root,
        Arc::new(BlockingBashModel {
            command: command.into(),
            work_calls: AtomicUsize::new(0),
        }),
    )
    .await;
    app.update_config(
        ConfigScope::User,
        ConfigChange::Limits(Some(AgentLimits {
            tool_timeout: Duration::from_secs(601),
            shutdown_grace: Duration::from_millis(10),
            ..AgentLimits::default()
        })),
    )
    .await
    .unwrap();
    app.update_config(
        ConfigScope::User,
        ConfigChange::Tools(Some(ToolSettings {
            mode: ToolMode::WorkspaceWrite,
            limits: ToolLimits::default(),
        })),
    )
    .await
    .unwrap();

    let started_path = workspace_root.join("started.pipe");
    let started = tokio::task::spawn_blocking(move || std::fs::read_to_string(started_path));
    let session = app
        .create_session(workspace.id, "Write lease")
        .await
        .unwrap();
    session.submit(SubmitInput::new("write")).await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), started)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        "started\n"
    );

    tokio::time::timeout(Duration::from_secs(3), app.shutdown())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !session.actor_closed() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    let contender = App::with_model(
        AppOptions::new(&data),
        Arc::new(CompletingModel {
            work_calls: AtomicUsize::new(0),
        }),
    )
    .await
    .unwrap();
    let busy = matches!(
        contender.session(session.id()).await,
        Err(Error::SessionBusy(id)) if id == session.id()
    );

    let release_path = workspace_root.join("release.pipe");
    tokio::time::timeout(
        Duration::from_secs(3),
        tokio::task::spawn_blocking(move || std::fs::write(release_path, b"continue\n")),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    let reopened = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match contender.session(session.id()).await {
                Ok(session) => break session,
                Err(Error::SessionBusy(_)) => tokio::task::yield_now().await,
                Err(error) => panic!("unexpected reopen error: {error}"),
            }
        }
    })
    .await
    .unwrap();

    assert!(busy, "the detached write must retain the session lease");
    assert_eq!(
        std::fs::read_to_string(workspace_root.join("result.txt")).unwrap(),
        "written\n"
    );
    drop(reopened);
    contender.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn a_timed_out_bash_write_is_persisted_as_an_unknown_external_effect() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    let (app, workspace) = app_with_model(
        &temporary.path().join("data"),
        &workspace_root,
        Arc::new(BlockingBashModel {
            command: "printf 'effect\\n' > effect.txt; sleep 5".into(),
            work_calls: AtomicUsize::new(0),
        }),
    )
    .await;
    app.update_config(
        ConfigScope::User,
        ConfigChange::Limits(Some(AgentLimits {
            tool_timeout: Duration::from_secs(2),
            ..AgentLimits::default()
        })),
    )
    .await
    .unwrap();
    app.update_config(
        ConfigScope::User,
        ConfigChange::Tools(Some(ToolSettings {
            mode: ToolMode::WorkspaceWrite,
            limits: ToolLimits {
                default_bash_timeout: Duration::from_secs(1),
                max_bash_timeout: Duration::from_secs(1),
                ..ToolLimits::default()
            },
        })),
    )
    .await
    .unwrap();

    let session = app
        .create_session(workspace.id, "Bash timeout")
        .await
        .unwrap();
    session
        .submit(SubmitInput::new("write then wait"))
        .await
        .unwrap();
    let (call, outcome) = wait_for_tool_finished(&session, "bash").await;
    assert_eq!(outcome.external_effect, ExternalEffect::Unknown);
    assert_eq!(
        std::fs::read_to_string(workspace_root.join("effect.txt")).unwrap(),
        "effect\n"
    );
    let writes = app.unresolved_writes(workspace.id).await.unwrap();
    let unresolved = writes
        .iter()
        .find(|write| write.call == call)
        .expect("timed-out write remains unresolved");
    assert_eq!(unresolved.status, UnresolvedWriteStatus::Finished);
    assert_eq!(
        unresolved
            .outcome
            .as_ref()
            .map(|outcome| outcome.external_effect),
        Some(ExternalEffect::Unknown)
    );
    assert_eq!(
        session
            .resolve_write(
                call,
                WriteResolution {
                    external_effect: ExternalEffect::Applied,
                    evidence: "effect.txt contains the expected line".into(),
                },
            )
            .await
            .unwrap(),
        CommandReceipt::Applied
    );
    let history = session.history(SessionSeq(0), 32).await.unwrap();
    assert!(history.items.iter().any(|entry| {
        matches!(
            &entry.event,
            SessionEvent::ToolFinished { call: saved, outcome, .. }
                if *saved == call
                    && outcome.external_effect == ExternalEffect::Applied
                    && outcome.result.as_ref().is_err_and(|error| {
                        error.message.contains("original tool result unavailable")
                    })
        )
    }));
    assert!(history.items.iter().any(|entry| {
        matches!(
            &entry.event,
            SessionEvent::WriteResolved { call: saved, external_effect, evidence }
                if *saved == call
                    && *external_effect == ExternalEffect::Applied
                    && evidence == "effect.txt contains the expected line"
        )
    }));
    assert!(
        app.unresolved_writes(workspace.id)
            .await
            .unwrap()
            .is_empty()
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn workspace_writes_are_recorded_by_the_app_tool_adapter() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    let app = App::with_model(
        AppOptions::new(temporary.path().join("data")),
        Arc::new(PatchingModel {
            work_calls: AtomicUsize::new(0),
        }),
    )
    .await
    .unwrap();
    app.save_profile(
        Profile::new(
            ProfileId::new("test").unwrap(),
            "Test",
            EndpointConfig::OpenAiResponses { base_url: None },
        )
        .unwrap(),
    )
    .await
    .unwrap();
    app.update_config(
        ConfigScope::User,
        ConfigChange::Worker(Some(
            ModelSelection::new(ProfileId::new("test").unwrap(), "test-model").unwrap(),
        )),
    )
    .await
    .unwrap();
    app.update_config(
        ConfigScope::User,
        ConfigChange::Limits(Some(AgentLimits {
            tool_timeout: Duration::from_secs(601),
            ..AgentLimits::default()
        })),
    )
    .await
    .unwrap();
    app.update_config(
        ConfigScope::User,
        ConfigChange::Tools(Some(ToolSettings {
            mode: ToolMode::WorkspaceWrite,
            limits: ToolLimits::default(),
        })),
    )
    .await
    .unwrap();
    let workspace = app.open_workspace(&workspace_root).await.unwrap();
    let session = app.create_session(workspace.id, "Write").await.unwrap();
    let receipt = session.submit(SubmitInput::new("write it")).await.unwrap();
    wait_for_input(&session, receipt.input).await;
    assert_eq!(
        std::fs::read_to_string(workspace_root.join("result.txt")).unwrap(),
        "written\n"
    );
    assert!(
        app.unresolved_writes(workspace.id)
            .await
            .unwrap()
            .is_empty()
    );
    app.shutdown().await.unwrap();
}

#[test]
fn a_session_cannot_resolve_another_sessions_write() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    std::fs::create_dir(&root).unwrap();
    let store = DataStore::open(temporary.path().join("data")).unwrap();
    let workspace = store.workspace(&root).unwrap();
    let owner = store.create_session(workspace.id, "owner".into()).unwrap();
    let other = store.create_session(workspace.id, "other".into()).unwrap();
    let call = CallRef {
        runtime: RuntimeId::new(),
        id: 1,
    };
    store
        .begin_write(workspace.id, owner.info.id, call, "apply_patch", json!({}))
        .unwrap();
    let resolution = WriteResolution {
        external_effect: ExternalEffect::Applied,
        evidence: "checked".into(),
    };

    assert!(matches!(
        store
            .resolve_write(workspace.id, other.info.id, call, resolution.clone())
            .unwrap(),
        ResolveWriteResult::Unchanged
    ));
    assert_eq!(
        store
            .unresolved_writes(workspace.id, Some(owner.info.id))
            .unwrap()
            .len(),
        1
    );
    assert!(matches!(
        store
            .resolve_write(workspace.id, owner.info.id, call, resolution.clone())
            .unwrap(),
        ResolveWriteResult::Applied
    ));
    assert!(matches!(
        store
            .resolve_write(workspace.id, owner.info.id, call, resolution)
            .unwrap(),
        ResolveWriteResult::AlreadyResolved(saved)
            if saved == ExternalEffect::Applied
    ));
    assert!(matches!(
        store
            .resolve_write(
                workspace.id,
                owner.info.id,
                call,
                WriteResolution {
                    external_effect: ExternalEffect::None,
                    evidence: "different".into(),
                },
            )
            .unwrap(),
        ResolveWriteResult::Conflicts(ExternalEffect::Applied)
    ));
}

#[tokio::test]
async fn app_queries_workspace_writes_authoritatively() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    std::fs::create_dir(&root).unwrap();
    let app = App::with_ports(
        AppOptions::new(temporary.path().join("data")),
        Arc::new(CompletingModel {
            work_calls: AtomicUsize::new(0),
        }),
        Vec::new(),
    )
    .await
    .unwrap();
    let workspace = app.open_workspace(&root).await.unwrap();
    let owner = app.create_session(workspace.id, "owner").await.unwrap();
    let call = CallRef {
        runtime: RuntimeId::new(),
        id: 1,
    };
    app.test_store()
        .begin_write(workspace.id, owner.id(), call, "apply_patch", json!({}))
        .unwrap();

    let writes = app.unresolved_writes(workspace.id).await.unwrap();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].session, owner.id());
    assert_eq!(writes[0].call, call);

    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn app_shutdown_reports_crash_writes_from_unopened_sessions() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    let data = temporary.path().join("data");
    std::fs::create_dir(&root).unwrap();
    let (workspace, session, call) = {
        let store = DataStore::open(&data).unwrap();
        let workspace = store.workspace(&root).unwrap();
        let session = store
            .create_session(workspace.id, "crashed".into())
            .unwrap();
        let call = CallRef {
            runtime: RuntimeId::new(),
            id: 1,
        };
        store
            .begin_write(
                workspace.id,
                session.info.id,
                call,
                "apply_patch",
                json!({}),
            )
            .unwrap();
        (workspace.id, session.info.id, call)
    };
    let app = App::with_ports(
        AppOptions::new(data),
        Arc::new(CompletingModel {
            work_calls: AtomicUsize::new(0),
        }),
        Vec::new(),
    )
    .await
    .unwrap();

    let report = app.shutdown().await.unwrap();
    assert!(matches!(
        report.unresolved_writes.as_slice(),
        [write]
            if write.workspace == workspace
                && write.session == session
                && write.call == call
    ));
}

#[test]
fn host_write_resolutions_never_invent_tool_success() {
    for effect in [ExternalEffect::None, ExternalEffect::Applied] {
        let outcome = crate::session::host_resolution_outcome(effect);
        assert_eq!(outcome.external_effect, effect);
        assert!(matches!(
            outcome.result,
            Err(error) if error.message.contains("original tool result unavailable")
        ));
    }
}

#[test]
fn a_finished_write_stays_blocking_until_its_agent_record_is_saved() {
    use bone_agent::{CallId, JobId, Origin, Record, RecordBody, Seq, ToolOutcome};

    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    std::fs::create_dir(&root).unwrap();
    let store = DataStore::open(temporary.path().join("data")).unwrap();
    let workspace = store.workspace(&root).unwrap();
    let session = store.create_session(workspace.id, "write".into()).unwrap();
    let profile = Profile::new(
        ProfileId::new("test").unwrap(),
        "Test",
        EndpointConfig::OpenAiResponses { base_url: None },
    )
    .unwrap();
    let selection = ModelSelection::new(profile.id.clone(), "test-model").unwrap();
    let config = resolve_runtime(
        &RuntimeSettings {
            worker: Some(selection),
            ..RuntimeSettings::default()
        },
        None,
        None,
        &[profile],
        workspace.root.clone(),
    )
    .unwrap();
    let runtime = RuntimeId::new();
    store
        .start_runtime(
            session.info.id,
            SavedRuntime {
                id: runtime,
                config,
            },
        )
        .unwrap();
    let call = CallRef { runtime, id: 7 };
    store
        .begin_write(
            workspace.id,
            session.info.id,
            call,
            "apply_patch",
            json!({}),
        )
        .unwrap();
    let outcome = ToolOutcome {
        result: Ok(json!({ "changed": true })),
        external_effect: ExternalEffect::Applied,
    };
    store
        .finish_write(workspace.id, call, outcome.clone())
        .unwrap();
    assert_eq!(
        store
            .unresolved_writes(workspace.id, Some(session.info.id))
            .unwrap()
            .len(),
        1
    );

    let record = Record {
        seq: Seq(1),
        origin: Origin::Kernel,
        body: RecordBody::ToolFinished {
            job: JobId(3),
            call: CallId(call.id),
            request: Arc::new(ToolCall::new("apply_patch", json!({}))),
            outcome: Arc::new(outcome),
        },
    };
    store
        .save_agent_record(session.info.id, runtime, &record, &[])
        .unwrap();
    assert!(
        store
            .unresolved_writes(workspace.id, Some(session.info.id))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn an_answer_clears_every_input_waiting_on_the_same_question() {
    use bone_agent::{
        Input as AgentInput, InputId as AgentInputId, Origin, Record, RecordBody, Seq,
    };

    let runtime = RuntimeId::new();
    let question_record = 7;
    let mut inputs = std::collections::BTreeMap::new();
    for id in [InputId(1), InputId(2)] {
        inputs.insert(
            id,
            InputView {
                id,
                request_id: RequestId::new(),
                text: format!("input {}", id.0),
                reply_to: None,
                state: InputState::WaitingForUser {
                    runtime,
                    question: QuestionId {
                        runtime,
                        record: question_record,
                        reply_to: id,
                    },
                    text: "which target?".into(),
                },
            },
        );
    }
    let record = Record {
        seq: Seq(8),
        origin: Origin::User(AgentInputId(3)),
        body: RecordBody::Input(
            AgentInput::new(AgentInputId(3), "the library")
                .answering(AgentInputId(1), Seq(question_record)),
        ),
    };

    let changes = crate::session::input_changes(runtime, &record, &inputs);
    assert_eq!(
        changes,
        vec![
            (InputId(1), InputState::Accepted { runtime }),
            (InputId(2), InputState::Accepted { runtime }),
        ]
    );
}

#[test]
fn retry_reconciles_every_failed_input_that_the_agent_restored() {
    use bone_agent::{Input as AgentInput, InputId as AgentInputId, InputStatus, Seq};

    let runtime = RuntimeId::new();
    let mut inputs = std::collections::BTreeMap::new();
    for id in [InputId(1), InputId(2), InputId(3)] {
        inputs.insert(
            id,
            InputView {
                id,
                request_id: RequestId::new(),
                text: format!("input {}", id.0),
                reply_to: None,
                state: InputState::RoutingFailed {
                    runtime,
                    message: "failed".into(),
                },
            },
        );
    }
    let agent = bone_agent::AgentView {
        inputs: [
            (1, InputStatus::Routing),
            (2, InputStatus::Handled),
            (
                3,
                InputStatus::RoutingFailed {
                    message: "still failed".into(),
                },
            ),
        ]
        .into_iter()
        .map(|(id, status)| bone_agent::InputView {
            input: AgentInput::new(AgentInputId(id), format!("input {id}")),
            accepted_at: Seq(id),
            status,
            required_jobs: Vec::new(),
        })
        .collect(),
        ..bone_agent::AgentView::default()
    };

    assert_eq!(
        crate::session::restored_routing_inputs(runtime, &inputs, &agent),
        vec![InputId(1), InputId(2)]
    );
}
