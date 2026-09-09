use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use bone_core::{
    Assignment, CallContext, CallError, CheckpointDraft, CompactInput, Completion, CoordinateInput,
    JobChange, JobSpec, KernelDecision, ModelPort, PortFuture, WorkInput, WorkProposal, WorkStep,
};
use tokio::sync::Notify;

use super::{assert_completed, configure_test_model, wait_for_input};
use crate::*;

#[derive(Default)]
struct GatedModel {
    coordinate_calls: AtomicUsize,
    work_calls: AtomicUsize,
    coordinate_started: Notify,
    release_first_coordinate: Arc<Notify>,
}

impl ModelPort for GatedModel {
    fn coordinate(
        &self,
        input: CoordinateInput,
        _: CallContext,
    ) -> PortFuture<std::result::Result<KernelDecision, CallError>> {
        let call = self.coordinate_calls.fetch_add(1, Ordering::SeqCst);
        self.coordinate_started.notify_one();
        let mut assignment = Assignment::new(JobSpec::new("answer", "session", "answered"));
        assignment.inputs = input.inputs.iter().map(|input| input.id).collect();
        if call == 0 {
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

#[tokio::test]
async fn agent_record_failure_blocks_execution_and_recovers_the_partial_archive() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    let model = Arc::new(GatedModel::default());
    let app = App::with_ports(
        AppOptions::new(temporary.path().join("data")),
        model.clone(),
        Vec::new(),
    )
    .await
    .unwrap();
    configure_test_model(&app).await;
    let workspace = app.open_workspace(workspace_root).await.unwrap();
    let session = app
        .create_session(workspace.id, "Persistence failure")
        .await
        .unwrap();
    let store = app.test_store();

    let first = session.submit(SubmitInput::new("first")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), model.coordinate_started.notified())
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
    // Round-trip through the session and core actors while archival is still blocked.
    assert!(matches!(session.snapshot().await, Err(Error::Storage(_))));
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
    assert_completed(wait_for_input(&session, second.input).await);
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

#[tokio::test]
async fn failed_runtime_config_persistence_never_restarts_execution() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    let model = Arc::new(GatedModel::default());
    let app = App::with_ports(
        AppOptions::new(temporary.path().join("data")),
        model.clone(),
        Vec::new(),
    )
    .await
    .unwrap();
    configure_test_model(&app).await;
    let workspace = app.open_workspace(workspace_root).await.unwrap();
    let session = app
        .create_session(workspace.id, "Config persistence")
        .await
        .unwrap();
    session.submit(SubmitInput::new("start")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), model.coordinate_started.notified())
        .await
        .expect("coordinator did not start");

    let store = app.test_store();
    store.fail_runtime_reconfigure(true);
    let limits = AgentLimits {
        background_workers: 5,
        ..AgentLimits::default()
    };
    let result = app
        .update_config(
            ConfigScope::Session(session.id()),
            ConfigChange::Limits(Some(limits.clone())),
        )
        .await;
    assert!(matches!(result, Err(Error::Storage(_))), "{result:?}");
    // Observation is acknowledged by the core after reconfiguration, so even a
    // newly scheduled model future that has not been polled would be visible here.
    let blocked = session.snapshot().await.unwrap();
    assert!(blocked.activity.is_empty(), "{blocked:?}");
    assert_eq!(
        model.coordinate_calls.load(Ordering::SeqCst),
        1,
        "new configuration must not run before its runtime boundary is durable"
    );
    assert_eq!(
        store
            .session(session.id())
            .unwrap()
            .unwrap()
            .runtime
            .unwrap()
            .config
            .limits,
        AgentLimits::default()
    );

    store.fail_runtime_reconfigure(false);
    app.update_config(
        ConfigScope::Session(session.id()),
        ConfigChange::Limits(Some(limits)),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), model.coordinate_started.notified())
        .await
        .expect("execution did not resume after the boundary became durable");
    assert!(model.coordinate_calls.load(Ordering::SeqCst) >= 2);
    app.shutdown().await.unwrap();
}
