use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use bone_agent::*;
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc, oneshot};

struct ModelRequest {
    input: ModelInput,
    context: JobContext,
    reply: oneshot::Sender<JobOutcome>,
}

struct ControlledModel(mpsc::UnboundedSender<ModelRequest>);

impl ModelPort for ControlledModel {
    fn infer(&self, input: ModelInput, context: JobContext) -> BoxFuture<'static, JobOutcome> {
        let (reply, response) = oneshot::channel();
        self.0
            .send(ModelRequest {
                input,
                context,
                reply,
            })
            .unwrap();
        // Deliberately never reads cancellation. The runtime must drop this
        // local wait before it can release a cancelled model slot.
        Box::pin(async move {
            response
                .await
                .unwrap_or_else(|_| JobOutcome::failed("test reply dropped"))
        })
    }
}

struct ToolRequest {
    context: JobContext,
    reply: oneshot::Sender<JobOutcome>,
}

struct ControlledTool {
    requests: mpsc::UnboundedSender<ToolRequest>,
    effect: ToolEffect,
}

fn tool_spec(effect: ToolEffect) -> ToolSpec {
    ToolSpec {
        name: "operation".into(),
        description: "controlled operation".into(),
        parameters: json!({"type": "object"}),
        effect,
    }
}

impl ToolPort for ControlledTool {
    fn specification(&self) -> ToolSpec {
        tool_spec(self.effect)
    }

    fn run(&self, _: Value, context: JobContext) -> BoxFuture<'static, JobOutcome> {
        let (reply, response) = oneshot::channel();
        self.requests.send(ToolRequest { context, reply }).unwrap();
        Box::pin(async move {
            response
                .await
                .unwrap_or_else(|_| JobOutcome::unknown("test reply dropped"))
        })
    }
}

fn session(
    tools: Vec<Arc<dyn ToolPort>>,
    kernel: KernelConfig,
) -> (AgentHandle, mpsc::UnboundedReceiver<ModelRequest>) {
    let (requests, receiver) = mpsc::unbounded_channel();
    let handle = Runtime::spawn(
        Arc::new(ControlledModel(requests)),
        tools,
        kernel,
        RuntimeConfig::default(),
    )
    .unwrap();
    (handle, receiver)
}

async fn request(requests: &mut mpsc::UnboundedReceiver<ModelRequest>) -> ModelRequest {
    tokio::time::timeout(Duration::from_secs(2), requests.recv())
        .await
        .expect("model request should start without waiting for another job")
        .expect("model request channel should stay open")
}

async fn notice(
    notices: &mut broadcast::Receiver<Notice>,
    matches: impl Fn(&Notice) -> bool,
) -> Notice {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let event = notices
                .recv()
                .await
                .expect("notice receiver should keep up");
            if matches(&event) {
                return event;
            }
        }
    })
    .await
    .expect("expected notice")
}

async fn observed(
    observation: &mut Observation,
    matches: impl Fn(&StepEvent) -> bool,
) -> Arc<StepEvent> {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let step = observation
                .events
                .recv()
                .await
                .expect("observer should keep up");
            if matches(&step) {
                return step;
            }
        }
    })
    .await
    .expect("expected observed transition")
}

async fn start_tool(handle: &AgentHandle, requests: &mut mpsc::UnboundedReceiver<ModelRequest>) {
    let receipt = handle.post("begin the task").await.unwrap();
    let accepted = handle.snapshot().await.unwrap();
    assert!(matches!(
        &accepted.record[receipt.record_cursor as usize - 1].kind,
        RecordKind::UserMessage(message) if message.id == receipt.id
    ));
    let work = request(requests).await;
    assert!(matches!(work.input.task, ModelTask::Work { .. }));
    work.reply
        .send(JobOutcome::work(WorkResult {
            autonomy: Autonomy::Run,
            operation: Some(Operation::Tool(ToolCall::new("operation", json!({})))),
            ..WorkResult::default()
        }))
        .unwrap();
}

fn keep(reply: &str) -> JobOutcome {
    JobOutcome::review(InputReview {
        disposition: InputDisposition::Keep,
        reply: Some(reply.into()),
        note: "status only".into(),
    })
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn status_review_runs_concurrently_without_restarting_the_work() {
    let (handle, mut requests) = session(Vec::new(), KernelConfig::default());
    let mut notices = handle.subscribe();
    handle.post("solve this difficult task").await.unwrap();
    let work = request(&mut requests).await;
    let id = handle.snapshot().await.unwrap().work.unwrap();
    for value in 0..10_000 {
        work.context.report_progress(JobProgress {
            message: value.to_string(),
            percent: None,
        });
    }
    notice(
        &mut notices,
        |event| matches!(event, Notice::JobProgress { progress, .. } if progress.message == "9999"),
    )
    .await;
    handle.post("how is it going?").await.unwrap();
    let review = request(&mut requests).await;
    assert!(matches!(review.input.task, ModelTask::ReviewInput { .. }));
    assert!(review.input.snapshot.tools.is_empty());
    assert_eq!(handle.snapshot().await.unwrap().work, Some(id));
    review.reply.send(keep("still thinking")).unwrap();
    notice(
        &mut notices,
        |event| matches!(event, Notice::Reply { text, .. } if text == "still thinking"),
    )
    .await;
    assert!(!work.context.cancellation_requested());
    work.reply
        .send(JobOutcome::work(WorkResult {
            reply: Some("the answer".into()),
            next: Next::Finish,
            ..WorkResult::default()
        }))
        .unwrap();
    notice(&mut notices, |event| {
        matches!(event, Notice::Finished { .. })
    })
    .await;
    let snapshot = handle.snapshot().await.unwrap();
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
            .record
            .iter()
            .filter(|entry| matches!(&entry.kind,
                RecordKind::Notice(Notice::JobProgress { id: job, .. }) if *job == id
            ))
            .count(),
        1
    );
    assert!(handle.shutdown().await.unwrap().unresolved_jobs.is_empty());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_returned_work_answer_waits_for_every_fixed_review_batch() {
    let (handle, mut requests) = session(Vec::new(), KernelConfig::default());
    let mut observation = handle.observe().await.unwrap();
    let mut notices = handle.subscribe();
    handle.post("solve A").await.unwrap();
    let work = request(&mut requests).await;
    let work_id = handle.snapshot().await.unwrap().work.unwrap();
    let first_message = handle.post("status?").await.unwrap();
    let first_review = request(&mut requests).await;
    let second_message = handle.post("still there?").await.unwrap();
    work.reply
        .send(JobOutcome::work(WorkResult {
            reply: Some("answer A".into()),
            next: Next::Finish,
            ..WorkResult::default()
        }))
        .unwrap();
    let held = observed(&mut observation, |step| {
        step.records.iter().any(|entry| {
            matches!(
                entry.kind, RecordKind::WorkHeld { job, .. } if job == work_id
            )
        })
    })
    .await;
    assert!(!held.effects.iter().any(|effect| matches!(effect, EffectSummary::Publish(Notice::Reply { text, .. }) if text == "answer A")));
    first_review.reply.send(keep("working")).unwrap();
    let second_review = request(&mut requests).await;
    assert!(
        matches!(&second_review.input.task, ModelTask::ReviewInput { messages }
        if messages.iter().map(|message| message.id).collect::<Vec<_>>() == vec![second_message.id])
    );
    assert_eq!(handle.snapshot().await.unwrap().candidate, Some(work_id));
    second_review.reply.send(keep("yes")).unwrap();
    notice(&mut notices, |event| {
        matches!(event, Notice::Finished { .. })
    })
    .await;
    let snapshot = handle.snapshot().await.unwrap();
    for message in [first_message.id, second_message.id] {
        assert_eq!(snapshot.record.iter().filter(|entry| matches!(
            &entry.kind, RecordKind::InputReviewed { messages, .. } if messages.contains(&message)
        )).count(), 1);
    }
    handle.shutdown().await.unwrap();
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn reconsider_drops_a_noncooperative_future_before_starting_replacement_work() {
    let (handle, mut requests) = session(Vec::new(), KernelConfig::default());
    let mut observation = handle.observe().await.unwrap();
    let mut notices = handle.subscribe();
    let original = handle.post("solve A").await.unwrap();
    let mut old = request(&mut requests).await;
    let old_id = handle.snapshot().await.unwrap().work.unwrap();
    let changed = handle.post("do not use A; consider B").await.unwrap();
    request(&mut requests)
        .await
        .reply
        .send(JobOutcome::review(InputReview {
            disposition: InputDisposition::Reconsider,
            note: "new constraint".into(),
            reply: None,
        }))
        .unwrap();
    old.reply.closed().await;
    let completion = observed(&mut observation, |step| {
        matches!(step.event,
            Event::JobFinished { id, .. } if id == old_id
        )
    })
    .await;
    assert!(matches!(
        &completion.event,
        Event::JobFinished {
            outcome: JobOutcome {
                result: Err(JobError {
                    kind: JobErrorKind::Cancelled,
                    ..
                }),
                external_effect: ExternalEffect::None,
            },
            ..
        }
    ));
    let replacement = request(&mut requests).await;
    assert!(
        matches!(&replacement.input.task, ModelTask::Work { messages }
        if messages.iter().map(|message| message.id).collect::<Vec<_>>() == vec![original.id, changed.id])
    );
    let current = handle.snapshot().await.unwrap();
    assert!(current.work.is_some_and(|id| id != old_id));
    assert!(current.review.is_none());
    replacement
        .reply
        .send(JobOutcome::work(WorkResult {
            autonomy: Autonomy::Run,
            reply: Some("answer B".into()),
            next: Next::Finish,
            ..WorkResult::default()
        }))
        .unwrap();
    notice(
        &mut notices,
        |event| matches!(event, Notice::Reply { text, .. } if text == "answer B"),
    )
    .await;
    let snapshot = handle.snapshot().await.unwrap();
    assert!(
        !snapshot
            .record
            .iter()
            .any(|entry| matches!(entry.kind, RecordKind::Notice(Notice::Paused)))
    );
    assert!(handle.shutdown().await.unwrap().unresolved_jobs.is_empty());
}

struct CancellationFailureModel(mpsc::UnboundedSender<ModelRequest>);

impl ModelPort for CancellationFailureModel {
    fn infer(&self, input: ModelInput, mut context: JobContext) -> BoxFuture<'static, JobOutcome> {
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
                () = context.wait_for_cancellation() => JobOutcome::failed("old request failed while cancelling"),
                outcome = response => outcome.unwrap_or_else(|_| JobOutcome::failed("test reply dropped")),
            }
        })
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn an_old_failure_racing_cancellation_cannot_pause_replacement_work() {
    let (sender, mut requests) = mpsc::unbounded_channel();
    let handle = Runtime::spawn(
        Arc::new(CancellationFailureModel(sender)),
        Vec::new(),
        KernelConfig::default(),
        RuntimeConfig::default(),
    )
    .unwrap();
    let mut notices = handle.subscribe();
    handle.post("solve A").await.unwrap();
    let _old = request(&mut requests).await;
    let old_id = handle.snapshot().await.unwrap().work.unwrap();
    handle.post("switch to B").await.unwrap();
    request(&mut requests)
        .await
        .reply
        .send(JobOutcome::review(InputReview::default()))
        .unwrap();
    notice(&mut notices, |event| {
        matches!(event, Notice::JobFinished { id, outcome: JobOutcome {
        result: Err(JobError { kind: JobErrorKind::Failed, message }), ..
    }} if *id == old_id && message == "old request failed while cancelling")
    })
    .await;
    let replacement = request(&mut requests).await;
    assert!(matches!(replacement.input.task, ModelTask::Work { .. }));
    replacement
        .reply
        .send(JobOutcome::work(WorkResult {
            reply: Some("answer B".into()),
            autonomy: Autonomy::Run,
            next: Next::Finish,
            ..WorkResult::default()
        }))
        .unwrap();
    notice(&mut notices, |event| {
        matches!(event, Notice::Finished { .. })
    })
    .await;
    assert!(
        !handle
            .snapshot()
            .await
            .unwrap()
            .record
            .iter()
            .any(|entry| matches!(entry.kind, RecordKind::Notice(Notice::Paused)))
    );
    handle.shutdown().await.unwrap();
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn stop_releases_both_model_waits_and_a_later_thank_you_does_not_resume_work() {
    let (handle, mut requests) = session(Vec::new(), KernelConfig::default());
    let mut notices = handle.subscribe();
    handle.post("solve A").await.unwrap();
    let mut work = request(&mut requests).await;
    handle.post("status?").await.unwrap();
    let mut review = request(&mut requests).await;
    handle.stop().await.unwrap();
    work.reply.closed().await;
    review.reply.closed().await;
    handle.post("thank you").await.unwrap();
    let thanks = request(&mut requests).await;
    assert!(matches!(&thanks.input.task, ModelTask::Work { messages }
        if messages.len() == 1 && messages[0].text == "thank you"));
    thanks
        .reply
        .send(JobOutcome::work(WorkResult {
            reply: Some("you are welcome".into()),
            ..WorkResult::default()
        }))
        .unwrap();
    notice(
        &mut notices,
        |event| matches!(event, Notice::Reply { text, .. } if text == "you are welcome"),
    )
    .await;
    assert!(!handle.snapshot().await.unwrap().autonomous);
    assert!(handle.shutdown().await.unwrap().unresolved_jobs.is_empty());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn work_and_review_have_independent_deadlines_and_current_failures_pause() {
    for (work_timeout, review_timeout, failing_work) in [(3, 10, true), (10, 3, false)] {
        let (handle, mut requests) = session(
            Vec::new(),
            KernelConfig {
                work_timeout: Duration::from_secs(work_timeout),
                review_timeout: Duration::from_secs(review_timeout),
                ..KernelConfig::default()
            },
        );
        let mut notices = handle.subscribe();
        handle.post("task").await.unwrap();
        let _work = request(&mut requests).await;
        handle.post("status?").await.unwrap();
        let _review = request(&mut requests).await;
        let snapshot = handle.snapshot().await.unwrap();
        let expected = if failing_work {
            snapshot.work.unwrap()
        } else {
            snapshot.review.unwrap()
        };
        tokio::time::advance(Duration::from_secs(3)).await;
        notice(&mut notices, |event| matches!(event, Notice::JobFinished { id, outcome: JobOutcome {
            result: Err(JobError { kind: JobErrorKind::TimedOut, .. }), external_effect: ExternalEffect::None,
        }} if *id == expected)).await;
        notice(&mut notices, |event| matches!(event, Notice::Paused)).await;
        handle.post("a new request").await.unwrap();
        request(&mut requests)
            .await
            .reply
            .send(JobOutcome::work(WorkResult {
                reply: Some("recovered".into()),
                ..WorkResult::default()
            }))
            .unwrap();
        notice(
            &mut notices,
            |event| matches!(event, Notice::Reply { text, .. } if text == "recovered"),
        )
        .await;
        assert!(handle.shutdown().await.unwrap().unresolved_jobs.is_empty());
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn tool_progress_and_soft_deadline_are_observed_without_invoking_review() {
    let (runs, mut started) = mpsc::unbounded_channel();
    let (handle, mut requests) = session(
        vec![Arc::new(ControlledTool {
            requests: runs,
            effect: ToolEffect::ReadOnly,
        })],
        KernelConfig {
            soft_deadline: Duration::from_secs(3),
            ..KernelConfig::default()
        },
    );
    let mut observation = handle.observe().await.unwrap();
    start_tool(&handle, &mut requests).await;
    let mut tool = started.recv().await.unwrap();
    tool.context.report_progress(JobProgress {
        message: "waiting for data".into(),
        percent: None,
    });
    observed(&mut observation, |step| {
        matches!(step.event, Event::JobProgress { .. })
    })
    .await;
    tokio::time::advance(Duration::from_secs(3)).await;
    let reminder = observed(&mut observation, |step| {
        matches!(step.event, Event::Wake { .. })
    })
    .await;
    assert!(
        reminder
            .records
            .iter()
            .any(|entry| matches!(entry.kind, RecordKind::Reminder { .. }))
    );
    assert!(
        reminder
            .effects
            .iter()
            .any(|effect| matches!(effect, EffectSummary::Start {
        request: JobRequest::Work { messages }, generation, ..
    } if messages.is_empty() && *generation == observation.snapshot.generation))
    );
    let work = request(&mut requests).await;
    assert!(matches!(work.input.task, ModelTask::Work { .. }));
    assert!(
        work.input
            .snapshot
            .jobs
            .iter()
            .any(|job| matches!(job.request, JobRequest::Tool(_)) && job.is_running())
    );
    work.reply
        .send(JobOutcome::work(WorkResult::default()))
        .unwrap();
    handle.stop().await.unwrap();
    tool.reply.closed().await;
    let report = handle.shutdown().await.unwrap();
    assert!(report.unresolved_jobs.is_empty());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn shutdown_abandons_read_only_models_but_reports_unconfirmed_writes() {
    let (runs, mut started) = mpsc::unbounded_channel();
    let (handle, mut requests) = session(
        vec![Arc::new(ControlledTool {
            requests: runs,
            effect: ToolEffect::ExternalWrite,
        })],
        KernelConfig::default(),
    );
    start_tool(&handle, &mut requests).await;
    let mut tool = started.recv().await.unwrap();
    handle.post("think about the next step").await.unwrap();
    let mut work = request(&mut requests).await;
    let closing = handle.clone();
    let shutdown = tokio::spawn(async move { closing.shutdown().await.unwrap() });
    tool.context.wait_for_cancellation().await;
    work.reply.closed().await;
    assert!(!tool.reply.is_closed());
    assert!(matches!(
        handle.post("too late").await,
        Err(HandleError::ShuttingDown)
    ));
    assert!(matches!(
        handle
            .resolve_write(
                JobId(999),
                JobOutcome::artifact("unavailable while closing")
            )
            .await,
        Err(HandleError::ShuttingDown)
    ));
    tokio::time::advance(Duration::from_secs(5)).await;
    let report = shutdown.await.unwrap();
    assert_eq!(report.unresolved_jobs.len(), 1);
    assert!(report.unresolved_jobs[0].external_write);
    assert!(matches!(
        report.unresolved_jobs[0].state,
        JobState::CancelRequested
    ));
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_write_can_refuse_cancellation_and_later_report_its_real_success() {
    let (runs, mut started) = mpsc::unbounded_channel();
    let (handle, mut requests) = session(
        vec![Arc::new(ControlledTool {
            requests: runs,
            effect: ToolEffect::ExternalWrite,
        })],
        KernelConfig::default(),
    );
    let mut observation = handle.observe().await.unwrap();
    start_tool(&handle, &mut requests).await;
    let mut tool = started.recv().await.unwrap();
    handle.stop().await.unwrap();
    let stopped = observed(&mut observation, |step| matches!(step.event, Event::Stop)).await;
    assert!(
        stopped
            .effects
            .iter()
            .any(|effect| matches!(effect, EffectSummary::RequestCancel { .. }))
    );
    tool.context.wait_for_cancellation().await;
    assert!(!tool.reply.is_closed());
    tool.context.report_progress(JobProgress {
        message: "cannot cancel: already submitted".into(),
        percent: None,
    });
    observed(&mut observation, |step| {
        matches!(step.event, Event::JobProgress { .. })
    })
    .await;
    tool.reply
        .send(JobOutcome {
            result: Ok(JobOutput::Artifact(json!({"saved": true}))),
            external_effect: ExternalEffect::Applied,
        })
        .unwrap();
    let result = observed(&mut observation, |step| {
        matches!(
            step.event,
            Event::JobFinished {
                outcome: JobOutcome {
                    external_effect: ExternalEffect::Applied,
                    ..
                },
                ..
            }
        )
    })
    .await;
    assert!(
        !result
            .effects
            .iter()
            .any(|effect| matches!(effect, EffectSummary::Start { .. }))
    );
    assert!(!handle.snapshot().await.unwrap().autonomous);
    assert!(handle.shutdown().await.unwrap().unresolved_jobs.is_empty());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn finish_names_read_only_cleanup_and_its_cancellation_does_not_restart_work() {
    let (runs, mut started) = mpsc::unbounded_channel();
    let (handle, mut requests) = session(
        vec![Arc::new(ControlledTool {
            requests: runs,
            effect: ToolEffect::ReadOnly,
        })],
        KernelConfig::default(),
    );
    let mut notices = handle.subscribe();
    start_tool(&handle, &mut requests).await;
    let mut tool = started.recv().await.unwrap();
    let tool_id = handle
        .snapshot()
        .await
        .unwrap()
        .jobs
        .iter()
        .find(|job| matches!(job.request, JobRequest::Tool(_)))
        .unwrap()
        .id;
    handle
        .post("we have enough, deliver the answer")
        .await
        .unwrap();
    request(&mut requests)
        .await
        .reply
        .send(JobOutcome::work(WorkResult {
            reply: Some("delivered".into()),
            next: Next::Finish,
            ..WorkResult::default()
        }))
        .unwrap();
    let finished = notice(&mut notices, |event| {
        matches!(event, Notice::Finished { .. })
    })
    .await;
    assert!(matches!(finished, Notice::Finished { cleanup } if cleanup == vec![tool_id]));
    tool.reply.closed().await;
    notice(&mut notices, |event| matches!(event, Notice::JobFinished { id, outcome: JobOutcome {
        result: Err(JobError { kind: JobErrorKind::Cancelled, .. }), external_effect: ExternalEffect::None,
    }} if *id == tool_id)).await;
    let snapshot = handle.snapshot().await.unwrap();
    assert!(!snapshot.autonomous);
    assert!(snapshot.work.is_none());
    assert_eq!(
        snapshot
            .record
            .iter()
            .filter(|entry| matches!(entry.kind, RecordKind::Notice(Notice::Finished { .. })))
            .count(),
        1
    );
    assert!(handle.shutdown().await.unwrap().unresolved_jobs.is_empty());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn unknown_writes_block_finish_and_remain_in_the_shutdown_report() {
    let (runs, mut started) = mpsc::unbounded_channel();
    let (handle, mut requests) = session(
        vec![Arc::new(ControlledTool {
            requests: runs,
            effect: ToolEffect::ExternalWrite,
        })],
        KernelConfig::default(),
    );
    let mut observation = handle.observe().await.unwrap();
    start_tool(&handle, &mut requests).await;
    started
        .recv()
        .await
        .unwrap()
        .reply
        .send(JobOutcome::unknown("connection lost after submit"))
        .unwrap();
    request(&mut requests)
        .await
        .reply
        .send(JobOutcome::work(WorkResult {
            reply: Some("not yet justified".into()),
            next: Next::Finish,
            ..WorkResult::default()
        }))
        .unwrap();
    let blocked = observed(&mut observation, |step| {
        step.records
            .iter()
            .any(|entry| matches!(entry.kind, RecordKind::PlanDiscarded { .. }))
    })
    .await;
    assert!(
        !blocked
            .effects
            .iter()
            .any(|effect| matches!(effect, EffectSummary::Publish(Notice::Finished { .. })))
    );
    let snapshot = handle.snapshot().await.unwrap();
    assert!(
        !snapshot
            .record
            .iter()
            .any(|entry| matches!(entry.kind, RecordKind::Notice(Notice::Finished { .. })))
    );
    let report = handle.shutdown().await.unwrap();
    assert_eq!(report.unresolved_jobs.len(), 1);
    assert!(
        matches!(&report.unresolved_jobs[0].state, JobState::Finished(outcome) if outcome.external_effect == ExternalEffect::Unknown)
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn host_confirmation_releases_the_write_gate_and_duplicate_confirmation_is_silent() {
    let (runs, mut started) = mpsc::unbounded_channel();
    let (handle, mut requests) = session(
        vec![Arc::new(ControlledTool {
            requests: runs,
            effect: ToolEffect::ExternalWrite,
        })],
        KernelConfig::default(),
    );
    let mut notices = handle.subscribe();
    start_tool(&handle, &mut requests).await;
    let first = started.recv().await.unwrap();
    let id = handle
        .snapshot()
        .await
        .unwrap()
        .jobs
        .iter()
        .find(|job| matches!(job.request, JobRequest::Tool(_)))
        .unwrap()
        .id;
    first
        .reply
        .send(JobOutcome::unknown("response lost after submit"))
        .unwrap();
    let waiting = request(&mut requests).await;
    let waiting_id = handle.snapshot().await.unwrap().work.unwrap();
    waiting
        .reply
        .send(JobOutcome::work(WorkResult::default()))
        .unwrap();
    notice(
        &mut notices,
        |event| matches!(event, Notice::JobFinished { id, .. } if *id == waiting_id),
    )
    .await;

    let confirmed = JobOutcome {
        result: Ok(JobOutput::Artifact(json!({"remote_receipt": "saved-1"}))),
        external_effect: ExternalEffect::Applied,
    };
    let mut observation = handle.observe().await.unwrap();
    handle.resolve_write(id, confirmed.clone()).await.unwrap();
    let resolution = observed(&mut observation, |step| {
        matches!(&step.event,
            Event::JobFinished { id: resolved, outcome } if *resolved == id && *outcome == confirmed
        )
    })
    .await;
    assert!(resolution.effects.iter().any(|effect| matches!(
        effect,
        EffectSummary::Start {
            request: JobRequest::Work { .. },
            ..
        }
    )));
    let before_duplicate = handle.observe().await.unwrap();
    handle.resolve_write(id, confirmed.clone()).await.unwrap();
    let after_duplicate = handle.observe().await.unwrap();
    assert_eq!(before_duplicate.sequence, after_duplicate.sequence);
    assert_eq!(before_duplicate.snapshot, after_duplicate.snapshot);

    request(&mut requests)
        .await
        .reply
        .send(JobOutcome::work(WorkResult {
            operation: Some(Operation::Tool(ToolCall::new(
                "operation",
                json!({"next": true}),
            ))),
            ..WorkResult::default()
        }))
        .unwrap();
    let second = tokio::time::timeout(Duration::from_secs(2), started.recv())
        .await
        .expect("confirmed first write must release the gate")
        .unwrap();
    assert_eq!(
        handle
            .snapshot()
            .await
            .unwrap()
            .jobs
            .iter()
            .filter(|job| matches!(job.request, JobRequest::Tool(_)))
            .count(),
        2
    );
    handle.stop().await.unwrap();
    second
        .reply
        .send(JobOutcome {
            result: Ok(JobOutput::Artifact(json!({"remote_receipt": "saved-2"}))),
            external_effect: ExternalEffect::Applied,
        })
        .unwrap();
    assert!(handle.shutdown().await.unwrap().unresolved_jobs.is_empty());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn invalid_write_confirmations_never_change_session_state() {
    let (runs, mut started) = mpsc::unbounded_channel();
    let (handle, mut requests) = session(
        vec![Arc::new(ControlledTool {
            requests: runs,
            effect: ToolEffect::ExternalWrite,
        })],
        KernelConfig::default(),
    );
    start_tool(&handle, &mut requests).await;
    let tool = started.recv().await.unwrap();
    let baseline = handle.snapshot().await.unwrap();
    let write = baseline
        .jobs
        .iter()
        .find(|job| job.external_write)
        .unwrap()
        .id;
    let model = baseline
        .jobs
        .iter()
        .find(|job| !job.external_write)
        .unwrap()
        .id;
    for id in [JobId(u64::MAX), model, write] {
        assert!(matches!(
            handle
                .resolve_write(id, JobOutcome::artifact("not valid yet"))
                .await,
            Err(HandleError::InvalidResolution(_))
        ));
        assert_eq!(handle.snapshot().await.unwrap(), baseline);
    }
    tool.reply
        .send(JobOutcome::unknown("verification required"))
        .unwrap();
    let _work = request(&mut requests).await;
    let baseline = handle.observe().await.unwrap();
    for outcome in [
        JobOutcome::unknown("still uncertain"),
        JobOutcome::work(WorkResult::default()),
        JobOutcome::review(InputReview::default()),
    ] {
        assert!(matches!(
            handle.resolve_write(write, outcome).await,
            Err(HandleError::InvalidResolution(_))
        ));
        let after = handle.observe().await.unwrap();
        assert_eq!(after.sequence, baseline.sequence);
        assert_eq!(after.snapshot, baseline.snapshot);
    }
    let not_applied = JobOutcome::failed("remote system confirms the write never happened");
    handle.resolve_write(write, not_applied).await.unwrap();
    let confirmed = handle.observe().await.unwrap();
    assert!(matches!(
        handle
            .resolve_write(
                write,
                JobOutcome {
                    result: Ok(JobOutput::Artifact(json!({"conflicting": true}))),
                    external_effect: ExternalEffect::Applied,
                }
            )
            .await,
        Err(HandleError::InvalidResolution(_))
    ));
    let unchanged = handle.observe().await.unwrap();
    assert_eq!(unchanged.sequence, confirmed.sequence);
    assert_eq!(unchanged.snapshot, confirmed.snapshot);
    assert!(handle.shutdown().await.unwrap().unresolved_jobs.is_empty());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn confirming_an_unknown_write_after_stop_records_the_fact_without_resuming() {
    let (runs, mut started) = mpsc::unbounded_channel();
    let (handle, mut requests) = session(
        vec![Arc::new(ControlledTool {
            requests: runs,
            effect: ToolEffect::ExternalWrite,
        })],
        KernelConfig::default(),
    );
    let mut observation = handle.observe().await.unwrap();
    start_tool(&handle, &mut requests).await;
    let tool = started.recv().await.unwrap();
    let id = handle
        .snapshot()
        .await
        .unwrap()
        .jobs
        .iter()
        .find(|job| job.external_write)
        .unwrap()
        .id;
    tool.reply
        .send(JobOutcome::unknown("connection lost"))
        .unwrap();
    let mut work = request(&mut requests).await;
    let work_id = handle.snapshot().await.unwrap().work.unwrap();
    handle.stop().await.unwrap();
    work.reply.closed().await;
    observed(
        &mut observation,
        |step| matches!(step.event, Event::JobFinished { id, .. } if id == work_id),
    )
    .await;
    let confirmed = JobOutcome {
        result: Ok(JobOutput::Artifact(json!({"receipt": "found-by-host"}))),
        external_effect: ExternalEffect::Applied,
    };
    handle.resolve_write(id, confirmed.clone()).await.unwrap();
    let resolution = observed(&mut observation, |step| {
        matches!(&step.event,
            Event::JobFinished { id: resolved, outcome } if *resolved == id && *outcome == confirmed
        )
    })
    .await;
    assert!(
        !resolution
            .effects
            .iter()
            .any(|effect| matches!(effect, EffectSummary::Start { .. }))
    );
    let snapshot = handle.snapshot().await.unwrap();
    assert!(!snapshot.autonomous);
    assert!(snapshot.work.is_none());
    assert!(snapshot.review.is_none());
    assert!(snapshot.candidate.is_none());
    assert!(handle.shutdown().await.unwrap().unresolved_jobs.is_empty());
}

struct PanickingModel;

impl ModelPort for PanickingModel {
    fn infer(&self, _: ModelInput, _: JobContext) -> BoxFuture<'static, JobOutcome> {
        panic!("synchronous model failure")
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_panic_constructing_a_model_future_is_a_job_observation() {
    let handle = Runtime::spawn(
        Arc::new(PanickingModel),
        Vec::new(),
        KernelConfig::default(),
        RuntimeConfig::default(),
    )
    .unwrap();
    let mut notices = handle.subscribe();
    handle.post("hello").await.unwrap();
    notice(&mut notices, |event| matches!(event, Notice::JobFinished { outcome: JobOutcome {
        result: Err(JobError { kind: JobErrorKind::Panicked, message }), external_effect: ExternalEffect::None,
    }, .. } if message.contains("synchronous model failure"))).await;
    assert!(handle.shutdown().await.unwrap().unresolved_jobs.is_empty());
}

struct PanickingTool;

impl ToolPort for PanickingTool {
    fn specification(&self) -> ToolSpec {
        tool_spec(ToolEffect::ExternalWrite)
    }
    fn run(&self, _: Value, _: JobContext) -> BoxFuture<'static, JobOutcome> {
        Box::pin(async { panic!("asynchronous write failure") })
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_write_panic_is_an_unknown_external_effect() {
    let (handle, mut requests) = session(vec![Arc::new(PanickingTool)], KernelConfig::default());
    let mut notices = handle.subscribe();
    start_tool(&handle, &mut requests).await;
    notice(&mut notices, |event| {
        matches!(
            event,
            Notice::JobFinished {
                outcome: JobOutcome {
                    result: Err(JobError {
                        kind: JobErrorKind::Panicked,
                        ..
                    }),
                    external_effect: ExternalEffect::Unknown,
                },
                ..
            }
        )
    })
    .await;
    let report = handle.shutdown().await.unwrap();
    assert_eq!(report.unresolved_jobs.len(), 1);
    assert!(report.unresolved_jobs[0].external_write);
}

struct InvalidEffectModel;

impl ModelPort for InvalidEffectModel {
    fn infer(&self, _: ModelInput, _: JobContext) -> BoxFuture<'static, JobOutcome> {
        Box::pin(async {
            JobOutcome {
                result: Ok(JobOutput::Work(WorkResult::default())),
                external_effect: ExternalEffect::Unknown,
            }
        })
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn read_only_protocol_errors_cannot_leave_an_unknown_write() {
    let handle = Runtime::spawn(
        Arc::new(InvalidEffectModel),
        Vec::new(),
        KernelConfig::default(),
        RuntimeConfig::default(),
    )
    .unwrap();
    let mut notices = handle.subscribe();
    handle.post("hello").await.unwrap();
    notice(&mut notices, |event| {
        matches!(
            event,
            Notice::JobFinished {
                outcome: JobOutcome {
                    result: Err(JobError {
                        kind: JobErrorKind::Failed,
                        ..
                    }),
                    external_effect: ExternalEffect::None,
                },
                ..
            }
        )
    })
    .await;
    assert!(handle.shutdown().await.unwrap().unresolved_jobs.is_empty());
}

struct ContinuingModel(AtomicUsize);

impl ModelPort for ContinuingModel {
    fn infer(&self, input: ModelInput, _: JobContext) -> BoxFuture<'static, JobOutcome> {
        assert!(
            matches!(input.task, ModelTask::Work { .. }),
            "normal work never needs a reviewer"
        );
        let count = self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            JobOutcome::work(WorkResult {
                note: format!("reasoning step {count}"),
                autonomy: Autonomy::Run,
                next: if count == 300 {
                    Next::Finish
                } else {
                    Next::Continue
                },
                ..WorkResult::default()
            })
        })
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn slow_notices_and_observers_do_not_block_work_and_can_reestablish_a_baseline() {
    let model = Arc::new(ContinuingModel(AtomicUsize::new(0)));
    let handle = Runtime::spawn(
        model.clone(),
        Vec::new(),
        KernelConfig::default(),
        RuntimeConfig::default(),
    )
    .unwrap();
    let mut slow_notices = handle.subscribe();
    let mut fast = handle.subscribe();
    let mut slow_events = handle.observe().await.unwrap();
    handle.post("think without tools").await.unwrap();
    notice(&mut fast, |event| matches!(event, Notice::Finished { .. })).await;
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
    assert!(!recovered.snapshot.autonomous);
    assert!(recovered.snapshot.record.len() > 300);
    handle.stop().await.unwrap();
    let step = observed(&mut recovered, |step| matches!(step.event, Event::Stop)).await;
    assert_eq!(step.sequence, recovered.sequence + 1);
    assert!(
        step.records
            .first()
            .is_some_and(|entry| entry.cursor > recovered.snapshot.record_cursor)
    );
    handle.shutdown().await.unwrap();
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn observation_baseline_and_job_metadata_have_no_subscription_gap() {
    let (handle, mut requests) = session(Vec::new(), KernelConfig::default());
    let mut observation = handle.observe().await.unwrap();
    let receipt = handle.post("first task").await.unwrap();
    let step = observed(&mut observation, |step| {
        matches!(step.event, Event::UserMessage { .. })
    })
    .await;
    assert_eq!(step.sequence, observation.sequence + 1);
    assert!(
        step.records
            .iter()
            .any(|entry| entry.cursor == receipt.record_cursor)
    );
    assert!(
        step.effects
            .iter()
            .any(|effect| matches!(effect, EffectSummary::Start {
        request: JobRequest::Work { messages }, generation, ..
    } if messages == &vec![receipt.id] && *generation == observation.snapshot.generation))
    );
    let _work = request(&mut requests).await;
    let next = handle.observe().await.unwrap();
    assert_eq!(next.sequence, step.sequence);
    assert!(next.snapshot.work.is_some());
    handle.shutdown().await.unwrap();
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn concurrent_and_repeated_shutdown_calls_receive_the_same_final_report() {
    let (handle, _) = session(Vec::new(), KernelConfig::default());
    let other = handle.clone();
    let (first, second) = tokio::join!(handle.shutdown(), other.shutdown());
    let first = first.unwrap();
    assert_eq!(first.unresolved_jobs, second.unwrap().unresolved_jobs);
    assert!(first.unresolved_jobs.is_empty());
    assert_eq!(
        handle.shutdown().await.unwrap().unresolved_jobs,
        first.unresolved_jobs
    );
    assert!(matches!(handle.snapshot().await, Err(HandleError::Closed)));
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn dropping_every_handle_cleans_up_noncooperative_reads_and_closes_observers() {
    let (handle, mut requests) = session(Vec::new(), KernelConfig::default());
    let mut observation = handle.observe().await.unwrap();
    let mut notices = handle.subscribe();
    handle.post("held work").await.unwrap();
    let mut held = request(&mut requests).await;
    let last = handle.clone();
    drop(handle);
    assert!(last.snapshot().await.unwrap().work.is_some());
    drop(last);
    held.reply.closed().await;
    notice(&mut notices, |event| {
        matches!(
            event,
            Notice::JobFinished {
                outcome: JobOutcome {
                    result: Err(JobError {
                        kind: JobErrorKind::Cancelled,
                        ..
                    }),
                    external_effect: ExternalEffect::None,
                },
                ..
            }
        )
    })
    .await;
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
