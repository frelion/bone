use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use bone_core::{
    CallContext, CallError, CheckpointDraft, CompactInput, Completion, CoordinateInput,
    KernelDecision, ModelPort, PortFuture, WorkInput, WorkProposal, WorkStep,
};
use tokio::sync::Notify;

use super::{assert_completed, configure_test_model, route_input, wait_for_input};
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
        let decision = route_input(&input, "answer");
        if call == 0 {
            let release = Arc::clone(&self.release_first_coordinate);
            Box::pin(async move {
                release.notified().await;
                Ok(decision)
            })
        } else {
            Box::pin(async move { Ok(decision) })
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
        AppOptions::isolated(temporary.path().join("data")),
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
        AppOptions::isolated(temporary.path().join("data")),
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
    // App observations are archived asynchronously. Suspension revokes model
    // authority immediately, but its cancellation result still needs a durable
    // commit and projection before activity disappears from the cached view.
    let mut updates = session.observe();
    let blocked = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            assert_eq!(
                model.coordinate_calls.load(Ordering::SeqCst),
                1,
                "failed configuration must never start another coordinator"
            );
            let view = updates.borrow().clone();
            if view.activity.is_empty() {
                break view;
            }
            updates.changed().await.unwrap();
        }
    })
    .await
    .expect("cancelled activity was not durably projected");
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

#[tokio::test]
async fn closing_reaches_core_while_session_is_waiting_for_an_input_commit() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    let model = Arc::new(GatedModel::default());
    let app = App::with_ports(
        AppOptions::isolated(temporary.path().join("data")),
        model.clone(),
        Vec::new(),
    )
    .await
    .unwrap();
    configure_test_model(&app).await;
    let workspace = app.open_workspace(workspace_root).await.unwrap();
    let session = app
        .create_session(workspace.id, "Pending input commit")
        .await
        .unwrap();
    app.update_config(
        ConfigScope::Session(session.id()),
        ConfigChange::Limits(Some(AgentLimits {
            shutdown_grace: Duration::from_millis(30),
            ..AgentLimits::default()
        })),
    )
    .await
    .unwrap();
    session.submit(SubmitInput::new("first")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), model.coordinate_started.notified())
        .await
        .unwrap();
    session.snapshot().await.unwrap();

    let store = app.test_store();
    store.pause_core_commits(true);
    session.submit(SubmitInput::new("second")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !store.core_commit_waiting() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(1), session.close_runtime()).await;
    // Release even on a failed assertion, so a blocked test transaction cannot
    // keep Tokio's blocking pool alive after this test exits.
    store.pause_core_commits(false);
    let result = result.expect("App close did not reach the blocked Core actor");
    assert!(matches!(result, Err(Error::Agent(_))), "{result:?}");
    assert_eq!(model.coordinate_calls.load(Ordering::SeqCst), 1);

    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match session.close_runtime().await {
                Ok(_) => break,
                Err(Error::Agent(_)) => tokio::task::yield_now().await,
                Err(error) => panic!("unexpected close error: {error}"),
            }
        }
    })
    .await
    .expect("resolved durable commit should allow closing");
    let continued = session
        .submit(SubmitInput::new("continue after closing"))
        .await
        .unwrap();
    assert_completed(wait_for_input(&session, continued.input).await);
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn closing_during_startup_reports_an_unresolved_commit() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();
    let model = Arc::new(GatedModel::default());
    let app = App::with_ports(
        AppOptions::isolated(temporary.path().join("data")),
        model.clone(),
        Vec::new(),
    )
    .await
    .unwrap();
    configure_test_model(&app).await;
    let workspace = app.open_workspace(workspace_root).await.unwrap();
    let session = app
        .create_session(workspace.id, "Pending startup commit")
        .await
        .unwrap();
    let store = app.test_store();
    store.pause_core_commits(true);
    session.submit(SubmitInput::new("first")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !store.core_commit_waiting() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(1), session.close_runtime()).await;
    store.pause_core_commits(false);
    let result =
        result.expect("close should interrupt startup without waiting for the transaction");
    assert!(matches!(result, Err(Error::Agent(_))), "{result:?}");
    assert_eq!(model.coordinate_calls.load(Ordering::SeqCst), 0);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match session.close_runtime().await {
                Ok(_) => break,
                Err(Error::Agent(_)) => tokio::task::yield_now().await,
                Err(error) => panic!("unexpected close error: {error}"),
            }
        }
    })
    .await
    .expect("resolved startup commit should allow closing");
    model.release_first_coordinate.notify_one();
    let continued = session
        .submit(SubmitInput::new("continue after startup close"))
        .await
        .unwrap();
    assert_completed(wait_for_input(&session, continued.input).await);
    app.shutdown().await.unwrap();
}
