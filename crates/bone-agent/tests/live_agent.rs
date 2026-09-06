//! Opt-in acceptance with real provider results and temporary workspace tools.
//! Delivery gates create deterministic interleavings without editing any result.
//! A pending local job is not evidence that a provider is still computing remotely.

use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use bone_agent::{
    AgentHandle, InputDisposition, JobContext, JobId, JobOutcome, JobOutput, JobRequest, JobState,
    KernelConfig, MessageId, ModelInput, ModelPort, ModelTask, Notice, RecordKind, Runtime,
    RuntimeConfig,
};
use bone_agent::{ModelAdapter, SystemConfig, TaskConfig, read_only_tools};
use bone_llm::service::chatgpt_subscription;
use bone_tools::ToolEnvironment;
use tokio::sync::{Notify, broadcast};

#[tokio::test]
#[ignore = "requires BONE_CONFIG, network access, and BONE's own ChatGPT login"]
async fn real_models_read_interrupt_change_direction_and_stop() {
    tokio::time::timeout(Duration::from_secs(10 * 60), acceptance())
        .await
        .expect("live acceptance exceeded ten minutes");
}

async fn acceptance() {
    let path =
        std::env::var_os("BONE_CONFIG").expect("set BONE_CONFIG to the system configuration");
    let config = bone_agent::config_builder()
        .unwrap()
        .build(path)
        .expect("valid shared configuration");
    let snapshot = config.snapshot().unwrap();
    let system = SystemConfig::from_snapshot(&snapshot).expect("valid system configuration");
    let solver = system.solver_for(&TaskConfig::default()).unwrap();
    println!(
        "Input reviewer: {}; solver: {}",
        system.coordinator.model, solver.model
    );
    let endpoint = chatgpt_subscription::connect(
        "bone-agent-live",
        snapshot
            .get::<bone_llm::LlmConfig>()
            .unwrap()
            .unwrap_or_default()
            .resolve_credential_root()
            .unwrap(),
        |prompt| {
            // Interactive only; do not redirect a first login to a persistent log.
            println!("Authorize at {}", prompt.verification_uri);
            println!("Device code: {} (do not share)", prompt.user_code);
        },
    )
    .await
    .expect("BONE's ChatGPT connection");
    let model = Arc::new(
        ModelAdapter::new(
            endpoint.model(&system.coordinator.model).unwrap(),
            endpoint.model(&solver.model).unwrap(),
        )
        .with_efforts(system.coordinator.effort, solver.effort),
    );
    let config = KernelConfig {
        soft_deadline: Duration::from_secs(u64::from(system.soft_deadline_seconds)),
        review_timeout: system.coordinator.timeout(),
        work_timeout: solver.timeout(),
    };

    let workspace = tempfile::tempdir().unwrap();
    let proof = format!(
        "bone-proof-{}",
        workspace.path().file_name().unwrap().to_string_lossy()
    );
    std::fs::write(workspace.path().join("proof.txt"), &proof).unwrap();
    let environment = ToolEnvironment::with_limits(
        workspace.path(),
        snapshot
            .get::<bone_tools::ToolLimits>()
            .unwrap()
            .unwrap_or_default(),
    )
    .unwrap();

    let mut read = Session::new(model.clone(), &environment, config.clone());
    read.post("Read proof.txt with the read tool. Reply with its exact contents and finish.")
        .await;
    read.until(|event| matches!(event, Notice::Finished { .. }))
        .await;
    let snapshot = read.agent.snapshot().await.unwrap();
    assert!(snapshot.jobs.iter().any(
        |job| matches!(&job.request, JobRequest::Tool(call) if call.name == "read")
            && matches!(&job.state, JobState::Finished(outcome) if outcome.result.is_ok())
    ));
    assert!(
        !snapshot
            .jobs
            .iter()
            .any(|job| matches!(job.request, JobRequest::ReviewInput { .. })),
        "normal solving must not call the coordinator"
    );
    assert!(read.replies.iter().any(|reply| reply.trim() == proof));
    read.close().await;
    println!("PASS: real solver -> native read -> solver answer, zero input reviews");

    let held = Arc::new(DeliveryGates::new(model.clone()));
    let mut work = Session::new(held.clone(), &environment, config.clone());
    work.post("This is a design discussion, with no files to inspect. Evaluate direction A: a single in-memory job queue with retries. Consider crash recovery, duplicates, cancellation, and ordering. Give a concise answer of at most five sentences beginning with 'Selected: A' and finish.").await;
    let original = work.work_started().await;
    for question in [
        "How is it going? Just a short status update; do not change or restart the work.",
        "Still working? This is only a progress question, with no new task requirement.",
    ] {
        let status = work.post(question).await;
        work.reply_to(status).await;
        println!(
            "status reply received; first Work provider request returned = {} (delivery remains gated)",
            held.first_work_provider_returned.load(Ordering::SeqCst)
        );
        let snapshot = work.agent.snapshot().await.unwrap();
        assert_eq!(snapshot.work, Some(original));
        assert_eq!(
            snapshot
                .jobs
                .iter()
                .filter(|job| matches!(job.request, JobRequest::Work { .. }))
                .count(),
            1
        );
        assert!(snapshot.record.iter().any(|entry| matches!(&entry.kind, RecordKind::InputReviewed { messages, disposition: InputDisposition::Keep, .. } if messages.contains(&status))));
    }
    println!("PASS: real reviews answer status with Keep; original Work JobId is unchanged");

    // Hold this review until the real old work has reached the kernel. This
    // exercises candidate handling as well as the natural-language routing.
    held.hold_next_review.store(true, Ordering::SeqCst);
    let changed = work.post("How is it going? Also, do not select A. Consider direction B instead: a durable append-only log with a single state owner. The solver should reconsider using my original words. Give a concise answer beginning with 'Selected: B' and finish; no files or clarifying questions are needed.").await;
    work.until(|event| matches!(event, Notice::JobStarted { request: JobRequest::ReviewInput { messages }, .. } if messages.contains(&changed))).await;
    held.release_work.notify_one();
    work.until(|event| matches!(event, Notice::JobFinished { id, .. } if *id == original))
        .await;
    let snapshot = work.agent.snapshot().await.unwrap();
    assert_eq!(snapshot.candidate, Some(original));
    assert!(
        !work
            .replies
            .iter()
            .any(|reply| reply.starts_with("Selected: A"))
    );
    held.release_review.notify_one();
    work.until(|event| matches!(event, Notice::Finished { .. }))
        .await;
    let snapshot = work.agent.snapshot().await.unwrap();
    assert!(snapshot.record.iter().any(|entry| matches!(&entry.kind, RecordKind::InputReviewed { messages, disposition: InputDisposition::Reconsider, .. } if messages.contains(&changed))));
    assert!(snapshot.record.iter().any(
        |entry| matches!(entry.kind, RecordKind::PlanDiscarded { job, .. } if job == original)
    ));
    assert!(snapshot.jobs.iter().any(|job| job.id == original
        && matches!(&job.state,
        JobState::Finished(outcome) if matches!(outcome.result, Ok(JobOutput::Work(_))))));
    assert!(
        work.replies
            .last()
            .is_some_and(|reply| reply.starts_with("Selected: B"))
    );
    assert!(
        !work
            .replies
            .iter()
            .any(|reply| reply.starts_with("Selected: A"))
    );
    assert!(!snapshot.autonomous);
    work.close().await;
    println!("PASS: real A result retained as material; no stale A reply; solver delivers B");

    let held = Arc::new(DeliveryGates::new(model));
    held.hold_next_review.store(true, Ordering::SeqCst);
    let mut stopped = Session::new(held, &environment, config);
    stopped.post("Reason about linearizability of a replicated job queue with retries and cancellation. Derive an invariant and a concrete counterexample. Answer concisely without tools.").await;
    stopped.work_started().await;
    let status = stopped
        .post("Please give a brief status update while the solver continues.")
        .await;
    stopped.until(|event| matches!(event, Notice::JobStarted { request: JobRequest::ReviewInput { messages }, .. } if messages.contains(&status))).await;
    let before = stopped.agent.snapshot().await.unwrap();
    assert!(before.work.is_some() && before.review.is_some());
    tokio::time::timeout(Duration::from_secs(2), stopped.agent.stop())
        .await
        .expect("stop does not wait for providers")
        .unwrap();
    stopped
        .until(|event| matches!(event, Notice::Stopped))
        .await;
    while stopped
        .agent
        .snapshot()
        .await
        .unwrap()
        .jobs
        .iter()
        .any(|job| job.is_running())
    {
        stopped
            .until(|event| matches!(event, Notice::JobFinished { .. }))
            .await;
    }
    let snapshot = stopped.agent.snapshot().await.unwrap();
    assert!(!snapshot.autonomous);
    assert!(snapshot.work.is_none() && snapshot.review.is_none() && snapshot.candidate.is_none());
    let stop_cursor = snapshot
        .record
        .iter()
        .rfind(|entry| matches!(entry.kind, RecordKind::Notice(Notice::Stopped)))
        .unwrap()
        .cursor;
    assert!(
        !snapshot
            .record
            .iter()
            .any(|entry| entry.cursor > stop_cursor
                && matches!(
                    entry.kind,
                    RecordKind::Notice(Notice::JobStarted { .. } | Notice::Reply { .. })
                ))
    );
    println!("PASS: Stop releases both local provider calls without reviving old work");
    stopped
        .post("Thanks. Do not resume the previous work. Reply with exactly WELCOME and finish.")
        .await;
    stopped
        .until(|event| matches!(event, Notice::Finished { .. }))
        .await;
    assert_eq!(stopped.replies.last().map(String::as_str), Some("WELCOME"));
    assert!(!stopped.agent.snapshot().await.unwrap().autonomous);
    stopped.close().await;
    println!("PASS: post-stop conversation stays open without restarting old work");
}

struct DeliveryGates {
    inner: Arc<ModelAdapter>,
    work_claimed: AtomicBool,
    first_work_provider_returned: Arc<AtomicBool>,
    hold_next_review: AtomicBool,
    release_work: Arc<Notify>,
    release_review: Arc<Notify>,
}

impl DeliveryGates {
    fn new(inner: Arc<ModelAdapter>) -> Self {
        Self {
            inner,
            work_claimed: AtomicBool::new(false),
            first_work_provider_returned: Arc::new(AtomicBool::new(false)),
            hold_next_review: AtomicBool::new(false),
            release_work: Arc::new(Notify::new()),
            release_review: Arc::new(Notify::new()),
        }
    }
}

impl ModelPort for DeliveryGates {
    fn infer(
        &self,
        input: ModelInput,
        context: JobContext,
    ) -> Pin<Box<dyn Future<Output = JobOutcome> + Send>> {
        let (gate, returned) = match input.task {
            ModelTask::Work { .. } if !self.work_claimed.swap(true, Ordering::SeqCst) => (
                Some(self.release_work.clone()),
                Some(self.first_work_provider_returned.clone()),
            ),
            ModelTask::ReviewInput { .. }
                if self.hold_next_review.swap(false, Ordering::SeqCst) =>
            {
                (Some(self.release_review.clone()), None)
            }
            _ => (None, None),
        };
        let future = self.inner.infer(input, context);
        Box::pin(async move {
            let outcome = future.await;
            if let Some(returned) = returned {
                returned.store(true, Ordering::SeqCst);
            }
            if outcome.result.is_ok()
                && let Some(gate) = gate
            {
                gate.notified().await;
            }
            outcome
        })
    }
}

struct Session {
    agent: AgentHandle,
    notices: broadcast::Receiver<Notice>,
    replies: Vec<String>,
    started: Instant,
    calls: usize,
}

impl Session {
    fn new(model: Arc<dyn ModelPort>, environment: &ToolEnvironment, config: KernelConfig) -> Self {
        let agent = Runtime::spawn(
            model,
            read_only_tools(environment),
            config,
            RuntimeConfig::default(),
        )
        .unwrap();
        Self {
            notices: agent.subscribe(),
            agent,
            replies: vec![],
            started: Instant::now(),
            calls: 0,
        }
    }

    async fn post(&self, text: &str) -> MessageId {
        let receipt = tokio::time::timeout(Duration::from_secs(2), self.agent.post(text))
            .await
            .expect("input receipt does not wait for a model")
            .unwrap();
        println!(
            "+{:>6.2}s accepted input {}",
            self.started.elapsed().as_secs_f64(),
            receipt.id.0
        );
        receipt.id
    }

    async fn until(&mut self, predicate: impl Fn(&Notice) -> bool) -> Notice {
        tokio::time::timeout(Duration::from_secs(180), async {
            loop {
                let notice = self.notices.recv().await.expect("live event consumption");
                let elapsed = self.started.elapsed().as_secs_f64();
                match &notice {
                    Notice::JobStarted { id, request } => {
                        self.calls += 1;
                        assert!(self.calls <= 24, "live run exceeded 24 jobs in one session");
                        println!("+{elapsed:>6.2}s job {} started: {request:?}", id.0);
                    }
                    Notice::JobFinished { id, outcome } => println!(
                        "+{elapsed:>6.2}s job {} finished: {}",
                        id.0,
                        match &outcome.result {
                            Ok(_) => "ok".to_owned(),
                            Err(error) => format!("{:?}: {}", error.kind, error.message),
                        }
                    ),
                    Notice::Reply { text, .. } => {
                        println!("+{elapsed:>6.2}s reply: {text}");
                        self.replies.push(text.clone());
                    }
                    Notice::Error { message } => panic!("live agent error: {message}"),
                    Notice::Paused => panic!("live agent unexpectedly paused"),
                    Notice::Finished { .. } | Notice::Stopped => {
                        println!("+{elapsed:>6.2}s {notice:?}")
                    }
                    Notice::JobProgress { .. } => {}
                }
                if predicate(&notice) {
                    return notice;
                }
            }
        })
        .await
        .expect("expected live event within three minutes")
    }

    async fn reply_to(&mut self, message: MessageId) {
        self.until(
            |event| matches!(event, Notice::Reply { reply_to, .. } if reply_to.contains(&message)),
        )
        .await;
    }

    async fn work_started(&mut self) -> JobId {
        match self
            .until(|event| {
                matches!(
                    event,
                    Notice::JobStarted {
                        request: JobRequest::Work { .. },
                        ..
                    }
                )
            })
            .await
        {
            Notice::JobStarted { id, .. } => id,
            _ => unreachable!(),
        }
    }

    async fn close(self) {
        assert!(
            self.agent
                .shutdown()
                .await
                .unwrap()
                .unresolved_jobs
                .is_empty()
        );
    }
}
