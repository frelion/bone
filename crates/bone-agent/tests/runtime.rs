//! Real actor execution with controlled futures and Tokio's deterministic clock.
mod support;
use bone_agent::*;
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use support::*;
use tokio::sync::{broadcast, mpsc, oneshot};

async fn start(
    handle: &AgentHandle,
    calls: &mut mpsc::UnboundedReceiver<ModelRequest>,
    id: u64,
    goal: &str,
) -> ModelRequest {
    handle.post(input(id, goal)).await.unwrap();
    receive(calls).await.route(goal);
    receive(calls).await
}

async fn start_tool(
    handle: &AgentHandle,
    calls: &mut mpsc::UnboundedReceiver<ModelRequest>,
) -> JobId {
    let request = start(handle, calls, 1, "operate").await;
    let job = request.job();
    request.work(operation("operation"));
    job
}

fn controlled_tool(
    effect: ToolEffect,
) -> (Arc<dyn ToolPort>, mpsc::UnboundedReceiver<ToolRequest>) {
    let (requests, receiver) = mpsc::unbounded_channel();
    (Arc::new(ControlledTool { requests, effect }), receiver)
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn kernel_routes_b_while_a_keeps_computing_then_b_uses_its_own_tools() {
    let (tool, mut tool_runs) = controlled_tool(ToolEffect::ReadOnly);
    let (handle, mut calls) = session(vec![tool], KernelConfig::default());
    let first = start(&handle, &mut calls, 1, "A original").await;
    let a = first.job();
    first.work(WorkProposal {
        next: Next::Continue,
        ..Default::default()
    });
    let mut held_a = receive(&mut calls).await;
    handle.post(input(2, "B original")).await.unwrap();
    let route = receive(&mut calls).await;
    assert_eq!(route.inputs(), [InputId(2)]);
    assert!(!held_a.context.cancellation_requested());
    assert!(!held_a.reply.is_closed());
    route.route("B");
    let b = receive(&mut calls).await;
    assert_ne!(b.job(), a);
    assert!(
        matches!(&b.input.task, ModelTask::Work { messages, .. } if messages == &vec![input(2, "B original")])
    );
    b.work(operation("operation"));
    let run = receive(&mut tool_runs).await;
    run.reply
        .send(CallOutcome::artifact(json!({"result":"B evidence"})))
        .unwrap();
    let b = receive(&mut calls).await;
    let mut notices = handle.subscribe();
    b.work(answer("B answer"));
    notice(
        &mut notices,
        |notice| matches!(notice, Notice::Reply { text, .. } if text == "B answer"),
    )
    .await;
    assert!(!held_a.reply.is_closed());
    handle.stop().await.unwrap();
    held_a.reply.closed().await;
    assert!(handle.shutdown().await.unwrap().unresolved_calls.is_empty());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_held_answer_drains_every_accepted_input_before_publication() {
    let (handle, mut calls) = session(vec![], KernelConfig::default());
    let old = start(&handle, &mut calls, 1, "A").await;
    let old_job = old.job();
    handle.post(input(2, "E1")).await.unwrap();
    let first = receive(&mut calls).await;
    handle.post(input(3, "E2")).await.unwrap();
    let mut observation = handle.observe().await.unwrap();
    old.work(answer("held answer"));
    observed(&mut observation, |step| {
        matches!(
            &step.event,
            Event::CallFinished {
                outcome: CallOutcome {
                    result: Ok(CallOutput::Work(_)),
                    ..
                },
                ..
            }
        )
    })
    .await;
    assert!(matches!(
        handle.post(input(4, "Busy")).await,
        Err(HandleError::Admission(AdmissionError::Busy))
    ));
    let mut notices = handle.subscribe();
    first.reply.send(decision(vec![])).unwrap();
    let second = receive(&mut calls).await;
    assert_eq!(second.inputs(), [InputId(3)]);
    assert!(!handle.snapshot().await.unwrap().record.iter().any(|record| matches!(&record.kind, RecordKind::Notice(Notice::Reply { job, .. }) if *job == old_job)));
    second.reply.send(decision(vec![])).unwrap();
    notice(
        &mut notices,
        |notice| matches!(notice, Notice::Reply { text, .. } if text == "held answer"),
    )
    .await;
    handle.post(input(4, "Busy")).await.unwrap();
    handle.shutdown().await.unwrap();
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn changed_goal_drops_noncooperative_old_wait_and_replacement_keeps_original_words() {
    let (handle, mut calls) = session(vec![], KernelConfig::default());
    let first = start(&handle, &mut calls, 1, "A original").await;
    let job = first.job();
    first.work(WorkProposal {
        next: Next::Continue,
        ..Default::default()
    });
    let mut old = receive(&mut calls).await;
    handle
        .post(input(2, "actually B, exact user words"))
        .await
        .unwrap();
    receive(&mut calls)
        .await
        .reply
        .send(decision(vec![update(
            job,
            Some("B"),
            JobAction::Keep,
            vec![InputId(2)],
            true,
        )]))
        .unwrap();
    let new = receive(&mut calls).await;
    assert_eq!(new.job(), job);
    old.reply.closed().await;
    assert!(
        matches!(&new.input.task, ModelTask::Work { messages, .. } if messages.iter().any(|input| input.text == "actually B, exact user words"))
    );
    let mut notices = handle.subscribe();
    new.work(answer("B complete"));
    notice(&mut notices, |notice| matches!(notice, Notice::JobFinished { id, state: JobState::Completed } if *id == job)).await;
    handle.shutdown().await.unwrap();
}

struct CancellationFailureModel(mpsc::UnboundedSender<ModelRequest>);
impl ModelPort for CancellationFailureModel {
    fn infer(
        &self,
        input: ModelInput,
        mut context: CallContext,
    ) -> BoxFuture<'static, CallOutcome> {
        let (reply, response) = oneshot::channel();
        self.0
            .send(ModelRequest {
                input,
                context: context.clone(),
                reply,
            })
            .unwrap();
        Box::pin(async move {
            tokio::select! {
                biased;
                _ = context.wait_for_cancellation() => CallOutcome::failed("old failure during cancellation"),
                outcome = response => outcome.unwrap(),
            }
        })
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn old_failure_racing_cancellation_cannot_fail_the_new_worker() {
    let (sender, mut calls) = mpsc::unbounded_channel();
    let handle = Runtime::spawn(
        Arc::new(CancellationFailureModel(sender)),
        vec![],
        KernelConfig::default(),
        RuntimeConfig::default(),
    )
    .unwrap();
    let first = start(&handle, &mut calls, 1, "A").await;
    let job = first.job();
    first.work(WorkProposal {
        next: Next::Continue,
        ..Default::default()
    });
    let _old = receive(&mut calls).await;
    handle.post(input(2, "B")).await.unwrap();
    receive(&mut calls)
        .await
        .reply
        .send(decision(vec![update(
            job,
            Some("B"),
            JobAction::Keep,
            vec![InputId(2)],
            true,
        )]))
        .unwrap();
    let new = receive(&mut calls).await;
    let mut notices = handle.subscribe();
    new.work(answer("new result"));
    notice(&mut notices, |notice| matches!(notice, Notice::JobFinished { id, state: JobState::Completed } if *id == job)).await;
    assert_eq!(
        handle.snapshot().await.unwrap().jobs[0].state,
        JobState::Completed
    );
    handle.shutdown().await.unwrap();
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn stop_ends_both_local_model_waits_without_waiting_for_routing() {
    let (handle, mut calls) = session(vec![], KernelConfig::default());
    let first = start(&handle, &mut calls, 1, "A").await;
    let job = first.job();
    first.work(WorkProposal {
        next: Next::Continue,
        ..Default::default()
    });
    let mut a = receive(&mut calls).await;
    handle.post(input(2, "B")).await.unwrap();
    let mut route = receive(&mut calls).await;
    handle.stop().await.unwrap();
    a.reply.closed().await;
    route.reply.closed().await;
    assert_eq!(handle.snapshot().await.unwrap().jobs.len(), 1);
    let thanks = start(&handle, &mut calls, 3, "Thanks").await;
    assert_ne!(thanks.job(), job);
    let mut notices = handle.subscribe();
    thanks.work(answer("welcome"));
    notice(&mut notices, |notice| {
        matches!(notice, Notice::InputFinished { id: InputId(3), .. })
    })
    .await;
    assert!(matches!(
        handle.snapshot().await.unwrap().jobs[0].state,
        JobState::Paused | JobState::Cancelled
    ));
    handle.shutdown().await.unwrap();
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn kernel_timeout_has_explicit_retry_while_worker_deadline_is_independent() {
    let (handle, mut calls) = session(
        vec![],
        KernelConfig {
            kernel_timeout: Duration::from_secs(3),
            work_timeout: Duration::from_secs(20),
            ..Default::default()
        },
    );
    let first = start(&handle, &mut calls, 1, "A").await;
    first.work(WorkProposal {
        next: Next::Continue,
        ..Default::default()
    });
    let mut a = receive(&mut calls).await;
    handle.post(input(2, "B original")).await.unwrap();
    let mut route = receive(&mut calls).await;
    let mut notices = handle.subscribe();
    tokio::time::advance(Duration::from_secs(3)).await;
    notice(&mut notices, |notice| {
        matches!(notice, Notice::InputRoutingFailed { .. })
    })
    .await;
    route.reply.closed().await;
    assert!(!a.reply.is_closed());
    assert!(calls.try_recv().is_err(), "no automatic retry");
    handle.retry_input(InputId(2)).await.unwrap();
    let retry = receive(&mut calls).await;
    assert_eq!(retry.inputs(), [InputId(2)]);
    retry.reply.send(decision(vec![])).unwrap();
    handle.stop().await.unwrap();
    a.reply.closed().await;
    handle.shutdown().await.unwrap();
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_worker_timeout_fails_only_its_job() {
    let (handle, mut calls) = session(
        vec![],
        KernelConfig {
            kernel_timeout: Duration::from_secs(20),
            work_timeout: Duration::from_secs(3),
            ..Default::default()
        },
    );
    let mut a = start(&handle, &mut calls, 1, "A").await;
    let job = a.job();
    handle.post(input(2, "B")).await.unwrap();
    let mut route = receive(&mut calls).await;
    let mut notices = handle.subscribe();
    tokio::time::advance(Duration::from_secs(3)).await;
    notice(&mut notices, |notice| matches!(notice, Notice::JobFinished { id, state: JobState::Failed { .. } } if *id == job)).await;
    a.reply.closed().await;
    assert!(!route.reply.is_closed());
    handle.stop().await.unwrap();
    route.reply.closed().await;
    handle.shutdown().await.unwrap();
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn progress_is_coalesced_without_restarting_models() {
    let (tool, mut runs) = controlled_tool(ToolEffect::ReadOnly);
    let (handle, mut calls) = session(vec![tool], KernelConfig::default());
    start_tool(&handle, &mut calls).await;
    let held = receive(&mut runs).await;
    let mut notices = handle.subscribe();
    for index in 0..10_000 {
        held.context.report_progress(CallProgress {
            message: index.to_string(),
            percent: None,
        });
    }
    notice(&mut notices, |notice| matches!(notice, Notice::CallProgress { progress, .. } if progress.message == "9999")).await;
    assert!(calls.try_recv().is_err());
    handle.shutdown().await.unwrap();
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn shutdown_reports_an_unconfirmed_write_after_grace_instead_of_claiming_cancellation() {
    let (tool, mut runs) = controlled_tool(ToolEffect::ExternalWrite);
    let (handle, mut calls) = session(vec![tool], KernelConfig::default());
    start_tool(&handle, &mut calls).await;
    let mut held = receive(&mut runs).await;
    let cloned = handle.clone();
    let shutdown = tokio::spawn(async move { cloned.shutdown().await.unwrap() });
    held.context.wait_for_cancellation().await;
    assert!(!held.reply.is_closed());
    tokio::time::advance(Duration::from_secs(5)).await;
    let report = shutdown.await.unwrap();
    assert_eq!(report.unresolved_calls.len(), 1);
    assert!(report.unresolved_calls[0].external_write);
    assert!(report.unresolved_calls[0].is_unresolved());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_write_can_refuse_cancel_and_report_real_success() {
    let (tool, mut runs) = controlled_tool(ToolEffect::ExternalWrite);
    let (handle, mut calls) = session(vec![tool], KernelConfig::default());
    start_tool(&handle, &mut calls).await;
    let mut held = receive(&mut runs).await;
    let mut notices = handle.subscribe();
    handle.stop().await.unwrap();
    held.context.wait_for_cancellation().await;
    held.reply.send(applied()).unwrap();
    notice(&mut notices, |notice| matches!(notice, Notice::CallFinished { outcome, .. } if outcome.external_effect == ExternalEffect::Applied)).await;
    assert!(calls.try_recv().is_err());
    assert!(handle.shutdown().await.unwrap().unresolved_calls.is_empty());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn unknown_write_requires_host_resolution_and_duplicates_are_silent() {
    let (tool, mut runs) = controlled_tool(ToolEffect::ExternalWrite);
    let (handle, mut calls) = session(vec![tool], KernelConfig::default());
    start_tool(&handle, &mut calls).await;
    let held = receive(&mut runs).await;
    let write = handle
        .snapshot()
        .await
        .unwrap()
        .calls
        .iter()
        .find(|call| call.external_write)
        .unwrap()
        .id;
    held.reply
        .send(CallOutcome::unknown("response lost"))
        .unwrap();
    let followup = receive(&mut calls).await;
    followup.work(WorkProposal::default());
    handle.stop().await.unwrap();
    let mut observation = handle.observe().await.unwrap();
    handle.resolve_write(write, applied()).await.unwrap();
    let resolved = observed(
        &mut observation,
        |step| matches!(step.event, Event::WriteResolved { id, .. } if id == write),
    )
    .await;
    assert!(
        !resolved
            .effects
            .iter()
            .any(|effect| matches!(effect, EffectSummary::Start { .. }))
    );
    let before = handle.observe().await.unwrap();
    handle.resolve_write(write, applied()).await.unwrap();
    let after = handle.observe().await.unwrap();
    assert_eq!(before.sequence, after.sequence);
    assert_eq!(before.snapshot, after.snapshot);
    assert!(handle.shutdown().await.unwrap().unresolved_calls.is_empty());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn invalid_host_resolutions_and_retries_do_not_change_state() {
    let (tool, mut runs) = controlled_tool(ToolEffect::ExternalWrite);
    let (handle, mut calls) = session(vec![tool], KernelConfig::default());
    start_tool(&handle, &mut calls).await;
    let held = receive(&mut runs).await;
    let snapshot = handle.snapshot().await.unwrap();
    let write = snapshot
        .calls
        .iter()
        .find(|call| call.external_write)
        .unwrap()
        .id;
    let read = snapshot
        .calls
        .iter()
        .find(|call| !call.external_write)
        .unwrap()
        .id;
    for id in [CallId(999), read, write] {
        assert!(matches!(
            handle.resolve_write(id, applied()).await,
            Err(HandleError::InvalidResolution(_))
        ));
    }
    assert!(matches!(
        handle.retry_input(InputId(999)).await,
        Err(HandleError::InvalidRetry(_))
    ));
    held.reply.send(CallOutcome::unknown("unknown")).unwrap();
    let _waiting = receive(&mut calls).await;
    let before = handle.observe().await.unwrap();
    for outcome in [
        CallOutcome::unknown("still unknown"),
        decision(vec![]),
        CallOutcome::work(answer("forged")),
    ] {
        assert!(matches!(
            handle.resolve_write(write, outcome).await,
            Err(HandleError::InvalidResolution(_))
        ));
    }
    let after = handle.observe().await.unwrap();
    assert_eq!(before.sequence, after.sequence);
    assert_eq!(before.snapshot, after.snapshot);
    handle
        .resolve_write(write, CallOutcome::failed("verified not applied"))
        .await
        .unwrap();
    assert!(matches!(
        handle.resolve_write(write, applied()).await,
        Err(HandleError::InvalidResolution(_))
    ));
    handle.shutdown().await.unwrap();
}

struct PanickingModel;
impl ModelPort for PanickingModel {
    fn infer(&self, _: ModelInput, _: CallContext) -> BoxFuture<'static, CallOutcome> {
        panic!("PRIVATE_MODEL_PANIC_SECRET");
    }
}
struct PanickingWrite;
impl ToolPort for PanickingWrite {
    fn specification(&self) -> ToolSpec {
        specification("operation", ToolEffect::ExternalWrite)
    }
    fn run(&self, _: Value, _: CallContext) -> BoxFuture<'static, CallOutcome> {
        Box::pin(async { panic!("PRIVATE_TOOL_PANIC_SECRET") })
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn model_future_construction_panic_is_reported_without_leaking_payload() {
    let handle = Runtime::spawn(
        Arc::new(PanickingModel),
        vec![],
        KernelConfig::default(),
        RuntimeConfig::default(),
    )
    .unwrap();
    let mut notices = handle.subscribe();
    handle.post(input(1, "hello")).await.unwrap();
    let outcome = notice(&mut notices, |notice| {
        matches!(
            notice,
            Notice::CallFinished {
                outcome: CallOutcome {
                    result: Err(CallError {
                        kind: CallErrorKind::Panicked,
                        ..
                    }),
                    ..
                },
                ..
            }
        )
    })
    .await;
    assert!(
        !serde_json::to_string(&outcome)
            .unwrap()
            .contains("PRIVATE_MODEL_PANIC_SECRET")
    );
    assert!(
        !serde_json::to_string(&handle.snapshot().await.unwrap())
            .unwrap()
            .contains("PRIVATE_MODEL_PANIC_SECRET")
    );
    handle.shutdown().await.unwrap();
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn write_future_panic_preserves_unknown_without_leaking_payload() {
    let (handle, mut calls) = session(vec![Arc::new(PanickingWrite)], KernelConfig::default());
    let mut notices = handle.subscribe();
    start_tool(&handle, &mut calls).await;
    let outcome = notice(&mut notices, |notice| {
        matches!(
            notice,
            Notice::CallFinished {
                outcome: CallOutcome {
                    result: Err(CallError {
                        kind: CallErrorKind::Panicked,
                        ..
                    }),
                    ..
                },
                ..
            }
        )
    })
    .await;
    assert!(matches!(
        outcome,
        Notice::CallFinished {
            outcome: CallOutcome {
                external_effect: ExternalEffect::Unknown,
                ..
            },
            ..
        }
    ));
    assert!(
        !serde_json::to_string(&handle.snapshot().await.unwrap())
            .unwrap()
            .contains("PRIVATE_TOOL_PANIC_SECRET")
    );
    let report = handle.shutdown().await.unwrap();
    assert_eq!(report.unresolved_calls.len(), 1);
    assert!(report.unresolved_calls[0].external_write);
}

struct ContinuingModel(AtomicUsize);
impl ModelPort for ContinuingModel {
    fn infer(&self, input: ModelInput, _: CallContext) -> BoxFuture<'static, CallOutcome> {
        let outcome = match input.task {
            ModelTask::Kernel { inputs, .. } => decision(vec![create(
                "continue",
                inputs.iter().map(|input| input.id).collect(),
            )]),
            ModelTask::Work { .. } => {
                let n = self.0.fetch_add(1, Ordering::SeqCst);
                CallOutcome::work(WorkProposal {
                    note: format!("step {n}"),
                    next: if n == 300 {
                        Next::Finish
                    } else {
                        Next::Continue
                    },
                    ..Default::default()
                })
            }
        };
        Box::pin(async move { outcome })
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn slow_subscribers_never_block_work_and_can_recover_a_baseline() {
    let model = Arc::new(ContinuingModel(AtomicUsize::new(0)));
    let handle = Runtime::spawn(
        model.clone(),
        vec![],
        KernelConfig::default(),
        RuntimeConfig::default(),
    )
    .unwrap();
    let mut slow_notices = handle.subscribe();
    let mut fast = handle.subscribe();
    let mut slow_events = handle.observe().await.unwrap();
    handle.post(input(1, "continue")).await.unwrap();
    notice(&mut fast, |notice| {
        matches!(notice, Notice::InputFinished { id: InputId(1), .. })
    })
    .await;
    assert_eq!(model.0.load(Ordering::SeqCst), 301);
    assert!(matches!(
        slow_notices.recv().await,
        Err(broadcast::error::RecvError::Lagged(_))
    ));
    assert!(matches!(
        slow_events.events.recv().await,
        Err(broadcast::error::RecvError::Lagged(_))
    ));
    let mut recovered = handle.observe().await.unwrap();
    handle.stop().await.unwrap();
    let step = observed(&mut recovered, |step| matches!(step.event, Event::Stop)).await;
    assert_eq!(step.sequence, recovered.sequence + 1);
    assert!(
        step.records
            .iter()
            .all(|record| record.cursor > recovered.snapshot.record_cursor)
    );
    handle.shutdown().await.unwrap();
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn receipt_observer_baseline_and_next_sequence_have_no_subscription_gap() {
    let (handle, mut calls) = session(vec![], KernelConfig::default());
    let mut observation = handle.observe().await.unwrap();
    let receipt = handle.post(input(77, "original")).await.unwrap();
    let step = observed(
        &mut observation,
        |step| matches!(&step.event, Event::Input(input) if input.id == InputId(77)),
    )
    .await;
    assert_eq!(step.sequence, observation.sequence + 1);
    assert!(
        step.records
            .iter()
            .any(|record| record.cursor == receipt.record_cursor)
    );
    let _held = receive(&mut calls).await;
    let before = handle.observe().await.unwrap();
    assert_eq!(handle.post(input(77, "original")).await.unwrap(), receipt);
    let after = handle.observe().await.unwrap();
    assert_eq!(before.sequence, after.sequence);
    assert_eq!(before.snapshot, after.snapshot);
    assert!(matches!(
        handle.post(input(77, "different")).await,
        Err(HandleError::Admission(AdmissionError::ConflictingInput))
    ));
    handle.shutdown().await.unwrap();
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn full_input_capacity_does_not_block_stop_and_busy_inputs_are_not_accepted() {
    let (handle, mut calls) = session(vec![], KernelConfig::default());
    handle.post(input(1, "held")).await.unwrap();
    let mut held = receive(&mut calls).await;
    for id in 2..=32 {
        handle.post(input(id, "queued")).await.unwrap();
    }
    assert!(matches!(
        handle.post(input(33, "overflow")).await,
        Err(HandleError::Admission(AdmissionError::Busy))
    ));
    assert_eq!(handle.snapshot().await.unwrap().inputs.len(), 32);
    handle.stop().await.unwrap();
    held.reply.closed().await;
    handle.shutdown().await.unwrap();
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn finishing_b_does_not_cancel_a_or_c_timer() {
    let (handle, mut calls) = session(vec![], KernelConfig::default());
    let a = start(&handle, &mut calls, 1, "A").await;
    a.work(WorkProposal {
        next: Next::Continue,
        ..Default::default()
    });
    let mut a = receive(&mut calls).await;
    let c = start(&handle, &mut calls, 2, "monitor C").await;
    let c_id = c.job();
    c.work(WorkProposal {
        next: Next::Wait {
            reconsider_after: Some(Duration::from_secs(7)),
        },
        ..Default::default()
    });
    let b = start(&handle, &mut calls, 3, "B").await;
    let mut notices = handle.subscribe();
    b.work(answer("B done"));
    notice(&mut notices, |notice| {
        matches!(notice, Notice::InputFinished { id: InputId(3), .. })
    })
    .await;
    assert!(!a.reply.is_closed());
    tokio::time::advance(Duration::from_secs(7)).await;
    assert_eq!(receive(&mut calls).await.job(), c_id);
    handle.stop().await.unwrap();
    a.reply.closed().await;
    handle.shutdown().await.unwrap();
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn concurrent_repeated_shutdown_returns_the_same_final_report() {
    let (handle, _) = session(vec![], KernelConfig::default());
    let second = handle.clone();
    let (a, b) = tokio::join!(handle.shutdown(), second.shutdown());
    assert_eq!(a.unwrap().unresolved_calls, b.unwrap().unresolved_calls);
    assert!(handle.shutdown().await.unwrap().unresolved_calls.is_empty());
    assert!(matches!(handle.snapshot().await, Err(HandleError::Closed)));
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn dropping_all_handles_cancels_noncooperative_waits_and_closes_observers() {
    let (handle, mut calls) = session(vec![], KernelConfig::default());
    let mut notices = handle.subscribe();
    let mut observation = handle.observe().await.unwrap();
    handle.post(input(1, "held")).await.unwrap();
    let mut held = receive(&mut calls).await;
    let last = handle.clone();
    drop(handle);
    assert_eq!(last.snapshot().await.unwrap().inputs.len(), 1);
    drop(last);
    held.reply.closed().await;
    while notices.recv().await.is_ok() {}
    assert!(matches!(
        notices.recv().await,
        Err(broadcast::error::RecvError::Closed)
    ));
    while observation.events.recv().await.is_ok() {}
    assert!(matches!(
        observation.events.recv().await,
        Err(broadcast::error::RecvError::Closed)
    ));
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn finishing_a_job_cleans_up_its_own_read_without_cancelling_another_job() {
    let (tool, mut runs) = controlled_tool(ToolEffect::ReadOnly);
    let (handle, mut calls) = session(vec![tool], KernelConfig::default());
    let first = start(&handle, &mut calls, 1, "A").await;
    let a = first.job();
    first.work(WorkProposal {
        next: Next::Continue,
        ..operation("operation")
    });
    let mut read = receive(&mut runs).await;
    let current_a = receive(&mut calls).await;
    let mut b = start(&handle, &mut calls, 2, "B").await;
    let mut notices = handle.subscribe();
    current_a.work(answer("A finished using available evidence"));
    notice(&mut notices, |notice| matches!(notice, Notice::JobFinished { id, state: JobState::Completed } if *id == a)).await;
    read.reply.closed().await;
    assert!(!b.reply.is_closed());
    handle.stop().await.unwrap();
    b.reply.closed().await;
    assert!(handle.shutdown().await.unwrap().unresolved_calls.is_empty());
}
