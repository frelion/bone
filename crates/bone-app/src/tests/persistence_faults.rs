use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use bone_agent::{
    Assignment, CallContext, CallError, CheckpointDraft, CompactInput, Completion, CoordinateInput,
    JobChange, JobSpec, KernelDecision, ModelPort, PortFuture, WorkInput, WorkProposal, WorkStep,
};
use bone_llm::EndpointConfig;
use tokio::sync::Notify;

use crate::*;

struct GatedModel {
    coordinate_calls: AtomicUsize,
    work_calls: AtomicUsize,
    first_coordinate_started: Arc<Notify>,
    release_first_coordinate: Arc<Notify>,
}

impl ModelPort for GatedModel {
    fn coordinate(
        &self,
        input: CoordinateInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<KernelDecision, CallError>> {
        let call = self.coordinate_calls.fetch_add(1, Ordering::SeqCst);
        let mut assignment = Assignment::new(JobSpec::new("answer", "session", "answered"));
        assignment.inputs = input.inputs.iter().map(|input| input.id).collect();
        if call == 0 {
            self.first_coordinate_started.notify_one();
            let release = Arc::clone(&self.release_first_coordinate);
            Box::pin(async move {
                release.notified().await;
                Ok(KernelDecision::Apply {
                    changes: vec![JobChange::Create(assignment)],
                    constraints: None,
                })
            })
        } else {
            Box::pin(async move {
                Ok(KernelDecision::Apply {
                    changes: vec![JobChange::Create(assignment)],
                    constraints: None,
                })
            })
        }
    }

    fn work(
        &self,
        _: WorkInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<WorkProposal, CallError>> {
        let call = self.work_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(WorkProposal::new(if call == 0 {
                WorkStep::Reply("first reply".into())
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

async fn wait_for_storage_problem(session: &Session) {
    let mut view = session.observe();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if matches!(view.borrow().problem, Some(AppProblem::Storage(_))) {
                return;
            }
            view.changed().await.unwrap();
        }
    })
    .await
    .expect("session did not publish the injected storage failure");
}

async fn wait_for_terminal_input(session: &Session, input: InputId) {
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
    .expect("input did not reach a terminal state");
}

#[tokio::test]
async fn agent_record_failure_blocks_execution_and_recovers_the_partial_archive() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    let model = Arc::new(GatedModel {
        coordinate_calls: AtomicUsize::new(0),
        work_calls: AtomicUsize::new(0),
        first_coordinate_started: Arc::new(Notify::new()),
        release_first_coordinate: Arc::new(Notify::new()),
    });
    let app = App::with_ports(
        AppOptions::new(temporary.path().join("data")),
        model.clone(),
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
    let session = app
        .create_session(workspace.id, "Persistence failure")
        .await
        .unwrap();
    let store = app.test_store();

    let first = session.submit(SubmitInput::new("first")).await.unwrap();
    tokio::time::timeout(
        Duration::from_secs(3),
        model.first_coordinate_started.notified(),
    )
    .await
    .expect("coordinator did not start");

    session.snapshot().await.unwrap();
    let through_before_failure = store.session(session.id()).unwrap().unwrap().agent_through;
    store.fail_agent_record_saves_after(1);
    model.release_first_coordinate.notify_one();

    wait_for_storage_problem(&session).await;
    let through_at_failure = store.session(session.id()).unwrap().unwrap().agent_through;
    assert_eq!(through_at_failure, through_before_failure + 1);

    let calls_while_blocked = model.coordinate_calls.load(Ordering::SeqCst);
    let second = session.submit(SubmitInput::new("second")).await.unwrap();
    tokio::task::yield_now().await;
    assert_eq!(
        model.coordinate_calls.load(Ordering::SeqCst),
        calls_while_blocked,
        "durable submit must stay queued while Agent history is not durable"
    );
    assert!(session.observe().borrow().inputs.iter().any(|input| {
        input.id == second.input && matches!(input.state, InputState::Queued { .. })
    }));

    store.clear_agent_record_save_failure();
    let recovered = session.snapshot().await.unwrap();
    assert!(recovered.problem.is_none());
    assert!(
        store.session(session.id()).unwrap().unwrap().agent_through > through_at_failure,
        "retry must persist the remainder of the Agent snapshot"
    );
    assert_eq!(
        store.replayed_agent_record_saves(),
        0,
        "the in-memory archive watermark must advance after every durable record"
    );
    let _ = session.retry(second.input).await.unwrap();
    wait_for_terminal_input(&session, second.input).await;
    assert!(model.coordinate_calls.load(Ordering::SeqCst) > calls_while_blocked);

    let history = session.history(SessionSeq(0), 256).await.unwrap();
    assert!(history.items.iter().any(|entry| matches!(
        entry.event,
        SessionEvent::InputSubmitted { input, .. } if input == first.input
    )));
    assert!(history.items.iter().any(|entry| matches!(
        entry.event,
        SessionEvent::InputSubmitted { input, .. } if input == second.input
    )));
    app.shutdown().await.unwrap();
}
