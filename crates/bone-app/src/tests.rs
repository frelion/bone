use std::{
    future::{Future, poll_fn},
    path::Path,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::Poll,
    time::Duration,
};

use bone_adapters::llm::EndpointConfig;
use bone_core::{
    Assignment, CallContext, CallError, CheckpointDraft, CompactInput, Completion,
    ConversationInput, ConversationStep, JobSpec, ModelPort, PortFuture, RecordBody, ToolCall,
    ToolEffect, ToolPort, ToolSpec, WorkInput, WorkProposal, WorkStep,
};
use serde_json::json;
use tokio::sync::{Notify, Semaphore};

use crate::{persistence::ResolveWriteResult, *};

mod assembly;
mod persistence_faults;

fn converse_for_work(input: &ConversationInput, goal: impl Into<String>) -> ConversationStep {
    let unassigned = input
        .inputs
        .iter()
        .filter(|candidate| {
            !input
                .jobs
                .iter()
                .any(|job| job.inputs.contains(&candidate.id))
        })
        .map(|input| input.id)
        .collect::<Vec<_>>();
    if !unassigned.is_empty() {
        let mut assignment = Assignment::new(JobSpec::new(goal, "user request", "verified"));
        assignment.inputs = unassigned;
        return ConversationStep::Start(vec![assignment]);
    }
    let finished = input
        .inputs
        .iter()
        .filter_map(|candidate| {
            let mut jobs = input
                .jobs
                .iter()
                .filter(|job| job.inputs.contains(&candidate.id));
            let outcome =
                jobs.try_fold(InputOutcome::Completed, |current, job| match &job.status {
                    bone_core::JobStatus::Finished(result) => Some(match result.kind {
                        OutcomeKind::Failed => InputOutcome::Failed,
                        OutcomeKind::Cancelled if current == InputOutcome::Completed => {
                            InputOutcome::Cancelled
                        }
                        _ => current,
                    }),
                    _ => None,
                })?;
            Some((candidate.id, outcome))
        })
        .collect::<Vec<_>>();
    if let Some((_, outcome)) = finished.first() {
        return ConversationStep::Reply {
            inputs: finished
                .iter()
                .filter(|(_, value)| value == outcome)
                .map(|(id, _)| *id)
                .collect(),
            text: "the answer".into(),
            outcome: outcome.clone(),
        };
    }
    ConversationStep::Wait
}

struct CompletingModel;

impl ModelPort for CompletingModel {
    fn converse(
        &self,
        input: ConversationInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<ConversationStep, CallError>> {
        let decision = converse_for_work(&input, "answer");
        Box::pin(async move { Ok(decision) })
    }

    fn work(
        &self,
        _: WorkInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<WorkProposal, CallError>> {
        Box::pin(async {
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

struct PatchingModel {
    work_calls: AtomicUsize,
}

impl ModelPort for PatchingModel {
    fn converse(
        &self,
        input: ConversationInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<ConversationStep, CallError>> {
        let decision = converse_for_work(&input, "patch");
        Box::pin(async move { Ok(decision) })
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
enum FirstConversationResult {
    Fail,
    Clarify,
}

struct PausedConversationModel {
    first: FirstConversationResult,
    calls: AtomicUsize,
    continue_second: Arc<Notify>,
}

impl ModelPort for PausedConversationModel {
    fn converse(
        &self,
        input: ConversationInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<ConversationStep, CallError>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            let result = match self.first {
                FirstConversationResult::Fail => Err(CallError::failed("conversation failed")),
                FirstConversationResult::Clarify => Ok(ConversationStep::Ask {
                    inputs: input.inputs.iter().map(|input| input.id).collect(),
                    question: "which target?".into(),
                }),
            };
            return Box::pin(async move { result });
        }
        let gate = Arc::clone(&self.continue_second);
        Box::pin(async move {
            gate.notified().await;
            Err(CallError::failed("test conversation stopped"))
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

struct EvidenceModel;

impl ModelPort for EvidenceModel {
    fn converse(
        &self,
        input: ConversationInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<ConversationStep, CallError>> {
        let decision = converse_for_work(&input, "produce cited evidence");
        Box::pin(async move { Ok(decision) })
    }

    fn work(
        &self,
        input: WorkInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<WorkProposal, CallError>> {
        Box::pin(async move {
            let mut tool = None;
            let mut note = None;
            for view in &input.records {
                match serde_json::from_str::<RecordBody>(&view.content) {
                    Ok(RecordBody::ToolFinished { .. }) => tool = Some(view.source),
                    Ok(RecordBody::Note { .. }) => note = Some(view.source),
                    _ => {}
                }
            }
            if tool.is_none() {
                return Ok(WorkProposal::new(WorkStep::Tool(ToolCall::new(
                    "evidence_tool",
                    json!({"token": "ARGUMENT_SECRET"}),
                ))));
            }
            if note.is_none() {
                let mut proposal = WorkProposal::new(WorkStep::Continue);
                proposal.note = Some("PRIVATE_PLANNING_NOTE".into());
                return Ok(proposal);
            }
            let mut completion = Completion::new("evidence complete");
            completion.evidence = vec![tool.unwrap(), note.unwrap()];
            Ok(WorkProposal::new(WorkStep::Finish(completion)))
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

struct EvidenceTool;

impl ToolPort for EvidenceTool {
    fn specification(&self) -> ToolSpec {
        ToolSpec {
            name: "evidence_tool".into(),
            description: "Return deterministic evidence.".into(),
            parameters: json!({"type": "object"}),
            effect: ToolEffect::ReadOnly,
        }
    }

    fn run(&self, _: serde_json::Value, _: CallContext) -> PortFuture<ToolOutcome> {
        Box::pin(async { ToolOutcome::value(json!({"body": "产物内容-abcdefgh"})) })
    }
}

struct SelectiveBarrierModel;

impl ModelPort for SelectiveBarrierModel {
    fn converse(
        &self,
        input: ConversationInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<ConversationStep, CallError>> {
        let goal = input.inputs[0].text.clone();
        let decision = converse_for_work(&input, goal);
        Box::pin(async move { Ok(decision) })
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
    fn converse(
        &self,
        input: ConversationInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<ConversationStep, CallError>> {
        let decision = converse_for_work(&input, "resume");
        Box::pin(async move { Ok(decision) })
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
    fn converse(
        &self,
        input: ConversationInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<ConversationStep, CallError>> {
        let decision = converse_for_work(&input, "wait");
        Box::pin(async move { Ok(decision) })
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
    fn converse(
        &self,
        input: ConversationInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<ConversationStep, CallError>> {
        let decision = converse_for_work(&input, "write");
        Box::pin(async move { Ok(decision) })
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
    let app = App::with_model(AppOptions::isolated(data), model)
        .await
        .unwrap();
    configure_test_model(&app).await;
    let workspace = app.open_workspace(workspace).await.unwrap();
    (app, workspace)
}

async fn configured_app() -> (tempfile::TempDir, App, WorkspaceInfo) {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    let app = App::with_ports(
        AppOptions::isolated(temporary.path().join("data")),
        Arc::new(CompletingModel),
        Vec::new(),
    )
    .await
    .unwrap();
    configure_test_model(&app).await;
    let workspace = app.open_workspace(workspace_root).await.unwrap();
    (temporary, app, workspace)
}

async fn wait_for_config(app: &App, session: &Session) {
    let mut observed = session.observe();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let config = app.resolved_config(session.id()).await.unwrap();
            if config.desired.as_ref().ok() == config.running.as_ref() {
                return;
            }
            observed.changed().await.unwrap();
        }
    })
    .await
    .expect("saved configuration should reach the runtime");
}

#[tokio::test]
async fn hung_model_preparation_does_not_block_controls_and_latest_selection_wins() {
    struct Cancelled(Arc<AtomicUsize>);
    impl Drop for Cancelled {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let temporary = tempfile::tempdir().unwrap();
    let started = Arc::new(Notify::new());
    let cancelled = Arc::new(AtomicUsize::new(0));
    let backend = crate::app::RuntimeBackend::Factory(Arc::new({
        let started = started.clone();
        let cancelled = cancelled.clone();
        move |config| {
            let started = started.clone();
            let cancelled = cancelled.clone();
            Box::pin(async move {
                if config.worker.selection.model == "blocked" {
                    let _cancelled = Cancelled(cancelled);
                    started.notify_one();
                    std::future::pending::<()>().await;
                }
                Ok(Arc::new(CompletingModel) as Arc<dyn ModelPort>)
            })
        }
    }));
    let app =
        App::with_backend(AppOptions::isolated(temporary.path().join("data")), backend).unwrap();
    configure_test_model(&app).await;
    let workspace = app.open_workspace(temporary.path()).await.unwrap();
    let session = app
        .create_session(workspace.id, "Responsive configuration")
        .await
        .unwrap();
    let receipt = session.submit(SubmitInput::new("start")).await.unwrap();
    assert_completed(wait_for_input(&session, receipt.input).await);
    let select = |model: &str| {
        ConfigChange::Model(Some(
            ModelSelection::new(ProfileId::new("test").unwrap(), model).unwrap(),
        ))
    };
    for round in 0..3 {
        tokio::time::timeout(
            Duration::from_secs(1),
            app.update_config(ConfigScope::Session(session.id()), select("blocked")),
        )
        .await
        .expect("saving must not wait for connection")
        .unwrap();
        tokio::time::timeout(Duration::from_secs(1), started.notified())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), session.save_draft("still editable"))
            .await
            .expect("draft command should stay responsive")
            .unwrap();
        match round {
            0 => {
                tokio::time::timeout(Duration::from_secs(1), session.stop())
                    .await
                    .unwrap()
                    .unwrap();
            }
            1 => {
                app.update_config(ConfigScope::Session(session.id()), select("latest"))
                    .await
                    .unwrap();
                wait_for_config(&app, &session).await;
                assert_eq!(
                    app.resolved_config(session.id())
                        .await
                        .unwrap()
                        .running
                        .unwrap()
                        .worker
                        .selection
                        .model,
                    "latest"
                );
                let receipt = session.submit(SubmitInput::new("continue")).await.unwrap();
                assert_completed(wait_for_input(&session, receipt.input).await);
            }
            _ => {
                tokio::time::timeout(Duration::from_secs(1), app.shutdown())
                    .await
                    .unwrap()
                    .unwrap();
            }
        }
        assert_eq!(cancelled.load(Ordering::SeqCst), round + 1);
    }
}

async fn workspace_write_session() -> (tempfile::TempDir, App, WorkspaceInfo, Session) {
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
            shutdown_grace: Duration::from_millis(10),
            ..crate::config::RuntimeSettings::default().limits
        })),
    )
    .await
    .unwrap();
    let session = app
        .create_session(workspace.id, "Write race")
        .await
        .unwrap();
    (temporary, app, workspace, session)
}

async fn wait_for_input(session: &Session, input: InputId) -> InputState {
    let mut view = session.observe();
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut cursor = SessionSeq(0);
        loop {
            let page = session.history(cursor, 256).await.unwrap();
            if let Some(state) = page.items.iter().find_map(|entry| match &entry.event {
                SessionEvent::InputFinished {
                    runtime,
                    input: finished,
                    outcome,
                } if *finished == input => Some(InputState::Finished {
                    runtime: *runtime,
                    outcome: outcome.clone(),
                }),
                SessionEvent::InputRejected {
                    input: rejected,
                    message,
                } if *rejected == input => Some(InputState::Rejected {
                    message: message.clone(),
                }),
                SessionEvent::InputCancelled { input: cancelled } if *cancelled == input => {
                    Some(InputState::Cancelled)
                }
                SessionEvent::Interrupted { runtime, inputs } if inputs.contains(&input) => {
                    Some(InputState::Interrupted { runtime: *runtime })
                }
                _ => None,
            }) {
                return state;
            }
            cursor = page.next_cursor;
            if !page.has_more {
                view.changed().await.unwrap();
            }
        }
    })
    .await
    .expect("input did not reach a durable terminal state")
}

fn assert_completed(state: InputState) {
    assert!(
        matches!(
            state,
            InputState::Finished {
                outcome: InputOutcome::Completed,
                ..
            }
        ),
        "expected successful completion, got {state:?}"
    );
}

async fn assert_pending(mut future: Pin<&mut impl Future>) {
    poll_fn(|context| {
        assert!(
            future.as_mut().poll(context).is_pending(),
            "operation crossed a closed gate"
        );
        Poll::Ready(())
    })
    .await;
}

async fn wait_for_runtime_detached(session: &Session) {
    const FINAL_WRITE_BOUNDARY_DEADLINE: Duration = Duration::from_secs(10);

    let mut view = session.observe();
    tokio::time::timeout(FINAL_WRITE_BOUNDARY_DEADLINE, async {
        loop {
            if matches!(view.borrow().runtime, RuntimeState::Detached) {
                return;
            }
            view.changed().await.unwrap();
        }
    })
    .await
    .expect("runtime did not reach the final write synchronization boundary");
}

async fn wait_for_input_state(
    session: &Session,
    input: InputId,
    matches: impl Fn(&InputState) -> bool,
) -> InputView {
    wait_for_input_state_with_timeout(session, input, matches, Duration::from_secs(3)).await
}

async fn wait_for_input_state_with_timeout(
    session: &Session,
    input: InputId,
    matches: impl Fn(&InputState) -> bool,
    timeout: Duration,
) -> InputView {
    let mut view = session.observe();
    tokio::time::timeout(timeout, async {
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
    tokio::time::timeout(Duration::from_secs(10), async {
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
async fn workspace_overview_reads_durable_summaries_and_attention_without_a_session_lease() {
    let (_temporary, app, workspace) = configured_app().await;
    let store = app.test_store();
    let saved = store
        .create_session(workspace.id, "Overview".into())
        .unwrap();
    let initial = app.workspace_overview(workspace.id).await.unwrap();
    assert!(!initial.attention_projection_pending);
    assert!(initial.attention.is_empty());
    let initial_summary = &initial.sessions[0];
    assert!(initial_summary.created_at > 0);
    assert_eq!(initial_summary.message_count, 0);
    assert_eq!(initial_summary.latest_reply_preview, None);
    assert!(!initial_summary.projection_pending);
    store.save_draft(saved.info.id, "继续检查".into()).unwrap();

    let desired = app
        .resolved_config(saved.info.id)
        .await
        .unwrap()
        .desired
        .unwrap();
    let runtime = RuntimeId::new();
    store
        .start_runtime(
            saved.info.id,
            SavedRuntime {
                id: runtime,
                config: desired,
            },
            0,
        )
        .unwrap();

    let waiting = store
        .accept_input(saved.info.id, &SubmitInput::new("需要目标"))
        .unwrap()
        .0;
    let question = QuestionId {
        runtime,
        record: 12,
        reply_to: waiting.input,
    };
    store
        .update_input(
            saved.info.id,
            waiting.input,
            InputState::WaitingForUser {
                runtime,
                question,
                text: "请选择目标".into(),
            },
            Some(SessionEvent::QuestionAsked {
                question,
                inputs: vec![waiting.input],
                text: "请选择目标".into(),
            }),
        )
        .unwrap();

    let related = store
        .accept_input(saved.info.id, &SubmitInput::new("同一问题的另一项输入"))
        .unwrap()
        .0;
    store
        .update_input(
            saved.info.id,
            related.input,
            InputState::WaitingForUser {
                runtime,
                question: QuestionId {
                    reply_to: related.input,
                    ..question
                },
                text: "请选择目标".into(),
            },
            None,
        )
        .unwrap();

    let interrupted = store
        .accept_input(saved.info.id, &SubmitInput::new("被中断的工作"))
        .unwrap()
        .0;
    store
        .update_input(
            saved.info.id,
            interrupted.input,
            InputState::Interrupted { runtime },
            Some(SessionEvent::Interrupted {
                runtime,
                inputs: vec![interrupted.input],
            }),
        )
        .unwrap();

    let call = CallRef { runtime, id: 9 };
    assert!(
        store
            .begin_write(
                workspace.id,
                saved.info.id,
                call,
                "workspace_write",
                json!({ "path": "result.txt" }),
            )
            .unwrap()
    );

    let _lease = store.claim_session(saved.info.id).unwrap();
    assert!(matches!(
        app.session(saved.info.id).await,
        Err(Error::SessionBusy(id)) if id == saved.info.id
    ));
    let overview = app.workspace_overview(workspace.id).await.unwrap();

    assert_eq!(overview.workspace, workspace);
    assert_eq!(overview.sessions.len(), 1);
    let summary = &overview.sessions[0];
    assert_eq!(summary.session, saved.info);
    assert_eq!(summary.created_at, initial_summary.created_at);
    assert_eq!(summary.message_count, 3);
    assert_eq!(summary.latest_reply_preview, None);
    assert!(!summary.projection_pending);
    assert!(summary.has_draft);
    assert_eq!(summary.draft_bytes, "继续检查".len() as u64);
    assert_eq!(summary.persisted_runtime, Some(runtime));
    assert!(summary.history_through.0 >= 4);
    assert_eq!(overview.unresolved_writes.len(), 1);
    assert!(!overview.attention_projection_pending);
    assert!(overview.attention.iter().any(|item| matches!(
        item,
        AttentionItem::WaitingForUser {
            session,
            inputs,
            question: saved_question,
            text,
            ..
        } if *session == saved.info.id
            && inputs == &[waiting.input, related.input]
            && *saved_question == question
            && text == "请选择目标"
    )));
    assert!(overview.attention.iter().all(|item| !matches!(
        item,
        AttentionItem::WaitingForUser { inputs, .. } if inputs.contains(&interrupted.input)
    )));
    assert!(overview.attention.iter().any(|item| matches!(
        item,
        AttentionItem::UnresolvedWrite {
            session,
            call: saved_call,
            status: UnresolvedWriteStatus::Pending,
        } if *session == saved.info.id && *saved_call == call
    )));

    store
        .finish_write(
            workspace.id,
            call,
            ToolOutcome {
                result: Err(CallError::failed("result unknown")),
                external_effect: ExternalEffect::Unknown,
            },
        )
        .unwrap();
    let finished = app.workspace_overview(workspace.id).await.unwrap();
    assert!(finished.attention.iter().any(|item| matches!(
        item,
        AttentionItem::UnresolvedWrite {
            call: saved_call,
            status: UnresolvedWriteStatus::Finished,
            ..
        } if *saved_call == call
    )));
    assert!(matches!(
        store
            .resolve_write(
                workspace.id,
                saved.info.id,
                call,
                WriteResolution {
                    external_effect: ExternalEffect::None,
                    evidence: "verified".into(),
                },
            )
            .unwrap(),
        ResolveWriteResult::Applied
    ));
    let resolved = app.workspace_overview(workspace.id).await.unwrap();
    assert!(resolved.attention.iter().all(|item| !matches!(
        item,
        AttentionItem::UnresolvedWrite { call: saved_call, .. } if *saved_call == call
    )));
    assert!(resolved.unresolved_writes.is_empty());
}

#[tokio::test]
async fn begin_write_rejects_a_workspace_that_does_not_own_the_session() {
    let (temporary, app, first_workspace) = configured_app().await;
    let second_root = temporary.path().join("second-workspace");
    std::fs::create_dir(&second_root).unwrap();
    let second_workspace = app.open_workspace(second_root).await.unwrap();
    let store = app.test_store();
    let session = store
        .create_session(second_workspace.id, "Second workspace".into())
        .unwrap();
    let call = CallRef {
        runtime: RuntimeId::new(),
        id: 1,
    };

    assert!(matches!(
        store.begin_write(
            first_workspace.id,
            session.info.id,
            call,
            "workspace_write",
            json!({}),
        ),
        Err(crate::storage::StoreError::Corrupt {
            message: "write attempt workspace does not match its session",
        })
    ));
    assert!(
        app.workspace_overview(first_workspace.id)
            .await
            .unwrap()
            .unresolved_writes
            .is_empty()
    );
    assert!(
        app.workspace_overview(second_workspace.id)
            .await
            .unwrap()
            .unresolved_writes
            .is_empty()
    );
}

fn run_git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn workspace_changes_are_paginated_typed_and_strictly_bounded() {
    let (_temporary, app, workspace) = configured_app().await;
    let non_git = app.workspace_changes(workspace.id, None, 16).await.unwrap();
    assert_eq!(non_git.baseline, WorkspaceBaseline::NotGit);
    assert!(non_git.files.is_empty());

    run_git(&workspace.root, &["init", "--quiet"]);
    run_git(
        &workspace.root,
        &["config", "user.email", "bone@example.invalid"],
    );
    run_git(&workspace.root, &["config", "user.name", "BONE test"]);
    std::fs::write(workspace.root.join("tracked.txt"), "old\n").unwrap();
    std::fs::write(workspace.root.join("binary.bin"), b"old\0binary").unwrap();
    run_git(&workspace.root, &["add", "tracked.txt", "binary.bin"]);
    run_git(&workspace.root, &["commit", "--quiet", "-m", "baseline"]);

    std::fs::write(
        workspace.root.join("tracked.txt"),
        format!("new\n{}", "long changed line\n".repeat(512)),
    )
    .unwrap();
    std::fs::write(workspace.root.join("untracked.txt"), "new file\n").unwrap();
    std::fs::write(workspace.root.join("binary.bin"), b"new\0binary").unwrap();

    let mut cursor = None;
    let mut files = Vec::new();
    loop {
        let page = app
            .workspace_changes(workspace.id, cursor, 1)
            .await
            .unwrap();
        assert!(matches!(
            page.baseline,
            WorkspaceBaseline::Git { head: Some(_) }
        ));
        files.extend(page.files);
        let Some(next) = page.next_cursor else { break };
        cursor = Some(next);
    }
    assert_eq!(files.len(), 3);
    assert!(files.iter().any(|file| {
        file.path == "untracked.txt"
            && !file.tracked
            && file.index == GitFileState::Untracked
            && file.worktree == GitFileState::Untracked
    }));
    assert!(files.iter().any(|file| {
        file.path == "tracked.txt" && file.tracked && file.worktree == GitFileState::Modified
    }));

    let diff = app
        .workspace_file(
            workspace.id,
            "tracked.txt",
            WorkspaceFileSource::DiffAgainstHead,
            128,
        )
        .await
        .unwrap();
    assert_eq!(diff.media, WorkspaceFileMedia::Text);
    assert!(diff.truncated);
    assert!(diff.bytes_read <= 129);
    assert!(diff.text.as_ref().unwrap().len() <= 128);

    let mut continuation = None;
    let mut complete_diff = String::new();
    loop {
        let page = app
            .workspace_file_page(
                workspace.id,
                "tracked.txt",
                WorkspaceFileSource::DiffAgainstHead,
                continuation,
                128,
            )
            .await
            .unwrap();
        complete_diff.push_str(page.text.as_deref().unwrap());
        let Some(next) = page.next_cursor else { break };
        continuation = Some(next);
    }
    assert!(complete_diff.contains("+long changed line"));

    let first = app
        .workspace_file_page(
            workspace.id,
            "tracked.txt",
            WorkspaceFileSource::WorkingTree,
            None,
            64,
        )
        .await
        .unwrap();
    let continuation = first.next_cursor.unwrap();
    std::fs::write(
        workspace.root.join("tracked.txt"),
        "changed between pages\n",
    )
    .unwrap();
    assert!(
        app.workspace_file_page(
            workspace.id,
            "tracked.txt",
            WorkspaceFileSource::WorkingTree,
            Some(continuation),
            64,
        )
        .await
        .is_err()
    );

    let untracked = app
        .workspace_file(
            workspace.id,
            "untracked.txt",
            WorkspaceFileSource::WorkingTree,
            64,
        )
        .await
        .unwrap();
    assert_eq!(untracked.media, WorkspaceFileMedia::Text);
    assert_eq!(untracked.text.as_deref(), Some("new file\n"));
    assert!(!untracked.truncated);

    let binary = app
        .workspace_file(
            workspace.id,
            "binary.bin",
            WorkspaceFileSource::DiffAgainstHead,
            64,
        )
        .await
        .unwrap();
    assert_eq!(binary.media, WorkspaceFileMedia::Binary);
    assert!(binary.text.is_none());

    for unsafe_path in ["../outside", "/absolute", "control\u{1b}path", "dir\\file"] {
        assert!(
            app.workspace_file(
                workspace.id,
                unsafe_path,
                WorkspaceFileSource::WorkingTree,
                64,
            )
            .await
            .is_err()
        );
    }
    assert!(
        app.workspace_file(
            workspace.id,
            ".git/config",
            WorkspaceFileSource::WorkingTree,
            64,
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn workspace_changes_reject_a_control_character_in_a_git_path() {
    let (_temporary, app, workspace) = configured_app().await;
    run_git(&workspace.root, &["init", "--quiet"]);
    std::fs::write(workspace.root.join("bad\u{1b}path"), "unsafe").unwrap();
    assert!(app.workspace_changes(workspace.id, None, 16).await.is_err());
}

#[tokio::test]
async fn legacy_attention_backfill_is_bounded_incremental_and_resumable() {
    let (_temporary, app, workspace) = configured_app().await;
    let store = app.test_store();
    let session = store
        .create_session(workspace.id, "Legacy attention".into())
        .unwrap();
    let waiting = store
        .accept_input(session.info.id, &SubmitInput::new("old question"))
        .unwrap()
        .0;
    let runtime = RuntimeId::new();
    let question = QuestionId {
        runtime,
        record: 77,
        reply_to: waiting.input,
    };
    store
        .update_input(
            session.info.id,
            waiting.input,
            InputState::WaitingForUser {
                runtime,
                question,
                text: "legacy question".into(),
            },
            None,
        )
        .unwrap();
    for index in 0..512 {
        store
            .accept_input(
                session.info.id,
                &SubmitInput::new(format!("ordinary input {index}")),
            )
            .unwrap();
    }
    let call = CallRef { runtime, id: 78 };
    assert!(
        store
            .begin_write(
                workspace.id,
                session.info.id,
                call,
                "legacy-write",
                json!({"path": "legacy"}),
            )
            .unwrap()
    );
    store.reset_attention_projection_for_test().unwrap();
    let decoded_before = store.attention_backfill_decode_count();

    let first = app.workspace_overview(workspace.id).await.unwrap();
    assert!(first.attention_projection_pending);
    assert_eq!(
        store.attention_backfill_decode_count() - decoded_before,
        130,
        "one overview decodes 128 inputs plus one boundary proof and one write"
    );
    assert_eq!(first.unresolved_writes.len(), 1);
    assert!(first.attention.iter().all(|item| !matches!(
        item,
        AttentionItem::WaitingForUser { question: saved, .. } if *saved == question
    )));

    let mut recovered = false;
    for _ in 0..5 {
        let overview = app.workspace_overview(workspace.id).await.unwrap();
        if overview.attention.iter().any(|item| {
            matches!(
                item,
                AttentionItem::WaitingForUser { question: saved, .. } if *saved == question
            )
        }) {
            recovered = true;
            assert!(!overview.attention_projection_pending);
            break;
        }
    }
    assert!(
        recovered,
        "bounded refreshes must finish legacy attention backfill"
    );
    let decoded_after_backfill = store.attention_backfill_decode_count();
    app.workspace_overview(workspace.id).await.unwrap();
    assert_eq!(
        store.attention_backfill_decode_count(),
        decoded_after_backfill,
        "a completed projection must not rescan business documents"
    );

    store
        .update_input(session.info.id, waiting.input, InputState::Cancelled, None)
        .unwrap();
    let updated = app.workspace_overview(workspace.id).await.unwrap();
    assert!(updated.attention.iter().all(|item| !matches!(
        item,
        AttentionItem::WaitingForUser { question: saved, .. } if *saved == question
    )));
}

#[tokio::test]
async fn workspace_overview_rejects_an_unknown_workspace() {
    let (_temporary, app, _workspace) = configured_app().await;
    assert!(matches!(
        app.workspace_overview(WorkspaceId::new()).await,
        Err(Error::WorkspaceNotFound)
    ));
}

#[tokio::test]
async fn workspace_config_resolves_without_opening_a_session() {
    let (_temporary, app, workspace) = configured_app().await;
    let resolved = app.resolved_workspace_config(workspace.id).await.unwrap();
    assert!(resolved.desired.is_ok());
    assert_eq!(resolved.running, None);
    assert_eq!(app.test_open_session_count().await, 0);
    assert!(matches!(
        app.resolved_workspace_config(WorkspaceId::new()).await,
        Err(Error::WorkspaceNotFound)
    ));
}

#[tokio::test]
async fn last_active_session_is_durable_and_workspace_scoped() {
    let (_temporary, app, workspace) = configured_app().await;
    let other_root = workspace.root.join("other");
    std::fs::create_dir(&other_root).unwrap();
    let other_workspace = app.open_workspace(other_root).await.unwrap();
    let session = app.create_session(workspace.id, "Selected").await.unwrap();
    let other = app
        .create_session(other_workspace.id, "Other")
        .await
        .unwrap();

    assert_eq!(app.last_active_session(workspace.id).await.unwrap(), None);
    app.set_last_active_session(workspace.id, session.id())
        .await
        .unwrap();
    assert_eq!(
        app.last_active_session(workspace.id).await.unwrap(),
        Some(session.id())
    );
    assert!(matches!(
        app.set_last_active_session(workspace.id, other.id()).await,
        Err(Error::SessionNotFound)
    ));
    assert!(matches!(
        app.set_last_active_session(workspace.id, SessionId::new())
            .await,
        Err(Error::SessionNotFound)
    ));
    assert!(matches!(
        app.last_active_session(WorkspaceId::new()).await,
        Err(Error::WorkspaceNotFound)
    ));
}

#[tokio::test]
async fn create_session_request_is_durable_and_idempotent() {
    let (temporary, app, workspace) = configured_app().await;
    let request_id = RequestId::new();
    let request = CreateSessionRequest {
        request_id,
        workspace: workspace.id,
        title: "New session".into(),
        provisional: true,
    };
    let first = app
        .create_session_idempotent(request.clone())
        .await
        .unwrap();
    let retry = app
        .create_session_idempotent(request.clone())
        .await
        .unwrap();
    assert_eq!(retry.id(), first.id());
    assert_eq!(
        app.workspace_overview(workspace.id)
            .await
            .unwrap()
            .sessions
            .len(),
        1
    );
    assert!(matches!(
        app.create_session_idempotent(CreateSessionRequest {
            title: "Different".into(),
            ..request.clone()
        })
        .await,
        Err(Error::RequestConflict)
    ));
    assert!(matches!(
        app.create_session_idempotent(CreateSessionRequest {
            provisional: false,
            ..request.clone()
        })
        .await,
        Err(Error::RequestConflict)
    ));

    let id = first.id();
    app.shutdown().await.unwrap();
    let reopened = App::with_ports(
        AppOptions::isolated(temporary.path().join("data")),
        Arc::new(CompletingModel),
        Vec::new(),
    )
    .await
    .unwrap();
    let retried_after_restart = reopened.create_session_idempotent(request).await.unwrap();
    assert_eq!(retried_after_restart.id(), id);
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn automatic_title_only_replaces_a_provisional_title_once() {
    let (_temporary, app, workspace) = configured_app().await;
    let session = app
        .create_session_idempotent(CreateSessionRequest {
            request_id: RequestId::new(),
            workspace: workspace.id,
            title: "New session".into(),
            provisional: true,
        })
        .await
        .unwrap();
    assert!(!session.title_from_first_input(" \n\t ").await.unwrap());
    assert!(session
        .title_from_first_input(
            "  Explain   why this behavior is deterministic and safely handles a very long first input  ",
        )
        .await
        .unwrap());
    assert_eq!(
        session.snapshot().await.unwrap().session.title,
        "Explain why this behavior is deterministic and safely handle…"
    );
    assert!(
        !session
            .title_from_first_input("Second input")
            .await
            .unwrap()
    );

    let unicode = app
        .create_session_idempotent(CreateSessionRequest {
            request_id: RequestId::new(),
            workspace: workspace.id,
            title: "New session".into(),
            provisional: true,
        })
        .await
        .unwrap();
    assert!(
        unicode
            .title_from_first_input(&"😀".repeat(100))
            .await
            .unwrap()
    );
    assert!(unicode.snapshot().await.unwrap().session.title.len() <= 200);

    let manually_named = app
        .create_session_idempotent(CreateSessionRequest {
            request_id: RequestId::new(),
            workspace: workspace.id,
            title: "New session".into(),
            provisional: true,
        })
        .await
        .unwrap();
    manually_named.rename("My chosen title").await.unwrap();
    assert!(
        !manually_named
            .title_from_first_input("This must not win")
            .await
            .unwrap()
    );
    assert_eq!(
        manually_named.snapshot().await.unwrap().session.title,
        "My chosen title"
    );

    let explicit_request = CreateSessionRequest {
        request_id: RequestId::new(),
        workspace: workspace.id,
        title: "Explicit title".into(),
        provisional: false,
    };
    let explicitly_named = app
        .create_session_idempotent(explicit_request.clone())
        .await
        .unwrap();
    assert_eq!(
        app.create_session_idempotent(explicit_request)
            .await
            .unwrap()
            .id(),
        explicitly_named.id()
    );
    assert!(
        !explicitly_named
            .title_from_first_input("This must also not win")
            .await
            .unwrap()
    );
}

#[test]
fn recent_history_pages_backward_from_a_stable_snapshot() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    let store = DataStore::open(temporary.path().join("data")).unwrap();
    let workspace = store.workspace(&workspace_root).unwrap();
    let session = store
        .create_session(workspace.id, "Recent history".into())
        .unwrap();

    let empty = store.recent_history(session.info.id, None, 2).unwrap();
    assert!(empty.items.is_empty());
    assert_eq!(empty.snapshot_through, SessionSeq(0));
    assert!(empty.older_cursor.is_none());

    for index in 1..=5 {
        store
            .accept_input(session.info.id, &SubmitInput::new(format!("input {index}")))
            .unwrap();
    }
    let newest = store.recent_history(session.info.id, None, 2).unwrap();
    assert_eq!(
        newest
            .items
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<_>>(),
        vec![SessionSeq(4), SessionSeq(5)]
    );
    assert_eq!(newest.snapshot_through, SessionSeq(5));
    let first_cursor = newest.older_cursor.unwrap();
    assert_eq!(first_cursor.before(), SessionSeq(4));
    assert_eq!(first_cursor.snapshot_through(), SessionSeq(5));

    store
        .accept_input(session.info.id, &SubmitInput::new("concurrent append"))
        .unwrap();
    let middle = store
        .recent_history(session.info.id, Some(first_cursor), 2)
        .unwrap();
    assert_eq!(
        middle
            .items
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<_>>(),
        vec![SessionSeq(2), SessionSeq(3)]
    );
    assert_eq!(middle.snapshot_through, SessionSeq(5));
    let oldest = store
        .recent_history(session.info.id, middle.older_cursor, 2)
        .unwrap();
    assert_eq!(
        oldest
            .items
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<_>>(),
        vec![SessionSeq(1)]
    );
    assert!(oldest.older_cursor.is_none());

    let fresh = store.recent_history(session.info.id, None, 2).unwrap();
    assert_eq!(fresh.snapshot_through, SessionSeq(6));
    assert_eq!(
        fresh
            .items
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<_>>(),
        vec![SessionSeq(5), SessionSeq(6)]
    );
}

#[tokio::test]
async fn headless_session_runs_and_persists_public_history() {
    let (_temporary, app, workspace) = configured_app().await;
    let session = app
        .create_session(workspace.id, "Test session")
        .await
        .unwrap();
    let receipt = session.submit(SubmitInput::new("do it")).await.unwrap();

    assert_completed(wait_for_input(&session, receipt.input).await);

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
    let overview = app.workspace_overview(workspace.id).await.unwrap();
    let summary = overview
        .sessions
        .iter()
        .find(|summary| summary.session.id == session.id())
        .unwrap();
    assert!(summary.created_at > 0);
    assert_eq!(summary.message_count, 2);
    assert_eq!(summary.latest_reply_preview.as_deref(), Some("the answer"));
    assert!(!summary.projection_pending);
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn acceptance_is_versioned_idempotent_and_rejection_atomically_creates_rework() {
    let (_temporary, app, workspace) = configured_app().await;
    let session = app
        .create_session(workspace.id, "Acceptance")
        .await
        .unwrap();
    let original = session
        .submit(SubmitInput::new("produce result"))
        .await
        .unwrap();
    assert_completed(wait_for_input(&session, original.input).await);
    let result = app
        .results(session.id(), None, 32)
        .await
        .unwrap()
        .items
        .pop()
        .unwrap()
        .result;

    let accepted = AcceptanceSubmission {
        request_id: AcceptanceRequestId::new(),
        result,
        decision: AcceptanceDecision::Accepted,
        reason: String::new(),
        rework: None,
    };
    let first = session.submit_acceptance(accepted.clone()).await.unwrap();
    let retry = session.submit_acceptance(accepted.clone()).await.unwrap();
    assert_eq!(retry, first);
    assert!(matches!(
        session
            .submit_acceptance(AcceptanceSubmission {
                decision: AcceptanceDecision::PartiallyAccepted,
                reason: "changed".into(),
                ..accepted
            })
            .await,
        Err(Error::RequestConflict)
    ));

    session
        .submit_acceptance(AcceptanceSubmission {
            request_id: AcceptanceRequestId::new(),
            result,
            decision: AcceptanceDecision::AcceptedWithRisk,
            reason: "manual verification remains".into(),
            rework: None,
        })
        .await
        .unwrap();

    // Product pagination counts durable results, not intervening journal
    // events. The most recent result must remain visible after acceptance
    // records are appended above it.
    let result_page_after_acceptance = app.results(session.id(), None, 1).await.unwrap();
    assert_eq!(result_page_after_acceptance.items.len(), 1);
    assert_eq!(result_page_after_acceptance.items[0].result, result);

    let rework = SubmitInput::new("fix the remaining issue");
    let rejected = AcceptanceSubmission {
        request_id: AcceptanceRequestId::new(),
        result,
        decision: AcceptanceDecision::Rejected,
        reason: "require another pass".into(),
        rework: Some(rework.clone()),
    };
    let rejected_receipt = session.submit_acceptance(rejected.clone()).await.unwrap();
    let rework_receipt = rejected_receipt.rework.unwrap();
    assert_eq!(
        session.submit_acceptance(rejected).await.unwrap(),
        rejected_receipt
    );
    assert_completed(wait_for_input(&session, rework_receipt.input).await);

    let recent_records = app.acceptances(result, None, 2).await.unwrap();
    assert_eq!(recent_records.items.len(), 2);
    assert_eq!(
        recent_records.items[0].decision,
        AcceptanceDecision::AcceptedWithRisk
    );
    assert_eq!(
        recent_records.items[1].decision,
        AcceptanceDecision::Rejected
    );
    assert_eq!(recent_records.items[1].rework, Some(rework_receipt));
    let older_records = app
        .acceptances(result, recent_records.older_cursor, 2)
        .await
        .unwrap();
    assert_eq!(older_records.items.len(), 1);
    assert_eq!(older_records.items[0].id, first.id);
    assert!(older_records.older_cursor.is_none());

    let mut result_cursor = None;
    let mut paged_results = Vec::new();
    loop {
        let page = app.results(session.id(), result_cursor, 1).await.unwrap();
        assert!(page.items.len() <= 1);
        paged_results.extend(page.items.into_iter().map(|item| item.result));
        let Some(older) = page.older_cursor else {
            break;
        };
        result_cursor = Some(older);
    }
    assert!(paged_results.contains(&result));

    let history = session.history(SessionSeq(0), 256).await.unwrap();
    assert_eq!(
        history
            .items
            .iter()
            .filter(|entry| matches!(
                &entry.event,
                SessionEvent::InputSubmitted { request_id, .. }
                    if *request_id == rework.request_id
            ))
            .count(),
        1
    );
    assert_eq!(
        history
            .items
            .iter()
            .filter(|entry| matches!(
                entry.event,
                SessionEvent::AcceptanceRecorded { acceptance, .. }
                    if acceptance == rejected_receipt.id
            ))
            .count(),
        1
    );
}

#[tokio::test]
async fn rejected_acceptance_requires_reason_and_rework() {
    let (_temporary, app, workspace) = configured_app().await;
    let session = app
        .create_session(workspace.id, "Validation")
        .await
        .unwrap();
    let result = ResultRef {
        session: session.id(),
        job: JobRef {
            runtime: RuntimeId::new(),
            id: 1,
        },
        version: SessionSeq(1),
    };
    assert!(matches!(
        session
            .submit_acceptance(AcceptanceSubmission {
                request_id: AcceptanceRequestId::new(),
                result,
                decision: AcceptanceDecision::Rejected,
                reason: "missing rework".into(),
                rework: None,
            })
            .await,
        Err(Error::InvalidState(_))
    ));
}

#[tokio::test]
async fn legacy_result_projection_backfill_is_bounded_and_resumable() {
    let (_temporary, app, workspace) = configured_app().await;
    let session = app
        .create_session(workspace.id, "Legacy result projection")
        .await
        .unwrap();
    let submitted = session
        .submit(SubmitInput::new("produce the legacy result"))
        .await
        .unwrap();
    assert_completed(wait_for_input(&session, submitted.input).await);
    let expected = app
        .results(session.id(), None, 1)
        .await
        .unwrap()
        .items
        .pop()
        .unwrap()
        .result;

    let store = app.test_store();
    for index in 0..1_024 {
        store
            .accept_input(
                session.id(),
                &SubmitInput::new(format!("ordinary legacy event {index}")),
            )
            .unwrap();
    }
    store
        .reset_result_projection_for_test(session.id())
        .unwrap();
    let decoded_before = store.result_backfill_decode_count();

    let first = app.results(session.id(), None, 1).await.unwrap();
    assert!(first.items.is_empty());
    assert!(first.projection_pending);
    assert_eq!(
        store.result_backfill_decode_count() - decoded_before,
        257,
        "one query must decode one fixed migration window plus its boundary proof, not the full sparse history"
    );

    let mut found = false;
    for _ in 0..5 {
        let page = app.results(session.id(), None, 1).await.unwrap();
        if page.items.iter().any(|item| item.result == expected) {
            found = true;
            assert!(!page.projection_pending);
            break;
        }
    }
    assert!(
        found,
        "bounded refreshes must eventually finish legacy backfill"
    );
}

#[tokio::test]
async fn result_evidence_is_explicit_paged_limited_private_and_durable() {
    let temporary = tempfile::tempdir().unwrap();
    let data = temporary.path().join("data");
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    let app = App::with_ports(
        AppOptions::isolated(&data),
        Arc::new(EvidenceModel),
        vec![Arc::new(EvidenceTool)],
    )
    .await
    .unwrap();
    configure_test_model(&app).await;
    let workspace = app.open_workspace(&workspace_root).await.unwrap();
    let session = app.create_session(workspace.id, "Evidence").await.unwrap();
    let receipt = session
        .submit(SubmitInput::new("produce an artifact"))
        .await
        .unwrap();
    let input_state = wait_for_input(&session, receipt.input).await;
    if !matches!(
        input_state,
        InputState::Finished {
            outcome: InputOutcome::Completed,
            ..
        }
    ) {
        let history = session.history(SessionSeq(0), 256).await.unwrap();
        panic!("expected evidence completion, got {input_state:?}; history={history:?}");
    }
    let result = app
        .results(session.id(), None, 1)
        .await
        .unwrap()
        .items
        .pop()
        .unwrap()
        .result;

    let artifact = app.result_artifact(result).await.unwrap();
    assert_eq!(artifact.summary, "evidence complete");
    assert_eq!(artifact.evidence_count, 2);

    // Simulate an existing database whose result projection predates the
    // independent evidence-source projection.
    app.test_store()
        .reset_evidence_projection_for_test(session.id())
        .unwrap();
    let first = app.result_evidence(result, None, 1).await.unwrap();
    assert_eq!(first.items.len(), 1);
    assert!(!first.projection_pending);
    let tool = first.items[0].source;
    assert!(matches!(
        first.items[0].availability,
        EvidenceAvailability::Available {
            kind: EvidenceSourceKind::ToolResult,
            ..
        }
    ));
    let second = app
        .result_evidence(result, first.next_cursor, 1)
        .await
        .unwrap();
    assert!(matches!(
        second.items[0].availability,
        EvidenceAvailability::Private
    ));
    let private = second.items[0].source;
    assert!(second.next_cursor.is_none());

    let mut offset = 0;
    let mut body = String::new();
    loop {
        let page = app.evidence_source(result, tool, offset, 4).await.unwrap();
        assert!(page.text.as_ref().is_none_or(|text| text.len() <= 4));
        body.push_str(page.text.as_deref().unwrap_or_default());
        let Some(next) = page.next_offset else { break };
        assert!(next > offset);
        offset = next;
    }
    assert!(body.contains("产物内容-abcdefgh"));
    assert!(!body.contains("ARGUMENT_SECRET"));

    let private_page = app
        .evidence_source(result, private, 0, usize::MAX)
        .await
        .unwrap();
    assert_eq!(private_page.availability, EvidenceAvailability::Private);
    assert!(private_page.text.is_none());
    assert!(!format!("{private_page:?}").contains("PRIVATE_PLANNING_NOTE"));

    app.test_store()
        .delete_evidence_source_for_test(private)
        .unwrap();
    let after_missing = app.result_evidence(result, None, 8).await.unwrap();
    assert!(matches!(
        after_missing.items[1].availability,
        EvidenceAvailability::Missing
    ));
    assert!(
        app.evidence_source(
            result,
            EvidenceRef {
                session: result.session,
                record: 42,
            },
            0,
            16,
        )
        .await
        .is_err()
    );

    app.shutdown().await.unwrap();
    drop(session);
    drop(app);

    let reopened = App::with_ports(
        AppOptions::isolated(&data),
        Arc::new(EvidenceModel),
        vec![Arc::new(EvidenceTool)],
    )
    .await
    .unwrap();
    let reopened_artifact = reopened.result_artifact(result).await.unwrap();
    assert_eq!(reopened_artifact, artifact);
    let reopened_source = reopened
        .evidence_source(result, tool, 0, 64 * 1024)
        .await
        .unwrap();
    assert!(
        reopened_source
            .text
            .as_deref()
            .unwrap()
            .contains("产物内容-abcdefgh")
    );
    assert!(!format!("{reopened_source:?}").contains("ARGUMENT_SECRET"));
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn evidence_list_does_not_read_large_source_bodies() {
    const SOURCE_COUNT: usize = 32;
    const BODY_BYTES: usize = 1024 * 1024 - 31;

    let (_temporary, app, workspace) = configured_app().await;
    let session = app
        .create_session(workspace.id, "Large evidence metadata")
        .await
        .unwrap();
    let result = ResultRef {
        session: session.id(),
        job: JobRef {
            runtime: RuntimeId::new(),
            id: 7,
        },
        version: SessionSeq(9_999),
    };
    let sources = (0..SOURCE_COUNT)
        .map(|index| {
            (
                EvidenceRef {
                    session: session.id(),
                    record: 10_000 + index as u64,
                },
                crate::persistence::StoredEvidenceSource::Public {
                    kind: EvidenceSourceKind::ToolResult,
                    title: format!("large tool output {index}"),
                    body: "x".repeat(BODY_BYTES),
                },
            )
        })
        .collect();
    app.test_store()
        .seed_result_evidence_for_test(result, sources)
        .unwrap();

    assert_eq!(app.test_store().evidence_body_read_counts(), (0, 0));
    let page = app
        .result_evidence(result, None, SOURCE_COUNT)
        .await
        .unwrap();
    assert_eq!(page.items.len(), SOURCE_COUNT);
    assert!(page.items.iter().all(|item| matches!(
        item.availability,
        EvidenceAvailability::Available {
            kind: EvidenceSourceKind::ToolResult,
            ..
        }
    )));
    assert_eq!(
        app.test_store().evidence_body_read_counts(),
        (0, 0),
        "metadata listing must neither deserialize body chunks nor consult legacy full-body documents"
    );

    let source = page.items[0].source;
    let body_page = app.evidence_source(result, source, 0, 4096).await.unwrap();
    assert_eq!(body_page.text.as_deref().map(str::len), Some(4096));
    assert_eq!(app.test_store().evidence_body_read_counts(), (1, 0));
}

#[tokio::test]
async fn legacy_evidence_metadata_is_backfilled_in_bounded_slices() {
    let (_temporary, app, workspace) = configured_app().await;
    let session = app
        .create_session(workspace.id, "Legacy evidence metadata")
        .await
        .unwrap();
    let result = ResultRef {
        session: session.id(),
        job: JobRef {
            runtime: RuntimeId::new(),
            id: 8,
        },
        version: SessionSeq(10_001),
    };
    let sources = (0..6)
        .map(|index| {
            (
                EvidenceRef {
                    session: session.id(),
                    record: 20_000 + index,
                },
                crate::persistence::StoredEvidenceSource::Public {
                    kind: EvidenceSourceKind::ToolResult,
                    title: format!("legacy source {index}"),
                    body: format!("legacy body {index}"),
                },
            )
        })
        .collect();
    app.test_store()
        .seed_legacy_result_evidence_for_test(result, sources)
        .unwrap();

    let first = app.result_evidence(result, None, 6).await.unwrap();
    assert!(first.projection_pending);
    assert_eq!(
        first
            .items
            .iter()
            .filter(|item| matches!(item.availability, EvidenceAvailability::Available { .. }))
            .count(),
        4
    );
    assert_eq!(app.test_store().evidence_body_read_counts(), (0, 4));

    let second = app.result_evidence(result, None, 6).await.unwrap();
    assert!(!second.projection_pending);
    assert!(
        second
            .items
            .iter()
            .all(|item| matches!(item.availability, EvidenceAvailability::Available { .. }))
    );
    assert_eq!(app.test_store().evidence_body_read_counts(), (0, 6));
}

#[tokio::test]
async fn idle_session_release_closes_external_handles_and_releases_cross_app_lease() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    let data_dir = temporary.path().join("data");
    let app = App::with_model(
        AppOptions::isolated(data_dir.clone()),
        Arc::new(CompletingModel),
    )
    .await
    .unwrap();
    configure_test_model(&app).await;
    let workspace = app.open_workspace(workspace_root).await.unwrap();
    let session = app
        .create_session(workspace.id, "Release idle")
        .await
        .unwrap();
    let held_clone = session.clone();
    let other = App::with_model(AppOptions::isolated(data_dir), Arc::new(CompletingModel))
        .await
        .unwrap();
    assert!(matches!(
        other.session(session.id()).await,
        Err(Error::SessionBusy(id)) if id == session.id()
    ));

    assert_eq!(
        app.release_session(session.id()).await.unwrap(),
        SessionReleaseReceipt {
            session: session.id(),
            status: SessionReleaseStatus::Released,
        }
    );
    assert!(held_clone.actor_closed());
    assert!(matches!(held_clone.snapshot().await, Err(Error::Closed)));
    let reopened_elsewhere = other.session(session.id()).await.unwrap();
    reopened_elsewhere.snapshot().await.unwrap();
    assert_eq!(app.test_open_session_count().await, 0);
    other.shutdown().await.unwrap();
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn release_retains_running_work_then_evicts_after_completion() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    let barrier = Arc::new(ShutdownBarrier::default());
    let app = App::with_ports(
        AppOptions::isolated(temporary.path().join("data")),
        Arc::new(PausableWorkModel {
            barrier: Arc::clone(&barrier),
        }),
        Vec::new(),
    )
    .await
    .unwrap();
    configure_test_model(&app).await;
    let workspace = app.open_workspace(workspace_root).await.unwrap();
    let session = app
        .create_session(workspace.id, "Keep working")
        .await
        .unwrap();
    let input = session.submit(SubmitInput::new("continue")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), barrier.wait_for_started(1))
        .await
        .unwrap();

    assert!(matches!(
        app.release_session(session.id()).await.unwrap().status,
        SessionReleaseStatus::Retained(
            SessionRetentionReason::ActiveWork | SessionRetentionReason::PersistenceInFlight
        )
    ));
    assert!(!session.actor_closed());
    barrier.release.add_permits(1);
    assert_completed(wait_for_input(&session, input.input).await);
    assert_eq!(
        app.release_session(session.id()).await.unwrap().status,
        SessionReleaseStatus::Released
    );
    assert!(session.actor_closed());
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn release_retains_an_idle_session_with_an_unresolved_write() {
    let (_temporary, app, workspace) = configured_app().await;
    let session = app
        .create_session(workspace.id, "Unresolved write")
        .await
        .unwrap();
    let call = CallRef {
        runtime: RuntimeId::new(),
        id: 41,
    };
    assert!(
        app.test_store()
            .begin_write(
                workspace.id,
                session.id(),
                call,
                "write",
                json!({"path": "unknown"}),
            )
            .unwrap()
    );

    assert_eq!(
        app.release_session(session.id()).await.unwrap().status,
        SessionReleaseStatus::Retained(SessionRetentionReason::UnresolvedWrites)
    );
    assert!(!session.actor_closed());
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn release_is_idempotent_and_linearizes_with_open_and_shutdown() {
    let (_temporary, app, workspace) = configured_app().await;
    let session = app
        .create_session(workspace.id, "Release races")
        .await
        .unwrap();
    assert_eq!(
        app.release_session(session.id()).await.unwrap().status,
        SessionReleaseStatus::Released
    );
    assert_eq!(
        app.release_session(session.id()).await.unwrap().status,
        SessionReleaseStatus::NotOpen
    );
    let reopened = app.session(session.id()).await.unwrap();
    let release_app = app.clone();
    let shutdown_app = app.clone();
    let id = session.id();
    let (release, shutdown) = tokio::join!(
        async move { release_app.release_session(id).await },
        async move { shutdown_app.shutdown().await }
    );
    shutdown.unwrap();
    match release {
        Ok(receipt) => assert!(matches!(
            receipt.status,
            SessionReleaseStatus::Released | SessionReleaseStatus::NotOpen
        )),
        Err(Error::Closed) => {}
        other => panic!("unexpected release/shutdown race result: {other:?}"),
    }
    assert!(reopened.actor_closed());
}

#[tokio::test]
async fn cancelled_release_still_evicts_the_actor_and_allows_reopen() {
    let (_temporary, app, workspace) = configured_app().await;
    let session = app
        .create_session(workspace.id, "Cancelled release")
        .await
        .unwrap();
    let mut release = Box::pin(app.release_session(session.id()));
    assert_pending(release.as_mut()).await;
    drop(release);

    tokio::time::timeout(Duration::from_secs(3), async {
        while app.test_open_session_count().await != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(session.actor_closed());
    let reopened = app.session(session.id()).await.unwrap();
    reopened.snapshot().await.unwrap();
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn releasing_many_idle_sessions_drops_every_cached_actor_and_lease() {
    let (_temporary, app, workspace) = configured_app().await;
    for index in 0..100 {
        let id = app
            .create_session(workspace.id, format!("Idle {index}"))
            .await
            .unwrap()
            .id();
        assert_eq!(
            app.release_session(id).await.unwrap().status,
            SessionReleaseStatus::Released
        );
        let lease = app.test_store().claim_session(id).unwrap();
        drop(lease);
        assert_eq!(app.test_open_session_count().await, 0);
    }
    assert_eq!(app.test_open_session_count().await, 0);
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
        Arc::new(PausedConversationModel {
            first: FirstConversationResult::Fail,
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
    // A maximum-size NUL input expands to 6 MiB of JSON and exercises multiple
    // durable writes. This is a capacity check, not a latency requirement.
    wait_for_input_state_with_timeout(
        &session,
        receipt.input,
        |state| matches!(state, InputState::ConversationFailed { .. }),
        Duration::from_secs(10),
    )
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
        Arc::new(PausedConversationModel {
            first: FirstConversationResult::Clarify,
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
    assert_completed(wait_for_input(&session, input.input).await);
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
async fn retrying_a_conversation_failure_restores_the_input_to_accepted() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    let continue_second = Arc::new(Notify::new());
    let (app, workspace) = app_with_model(
        &temporary.path().join("data"),
        &workspace_root,
        Arc::new(PausedConversationModel {
            first: FirstConversationResult::Fail,
            calls: AtomicUsize::new(0),
            continue_second: Arc::clone(&continue_second),
        }),
    )
    .await;
    let session = app.create_session(workspace.id, "Retry").await.unwrap();
    let receipt = session.submit(SubmitInput::new("try it")).await.unwrap();
    wait_for_input_state(&session, receipt.input, |state| {
        matches!(state, InputState::ConversationFailed { .. })
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
        Arc::new(PausedConversationModel {
            first: FirstConversationResult::Clarify,
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
    assert_completed(wait_for_input(&session, receipt.input).await);
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
    wait_for_config(&app, &session).await;
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
async fn model_change_and_explicit_reload_reconnect_an_equal_runtime() {
    let (_temporary, app, workspace) = configured_app().await;
    let session = app
        .create_session(workspace.id, "Model reconnect")
        .await
        .unwrap();
    let receipt = session.submit(SubmitInput::new("start")).await.unwrap();
    assert_completed(wait_for_input(&session, receipt.input).await);
    let RuntimeState::Running { id, config } = session.snapshot().await.unwrap().runtime else {
        panic!("runtime did not start")
    };
    let selected = ModelSelection::new(ProfileId::new("test").unwrap(), "test-model").unwrap();
    assert_eq!(config.worker.selection, selected);
    assert_eq!(config.coordinator.selection, selected);

    let saved = app
        .update_config(
            ConfigScope::Session(session.id()),
            ConfigChange::Model(Some(selected.clone())),
        )
        .await
        .unwrap();
    assert_eq!(saved.worker, Some(selected.clone()));
    assert_eq!(saved.coordinator, Some(selected));

    session.reload_config().await.unwrap();
    let reconfigurations = session
        .history(SessionSeq(0), 32)
        .await
        .unwrap()
        .items
        .iter()
        .filter(|entry| {
            matches!(
                &entry.event,
                SessionEvent::RuntimeReconfigured { runtime, config }
                    if *runtime == id
                        && config.worker.selection.model == "test-model"
                        && config.coordinator.selection.model == "test-model"
            )
        })
        .count();
    assert!(
        (1..=2).contains(&reconfigurations),
        "queued equal selections may coalesce, but explicit reload must cross the connection boundary"
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn workspace_model_change_does_not_force_a_shadowed_session_to_reconnect() {
    let (_temporary, app, workspace) = configured_app().await;
    let session = app
        .create_session(workspace.id, "Shadowed model")
        .await
        .unwrap();
    let selected = ModelSelection::new(ProfileId::new("test").unwrap(), "test-model").unwrap();
    app.update_config(
        ConfigScope::Session(session.id()),
        ConfigChange::Model(Some(selected)),
    )
    .await
    .unwrap();
    let receipt = session.submit(SubmitInput::new("start")).await.unwrap();
    assert_completed(wait_for_input(&session, receipt.input).await);
    let before = session
        .history(SessionSeq(0), 32)
        .await
        .unwrap()
        .items
        .iter()
        .filter(|entry| matches!(entry.event, SessionEvent::RuntimeReconfigured { .. }))
        .count();

    app.update_config(
        ConfigScope::Workspace(workspace.id),
        ConfigChange::Model(Some(
            ModelSelection::new(ProfileId::new("test").unwrap(), "workspace-model").unwrap(),
        )),
    )
    .await
    .unwrap();

    let after = session
        .history(SessionSeq(0), 32)
        .await
        .unwrap()
        .items
        .iter()
        .filter(|entry| matches!(entry.event, SessionEvent::RuntimeReconfigured { .. }))
        .count();
    assert_eq!(after, before);
    assert_eq!(
        app.resolved_config(session.id())
            .await
            .unwrap()
            .desired
            .unwrap()
            .worker
            .selection
            .model,
        "test-model"
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn model_change_is_validated_before_either_role_is_persisted() {
    let (_temporary, app, _workspace) = configured_app().await;
    let before = app.config(ConfigScope::User).await.unwrap();
    let invalid = ModelSelection {
        profile: ProfileId::new("test").unwrap(),
        model: "invalid\nmodel".into(),
        options: None,
    };

    assert!(matches!(
        app.update_config(ConfigScope::User, ConfigChange::Model(Some(invalid)))
            .await,
        Err(Error::InvalidState(_))
    ));
    assert_eq!(app.config(ConfigScope::User).await.unwrap(), before);
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn chatgpt_model_selection_is_saved_without_credentials() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    let credentials =
        crate::credentials::ChatGptCredentials::at(temporary.path().join("chatgpt-credentials"))
            .unwrap();
    let providers = ProviderConnector::with_chatgpt_credentials(credentials.clone());
    let app = App::with_provider_connector(
        AppOptions::isolated(temporary.path().join("data")),
        providers,
    )
    .await
    .unwrap();
    let workspace = app.open_workspace(workspace_root).await.unwrap();
    let scope = ConfigScope::Workspace(workspace.id);

    let old_worker = ModelSelection::new(ProfileId::chatgpt(), "old-worker").unwrap();
    let old_coordinator = ModelSelection::new(ProfileId::chatgpt(), "old-coordinator").unwrap();
    app.update_config(scope, ConfigChange::Worker(Some(old_worker.clone())))
        .await
        .unwrap();
    app.update_config(
        scope,
        ConfigChange::Coordinator(Some(old_coordinator.clone())),
    )
    .await
    .unwrap();
    let selected = ModelSelection::new(ProfileId::chatgpt(), "gpt-5.4").unwrap();
    let saved = app
        .update_config(scope, ConfigChange::Model(Some(selected.clone())))
        .await
        .unwrap();
    assert_eq!(saved.worker, Some(selected.clone()));
    assert_eq!(saved.coordinator, Some(selected));
    assert_eq!(app.config(scope).await.unwrap(), saved);
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
    assert_completed(wait_for_input(&first, first_input.input).await);
    assert_completed(wait_for_input(&second, second_input.input).await);

    let limits = AgentLimits {
        background_workers: 5,
        ..crate::config::RuntimeSettings::default().limits
    };
    app.update_config(
        ConfigScope::Workspace(first_workspace.id),
        ConfigChange::Limits(Some(limits.clone())),
    )
    .await
    .unwrap();

    wait_for_config(&app, &first).await;

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
        crate::config::RuntimeSettings::default().limits
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn profile_changes_reconfigure_sessions_that_use_the_profile() {
    let (_temporary, app, workspace) = configured_app().await;
    let session = app.create_session(workspace.id, "Profile").await.unwrap();
    let input = session.submit(SubmitInput::new("start")).await.unwrap();
    assert_completed(wait_for_input(&session, input.input).await);
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

    wait_for_config(&app, &session).await;
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
    assert_completed(wait_for_input(&session, input.input).await);
    let running = app
        .resolved_config(session.id())
        .await
        .unwrap()
        .running
        .unwrap();

    app.update_config(ConfigScope::User, ConfigChange::Worker(None))
        .await
        .unwrap();
    assert!(matches!(
        session.reload_config().await,
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
    assert_completed(wait_for_input(&session, blocked.input).await);
    assert_eq!(session.snapshot().await.unwrap().problem, None);
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn login_recovers_sessions_after_credentials_are_repaired() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    let credentials =
        crate::credentials::ChatGptCredentials::at(temporary.path().join("chatgpt-credentials"))
            .unwrap();
    let providers = ProviderConnector::with_chatgpt_credentials(credentials.clone());
    let app = App::with_provider_connector(
        AppOptions::isolated(temporary.path().join("data")),
        providers,
    )
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
    crate::safe_file::atomic_write_private(
        &credentials.auth_file().unwrap(),
        br#"{"access_token":"offline-test-token","expires_at":4102444800,"account_id":"offline-account"}"#,
    )
    .unwrap();
    let login = app.login(profile_id.clone()).await.unwrap();
    let mut login_states = login.observe();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match login_states.borrow().as_ref() {
                LoginState::Succeeded => return,
                LoginState::Failed { message } => panic!("login failed: {message}"),
                _ => {}
            }
            login_states.changed().await.unwrap();
        }
    })
    .await
    .expect("login should refresh blocked sessions");

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
        "login should resume the unchanged desired configuration: {:?}",
        session.snapshot().await.unwrap()
    );

    assert_eq!(
        app.resolved_config(session.id()).await.unwrap().running,
        Some(desired)
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_rejected_agent_limit_change_keeps_the_same_job_running() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    let barrier = Arc::new(ShutdownBarrier::default());
    let app = App::with_ports(
        AppOptions::isolated(temporary.path().join("data")),
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
                context_bytes: 0,
                ..crate::config::RuntimeSettings::default().limits
            })),
        )
        .await,
        Err(Error::InvalidState(_))
    ));
    assert_eq!(session.snapshot().await.unwrap().jobs[0].id, job);

    barrier.release.add_permits(1);
    assert_completed(wait_for_input(&session, input.input).await);
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
        Arc::new(CompletingModel),
    )
    .await;
    let session = app.create_session(workspace.id, "Restart").await.unwrap();
    let first = session.submit(SubmitInput::new("start")).await.unwrap();
    assert_completed(wait_for_input(&session, first.input).await);

    std::fs::remove_dir(&workspace_root).unwrap();
    let result = app
        .update_config(
            ConfigScope::Session(session.id()),
            ConfigChange::Tools(Some(ToolLimits {
                default_bash_timeout: Duration::from_secs(1),
                max_bash_timeout: Duration::from_secs(1),
                ..ToolLimits::default()
            })),
        )
        .await;
    result.unwrap();
    assert!(matches!(
        session.reload_config().await,
        Err(Error::Tools(_))
    ));
    std::fs::create_dir(&workspace_root).unwrap();

    session.close_runtime().await.unwrap();
    let second = session.submit(SubmitInput::new("restart")).await.unwrap();
    assert_completed(wait_for_input(&session, second.input).await);
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
                0,
            )
            .unwrap();
        session.info.id
    };
    let app = App::with_ports(
        AppOptions::isolated(data),
        Arc::new(CompletingModel),
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
    assert_completed(wait_for_input(&session, receipt.input).await);
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
        AppOptions::isolated(temporary.path().join("data")),
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
            ..crate::config::RuntimeSettings::default().limits
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
        AppOptions::isolated(temporary.path().join("data")),
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
    assert_completed(wait_for_input(&idle, idle_input.input).await);

    let closing_task = {
        let closing = closing.clone();
        tokio::spawn(async move { closing.close_runtime().await })
    };
    tokio::time::timeout(Duration::from_secs(3), barrier.wait_for_cancelled(1))
        .await
        .unwrap();

    let limits = AgentLimits {
        background_workers: 5,
        ..crate::config::RuntimeSettings::default().limits
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
    tokio::time::timeout(Duration::from_secs(1), updating)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let resolved = tokio::time::timeout(Duration::from_secs(1), app.resolved_config(idle.id()))
        .await
        .unwrap()
        .unwrap();

    barrier.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(3), closing_task)
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
    let (_temporary, app, workspace, session) = workspace_write_session().await;
    let gate = app.test_write_gate(workspace.id).await;
    let guard = gate.lock.lock().await;
    session.submit(SubmitInput::new("write")).await.unwrap();
    let mut view = session.observe();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if view.borrow().activity.iter().any(|activity| {
                matches!(
                    &activity.kind,
                    ActivityKind::Tool { name, .. } if name == "apply_patch"
                )
            }) {
                break;
            }
            view.changed().await.unwrap();
        }
    })
    .await
    .unwrap();

    let mut shutdown = Box::pin(app.shutdown());
    assert_pending(shutdown.as_mut()).await;
    wait_for_runtime_detached(&session).await;
    assert_pending(shutdown.as_mut()).await;
    drop(guard);
    let report = tokio::time::timeout(Duration::from_secs(3), shutdown)
        .await
        .unwrap()
        .unwrap();
    assert!(report.unresolved_writes.is_empty());
    assert!(!workspace.root.join("result.txt").exists());
}

#[tokio::test]
async fn close_runtime_linearizes_with_a_write_waiting_to_record_its_intent() {
    let (_temporary, app, workspace, session) = workspace_write_session().await;
    let gate = app.test_write_gate(workspace.id).await;
    let guard = gate.lock.lock().await;
    session.submit(SubmitInput::new("write")).await.unwrap();
    let mut view = session.observe();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if view.borrow().activity.iter().any(|activity| {
                matches!(
                    &activity.kind,
                    ActivityKind::Tool { name, .. } if name == "apply_patch"
                )
            }) {
                break;
            }
            view.changed().await.unwrap();
        }
    })
    .await
    .unwrap();

    let mut close = Box::pin(session.close_runtime());
    assert_pending(close.as_mut()).await;
    wait_for_runtime_detached(&session).await;
    assert_pending(close.as_mut()).await;
    drop(guard);
    let report = tokio::time::timeout(Duration::from_secs(3), close)
        .await
        .unwrap()
        .unwrap();
    assert!(report.unresolved_writes.is_empty());
    assert!(!workspace.root.join("result.txt").exists());
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
            shutdown_grace: Duration::from_millis(10),
            ..crate::config::RuntimeSettings::default().limits
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

    let contender = App::with_model(AppOptions::isolated(&data), Arc::new(CompletingModel))
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
struct RecoveryWriteModel(std::sync::atomic::AtomicBool);

#[cfg(unix)]
impl ModelPort for RecoveryWriteModel {
    fn converse(
        &self,
        input: ConversationInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<ConversationStep, CallError>> {
        Box::pin(async move { Ok(converse_for_work(&input, "write")) })
    }
    fn work(
        &self,
        input: WorkInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<WorkProposal, CallError>> {
        let saw_result = input.records.iter().any(|record| {
            matches!(
                serde_json::from_str::<RecordBody>(&record.content),
                Ok(RecordBody::ToolFinished { .. })
            )
        });
        let step = if saw_result {
            WorkStep::Finish(Completion::new("finished"))
        } else {
            let first = self.0.swap(false, Ordering::SeqCst);
            WorkStep::Tool(ToolCall::new(
                "bash",
                json!({"command": if first { "printf effect > effect.txt; sleep 5" } else { "printf resumed > resumed.txt" }}),
            ))
        };
        Box::pin(async move { Ok(WorkProposal::new(step)) })
    }
    fn compact(
        &self,
        _: CompactInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<CheckpointDraft, CallError>> {
        panic!("no compaction expected")
    }
}

#[cfg(unix)]
#[tokio::test]
async fn restored_unknown_write_resolves_against_original_runtime_ledger() {
    for ledger_completed in [false, true] {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        let (app, workspace) = app_with_model(
            &temporary.path().join("data"),
            &root,
            Arc::new(RecoveryWriteModel(std::sync::atomic::AtomicBool::new(true))),
        )
        .await;
        app.update_config(
            ConfigScope::User,
            ConfigChange::Tools(Some(ToolLimits {
                default_bash_timeout: Duration::from_secs(1),
                max_bash_timeout: Duration::from_secs(1),
                ..ToolLimits::default()
            })),
        )
        .await
        .unwrap();
        let session = app
            .create_session(workspace.id, "recover write identity")
            .await
            .unwrap();
        session
            .submit(SubmitInput::new("write once"))
            .await
            .unwrap();
        let (original_call, outcome) = wait_for_tool_finished(&session, "bash").await;
        assert_eq!(outcome.external_effect, ExternalEffect::Unknown);
        let session_id = session.snapshot().await.unwrap().session.id;
        session.close_runtime().await.unwrap();
        if ledger_completed {
            app.test_store()
                .resolve_write(
                    workspace.id,
                    session_id,
                    original_call,
                    WriteResolution {
                        external_effect: ExternalEffect::Applied,
                        evidence: "verified while detached".into(),
                    },
                )
                .unwrap();
        }
        let next = session
            .submit(SubmitInput::new("continue after restart"))
            .await
            .unwrap();
        let mut updates = session.observe();
        let new_runtime = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let RuntimeState::Running { id, .. } = &updates.borrow().runtime {
                    break *id;
                }
                updates.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_ne!(new_runtime, original_call.runtime);
        if !ledger_completed {
            assert_eq!(
                session
                    .resolve_write(
                        original_call,
                        WriteResolution {
                            external_effect: ExternalEffect::Applied,
                            evidence: "verified effect.txt".into(),
                        }
                    )
                    .await
                    .unwrap(),
                CommandReceipt::Applied
            );
        }
        let finished = wait_for_input(&session, next.input).await;
        assert!(matches!(
            finished,
            InputState::Finished {
                outcome: bone_core::InputOutcome::Completed,
                ..
            }
        ));
        let stored = app
            .test_store()
            .load_core_restore(session_id)
            .unwrap()
            .unwrap();
        assert!(stored.records.iter().any(|record| matches!(&record.body,
            RecordBody::ToolFinished { call, outcome, .. } if call.0 == original_call.id && outcome.external_effect == ExternalEffect::Applied
        )));
        assert_eq!(
            app.test_store()
                .core_call_origin(session_id, bone_core::CallId(original_call.id))
                .unwrap(),
            Some(original_call)
        );
        assert!(
            app.unresolved_writes(workspace.id)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            std::fs::read_to_string(root.join("resumed.txt")).unwrap(),
            "resumed"
        );
        app.shutdown().await.unwrap();
    }
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
        ConfigChange::Tools(Some(ToolLimits {
            default_bash_timeout: Duration::from_secs(1),
            max_bash_timeout: Duration::from_secs(1),
            ..ToolLimits::default()
        })),
    )
    .await
    .unwrap();

    app.update_config(
        ConfigScope::User,
        ConfigChange::Limits(Some(AgentLimits {
            // Keep the Agent's outer deadline well beyond Bash's own
            // one-second deadline so this test exercises Bash cleanup and
            // write auditing instead of racing the two timeout layers.
            tool_timeout: Duration::from_secs(10),
            ..crate::config::RuntimeSettings::default().limits
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
        AppOptions::isolated(temporary.path().join("data")),
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
    let workspace = app.open_workspace(&workspace_root).await.unwrap();
    let session = app.create_session(workspace.id, "Write").await.unwrap();
    let receipt = session.submit(SubmitInput::new("write it")).await.unwrap();
    assert_completed(wait_for_input(&session, receipt.input).await);
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
        AppOptions::isolated(temporary.path().join("data")),
        Arc::new(CompletingModel),
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
        AppOptions::isolated(data),
        Arc::new(CompletingModel),
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

#[tokio::test]
async fn a_finished_write_stays_blocking_until_its_agent_record_is_saved() {
    use bone_core::{CallId, JobId, Origin, Record, RecordBody, Seq, ToolOutcome};

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
            0,
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
        seq: Seq(2),
        origin: Origin::Kernel,
        body: RecordBody::ToolFinished {
            job: JobId(3),
            call: CallId(call.id),
            request: Arc::new(ToolCall::new("apply_patch", json!({}))),
            outcome: Arc::new(outcome),
        },
    };
    let started = Arc::new(Record {
        seq: Seq(1),
        origin: Origin::Kernel,
        body: RecordBody::CallStarted {
            call: CallId(call.id),
            kind: bone_core::CallKind::Tool,
            job: Some(JobId(3)),
            tool: Some(Arc::new(ToolCall::new("apply_patch", json!({})))),
        },
    });
    let durable = store.core_durable_port(
        session.info.id,
        runtime,
        Arc::new(store.claim_session(session.info.id).unwrap()),
        Arc::new(tokio::sync::Mutex::new(())),
    );
    durable
        .commit(bone_core::DurableCommit {
            commit_id: "finished-write".into(),
            expected_revision: 0,
            snapshot: serde_json::from_value(
                json!({ "version": 1, "epoch": 0, "through": 2, "payload": {} }),
            )
            .unwrap(),
            records: vec![Arc::clone(&started), Arc::new(record.clone())],
        })
        .await
        .unwrap();
    store
        .save_agent_record(session.info.id, runtime, &started, &[])
        .unwrap();
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
fn accepted_input_clears_the_current_question_for_answers_and_new_messages() {
    use bone_core::{
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
    let other_runtime = RuntimeId::new();
    let mut other = inputs[&InputId(1)].clone();
    if let InputState::WaitingForUser { runtime, .. } = &mut other.state {
        *runtime = other_runtime;
    }
    other.id = InputId(9);
    inputs.insert(other.id, other);
    for input in [
        AgentInput::new(AgentInputId(3), "the library")
            .answering(AgentInputId(1), Seq(question_record)),
        AgentInput::new(AgentInputId(3), "never mind; inspect the other module"),
    ] {
        let record = Record {
            seq: Seq(8),
            origin: Origin::User(AgentInputId(3)),
            body: RecordBody::Input(input),
        };
        assert_eq!(
            crate::session::input_changes(runtime, &record, &inputs),
            vec![
                (InputId(1), InputState::Accepted { runtime }),
                (InputId(2), InputState::Accepted { runtime }),
            ]
        );
    }
}

#[test]
fn retry_reconciles_every_failed_input_that_the_agent_restored() {
    use bone_core::{Input as AgentInput, InputId as AgentInputId, InputStatus, Seq};

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
                state: InputState::ConversationFailed {
                    runtime,
                    message: "failed".into(),
                },
            },
        );
    }
    let agent = bone_core::AgentView {
        inputs: [
            (1, InputStatus::Thinking),
            (2, InputStatus::Handled),
            (
                3,
                InputStatus::ConversationFailed {
                    message: "still failed".into(),
                },
            ),
        ]
        .into_iter()
        .map(|(id, status)| bone_core::InputView {
            input: AgentInput::new(AgentInputId(id), format!("input {id}")),
            accepted_at: Seq(id),
            status,
            required_jobs: Vec::new(),
        })
        .collect(),
        ..bone_core::AgentView::default()
    };

    assert_eq!(
        crate::session::restored_conversation_inputs(runtime, &inputs, &agent),
        vec![InputId(1), InputId(2)]
    );
}
