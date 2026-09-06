//! Models and tools share one task registry and one completion path.

use std::{any::Any, collections::HashMap, panic::AssertUnwindSafe, sync::Arc, time::Duration};

use futures_util::FutureExt;
use tokio::{
    sync::{Notify, broadcast, mpsc, oneshot, watch},
    task::{AbortHandle, JoinError, JoinSet},
    time::{Instant, sleep, sleep_until, timeout},
};

use crate::{
    Call, Effect, EffectSummary, Event, ExternalEffect, JobContext, JobError, JobErrorKind, JobId,
    JobOutcome, JobOutput, JobProgress, JobSnapshot, JobState, Kernel, KernelConfig, MessageId,
    MessageReceipt, ModelPort, Notice, Snapshot, StepEvent, ToolEffect, ToolPort, WakeId,
    kernel::KernelError,
};

const COMMAND_CAPACITY: usize = 64;
const NOTICE_CAPACITY: usize = 256;
const EVENT_CAPACITY: usize = 256;

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub shutdown_grace_period: Duration,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            shutdown_grace_period: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("an active Tokio runtime is required")]
    NoTokioRuntime,
    #[error(transparent)]
    Kernel(#[from] KernelError),
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HandleError {
    #[error("the agent runtime has closed")]
    Closed,
    #[error("the agent runtime is shutting down")]
    ShuttingDown,
    #[error("the user-message identifier space is exhausted")]
    MessageIdsExhausted,
    #[error("invalid write resolution: {0}")]
    InvalidResolution(String),
}

#[derive(Clone, Debug)]
pub struct ShutdownReport {
    /// Includes running jobs and completed jobs with unknown external effects.
    pub unresolved_jobs: Vec<JobSnapshot>,
}

/// An atomic baseline and the live steps after it. Reading it never controls the agent.
/// The first received step has sequence + 1; the snapshot includes all earlier steps.
/// On Lagged, call observe again for a fresh baseline. Exact missed steps are not
/// replayed, although the snapshot retains the complete in-memory session record.
pub struct Observation {
    pub snapshot: Snapshot,
    pub sequence: u64,
    pub events: broadcast::Receiver<Arc<StepEvent>>,
}

#[derive(Clone)]
pub struct AgentHandle {
    commands: mpsc::Sender<Command>,
    notices: broadcast::WeakSender<Notice>,
    shutdown_report: watch::Receiver<Option<ShutdownReport>>,
}

impl AgentHandle {
    /// Resolves when the kernel accepts the message, before its work finishes.
    pub async fn post(&self, text: impl Into<String>) -> Result<MessageReceipt, HandleError> {
        let text = text.into();
        self.request(|reply| Command::Post { text, reply }).await
    }

    pub async fn stop(&self) -> Result<(), HandleError> {
        self.request(Command::Stop).await
    }

    pub async fn snapshot(&self) -> Result<Snapshot, HandleError> {
        self.request(Command::Snapshot).await
    }

    /// Submit an externally verified result for a write previously reported as
    /// Unknown. This is a host control, never a conclusion inferred by a model.
    /// Repeating the same confirmed result is harmless; changing it is rejected.
    pub async fn resolve_write(&self, id: JobId, outcome: JobOutcome) -> Result<(), HandleError> {
        self.request(|reply| Command::ResolveWrite { id, outcome, reply })
            .await
    }

    /// Subscribe to complete kernel transitions without a snapshot/subscription race.
    /// Each receiver is independent and bounded. Slow or dropped observers neither
    /// block execution nor keep the runtime alive. Live steps share their payload;
    /// the complete history is copied only when taking the baseline snapshot.
    pub async fn observe(&self) -> Result<Observation, HandleError> {
        self.request(Command::Observe).await
    }

    pub async fn shutdown(&self) -> Result<ShutdownReport, HandleError> {
        let mut report = self.shutdown_report.clone();
        let finished = report.borrow().is_some();
        if !finished {
            // The actor can finish between this check and send. Its cached
            // report is authoritative even after the command channel closes.
            let _ = self.commands.send(Command::Shutdown).await;
        }
        loop {
            if let Some(report) = report.borrow_and_update().clone() {
                return Ok(report);
            }
            report.changed().await.map_err(|_| HandleError::Closed)?;
        }
    }

    /// Slow subscribers may receive `RecvError::Lagged`, but cannot block the
    /// agent. The snapshot retains its complete in-memory history.
    pub fn subscribe(&self) -> broadcast::Receiver<Notice> {
        match self.notices.upgrade() {
            Some(notices) => notices.subscribe(),
            None => broadcast::channel(1).1,
        }
    }

    async fn request<T>(
        &self,
        command: impl FnOnce(Reply<T>) -> Command,
    ) -> Result<T, HandleError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(command(reply))
            .await
            .map_err(|_| HandleError::Closed)?;
        response.await.map_err(|_| HandleError::Closed)?
    }
}

pub struct Runtime;

impl Runtime {
    pub fn spawn(
        model: Arc<dyn ModelPort>,
        tools: Vec<Arc<dyn ToolPort>>,
        kernel_config: KernelConfig,
        runtime_config: RuntimeConfig,
    ) -> Result<AgentHandle, RuntimeError> {
        let executor =
            tokio::runtime::Handle::try_current().map_err(|_| RuntimeError::NoTokioRuntime)?;
        let mut registry = HashMap::new();
        let mut specifications = Vec::with_capacity(tools.len());
        for tool in tools {
            let specification = tool.specification();
            registry.insert(specification.name.clone(), (tool, specification.effect));
            specifications.push(specification);
        }
        let kernel = Kernel::new(kernel_config, specifications)?;
        let (commands, command_rx) = mpsc::channel(COMMAND_CAPACITY);
        let (notices, _) = broadcast::channel(NOTICE_CAPACITY);
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let (shutdown_report, shutdown_rx) = watch::channel(None);
        let handle = AgentHandle {
            commands,
            notices: notices.downgrade(),
            shutdown_report: shutdown_rx,
        };
        executor.spawn(
            Actor {
                kernel,
                model,
                registry,
                config: runtime_config,
                commands: command_rx,
                notices,
                events,
                event_sequence: 0,
                started: Instant::now(),
                shutdown_report,
                tasks: JoinSet::new(),
                running: HashMap::new(),
                wakes: HashMap::new(),
                progress_ready: Arc::new(Notify::new()),
                next_message_id: Some(1),
            }
            .run(),
        );
        Ok(handle)
    }
}

type Reply<T> = oneshot::Sender<Result<T, HandleError>>;

enum Command {
    Post {
        text: String,
        reply: Reply<MessageReceipt>,
    },
    Stop(Reply<()>),
    Snapshot(Reply<Snapshot>),
    Observe(Reply<Observation>),
    ResolveWrite {
        id: JobId,
        outcome: JobOutcome,
        reply: Reply<()>,
    },
    Shutdown,
}

struct RunningJob {
    task: AbortHandle,
    cancel: watch::Sender<bool>,
    progress: watch::Receiver<Option<JobProgress>>,
    last_progress: Option<JobProgress>,
    failure_effect: ExternalEffect,
}

struct Actor {
    kernel: Kernel,
    model: Arc<dyn ModelPort>,
    registry: HashMap<String, (Arc<dyn ToolPort>, ToolEffect)>,
    config: RuntimeConfig,
    commands: mpsc::Receiver<Command>,
    notices: broadcast::Sender<Notice>,
    events: broadcast::Sender<Arc<StepEvent>>,
    event_sequence: u64,
    started: Instant,
    shutdown_report: watch::Sender<Option<ShutdownReport>>,
    tasks: JoinSet<Event>,
    running: HashMap<JobId, RunningJob>,
    wakes: HashMap<WakeId, AbortHandle>,
    progress_ready: Arc<Notify>,
    next_message_id: Option<u64>,
}

impl Actor {
    async fn run(mut self) {
        let mut shutdown: Option<Instant> = None;
        let mut commands_open = true;
        loop {
            if shutdown.is_some() && self.running.is_empty() {
                break;
            }
            let deadline = shutdown.unwrap_or_else(Instant::now);
            tokio::select! {
                command = self.commands.recv(), if commands_open => {
                    match command {
                        Some(Command::Post { text, reply }) => {
                            let result = if shutdown.is_some() {
                                Err(HandleError::ShuttingDown)
                            } else {
                                self.post(text)
                            };
                            let _ = reply.send(result);
                        }
                        Some(Command::Stop(reply)) => {
                            if shutdown.is_none() { self.apply(Event::Stop); }
                            let _ = reply.send(Ok(()));
                        }
                        Some(Command::Snapshot(reply)) => {
                            let _ = reply.send(Ok(self.kernel.snapshot()));
                        }
                        Some(Command::Observe(reply)) => {
                            // No step can occur between these reads and subscription.
                            let _ = reply.send(Ok(Observation {
                                snapshot: self.kernel.snapshot(),
                                sequence: self.event_sequence,
                                events: self.events.subscribe(),
                            }));
                        }
                        Some(Command::ResolveWrite { id, outcome, reply }) => {
                            let result = if shutdown.is_some() {
                                Err(HandleError::ShuttingDown)
                            } else {
                                self.resolve_write(id, outcome)
                            };
                            let _ = reply.send(result);
                        }
                        Some(Command::Shutdown) => {
                            shutdown.get_or_insert_with(|| self.begin_shutdown());
                        }
                        None => {
                            commands_open = false;
                            shutdown.get_or_insert_with(|| self.begin_shutdown());
                        }
                    }
                }
                completed = self.tasks.join_next(), if !self.tasks.is_empty() => {
                    if let Some(completed) = completed { self.completed(completed); }
                }
                () = self.progress_ready.notified(), if !self.running.is_empty() => {
                    self.flush_progress();
                }
                () = sleep_until(deadline), if shutdown.is_some() => break,
            }
        }

        // A result and the shutdown deadline can become ready together. Keep
        // already-available outcomes before taking the final snapshot.
        while let Some(completed) = self.tasks.try_join_next() {
            self.completed(completed);
        }
        self.flush_progress();
        let report = ShutdownReport {
            unresolved_jobs: self
                .kernel
                .snapshot()
                .jobs
                .into_iter()
                .filter(JobSnapshot::is_unresolved)
                .collect(),
        };
        // Dropping a local future proves nothing about external effects. The
        // report therefore preserves jobs still unresolved at the deadline.
        self.tasks.abort_all();
        self.shutdown_report.send_replace(Some(report));
    }

    fn post(&mut self, text: String) -> Result<MessageReceipt, HandleError> {
        let value = self
            .next_message_id
            .ok_or(HandleError::MessageIdsExhausted)?;
        self.next_message_id = value.checked_add(1);
        let id = MessageId(value);
        let record_cursor = self.kernel.record_cursor() + 1;
        self.apply(Event::UserMessage { id, text });
        Ok(MessageReceipt { id, record_cursor })
    }

    fn begin_shutdown(&mut self) -> Instant {
        self.apply(Event::Stop);
        Instant::now() + self.config.shutdown_grace_period
    }

    fn resolve_write(&mut self, id: JobId, outcome: JobOutcome) -> Result<(), HandleError> {
        let invalid =
            |reason: &str| HandleError::InvalidResolution(format!("job {}: {reason}", id.0));
        let snapshot = self.kernel.snapshot();
        let job = snapshot
            .jobs
            .iter()
            .find(|job| job.id == id)
            .ok_or_else(|| invalid("job does not exist"))?;
        if !job.external_write {
            return Err(invalid("job is not an external write"));
        }
        let JobState::Finished(previous) = &job.state else {
            return Err(invalid("write is still running; await its actual outcome"));
        };
        if outcome.external_effect == ExternalEffect::Unknown {
            return Err(invalid("confirmation must establish Applied or None"));
        }
        if !matches!(&outcome.result, Ok(JobOutput::Artifact(_)) | Err(_)) {
            return Err(invalid(
                "confirmation must contain an artifact or error, never model instructions",
            ));
        }
        if previous.external_effect != ExternalEffect::Unknown {
            return if *previous == outcome {
                Ok(())
            } else {
                Err(invalid("a confirmed result cannot be changed"))
            };
        }
        self.apply(Event::JobFinished { id, outcome });
        Ok(())
    }

    fn completed(&mut self, completed: Result<Event, JoinError>) {
        let event = match completed {
            Ok(event) => event,
            Err(error) => {
                let Some((&id, running)) = self
                    .running
                    .iter()
                    .find(|(_, running)| running.task.id() == error.id())
                else {
                    // Cancelled timers carry no operation result.
                    return;
                };
                Event::JobFinished {
                    id,
                    outcome: failure(
                        if error.is_cancelled() {
                            JobErrorKind::Cancelled
                        } else {
                            JobErrorKind::Panicked
                        },
                        format!("job task exited: {error}"),
                        running.failure_effect,
                    ),
                }
            }
        };
        match &event {
            Event::JobFinished { id, .. } => {
                self.flush_progress();
                self.running.remove(id);
            }
            Event::Wake { id } => {
                self.wakes.remove(id);
            }
            _ => {}
        }
        self.apply(event);
    }

    fn flush_progress(&mut self) {
        let mut updates = Vec::new();
        for (id, running) in &mut self.running {
            let progress = running.progress.borrow_and_update().clone();
            if progress != running.last_progress {
                running.last_progress = progress.clone();
                if let Some(progress) = progress {
                    updates.push(Event::JobProgress { id: *id, progress });
                }
            }
        }
        for event in updates {
            self.apply(event);
        }
    }

    fn apply(&mut self, event: Event) {
        let cursor = self.kernel.record_cursor();
        let observed =
            (self.events.receiver_count() > 0).then(|| (event.clone(), self.started.elapsed()));
        let effects = self.kernel.step(event);
        self.event_sequence = self
            .event_sequence
            .checked_add(1)
            .expect("event sequence exhausted");
        if let Some((event, elapsed)) = observed {
            let step = StepEvent {
                sequence: self.event_sequence,
                elapsed,
                event,
                records: self.kernel.records_since(cursor).to_vec(),
                effects: effects
                    .iter()
                    .map(|effect| self.summarize(effect))
                    .collect(),
            };
            let _ = self.events.send(Arc::new(step));
        }
        for effect in effects {
            self.dispatch(effect);
        }
    }

    fn summarize(&self, effect: &Effect) -> EffectSummary {
        match effect {
            Effect::Start { id, timeout, .. } => {
                let job = self.kernel.job(*id);
                EffectSummary::Start {
                    id: *id,
                    request: job.request.clone(),
                    record_cursor: job.record_cursor,
                    revision: job.revision,
                    generation: job.generation,
                    timeout: *timeout,
                }
            }
            Effect::RequestCancel { id } => EffectSummary::RequestCancel { id: *id },
            Effect::WakeAfter { id, delay } => EffectSummary::WakeAfter {
                id: *id,
                delay: *delay,
            },
            Effect::CancelWake { id } => EffectSummary::CancelWake { id: *id },
            Effect::Publish(notice) => EffectSummary::Publish(notice.clone()),
        }
    }

    fn dispatch(&mut self, effect: Effect) {
        match effect {
            Effect::Start { id, call, timeout } => self.start_job(id, call, timeout),
            Effect::RequestCancel { id } => {
                if let Some(running) = self.running.get(&id) {
                    running.cancel.send_replace(true);
                }
            }
            Effect::WakeAfter { id, delay } => {
                let task = self.tasks.spawn(async move {
                    sleep(delay).await;
                    Event::Wake { id }
                });
                if let Some(previous) = self.wakes.insert(id, task) {
                    previous.abort();
                }
            }
            Effect::CancelWake { id } => {
                if let Some(task) = self.wakes.remove(&id) {
                    task.abort();
                }
            }
            Effect::Publish(notice) => {
                let _ = self.notices.send(notice);
            }
        }
    }

    fn start_job(&mut self, id: JobId, call: Call, deadline: Option<Duration>) {
        let model = self.model.clone();
        let tool = match &call {
            Call::Tool(call) => self.registry.get(&call.name).cloned(),
            Call::Model(_) => None,
        };
        let failure_effect = match &tool {
            Some((_, ToolEffect::ExternalWrite)) => ExternalEffect::Unknown,
            _ => ExternalEffect::None,
        };
        let (progress_tx, progress_rx) = watch::channel(None);
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let cancel_on_timeout = cancel_tx.clone();
        let context = JobContext::new(progress_tx, self.progress_ready.clone(), cancel_rx);
        let mut cancellation = context.clone();

        // Even constructing the port future happens inside the supervised task.
        // Ports must yield while waiting; blocking work belongs on its own thread.
        let task = self.tasks.spawn(async move {
            let execution = AssertUnwindSafe(async move {
                match call {
                    Call::Model(input) => model.infer(input, context).await,
                    Call::Tool(call) => match tool {
                        Some((tool, _)) => tool.run(call.arguments, context).await,
                        None => {
                            JobOutcome::failed(format!("tool {:?} is not registered", call.name))
                        }
                    },
                }
            })
            .catch_unwind();
            let completion = async {
                let result = match deadline {
                    Some(duration) => timeout(duration, execution).await,
                    None => Ok(execution.await),
                };
                match result {
                    Ok(Ok(outcome)) => outcome,
                    Ok(Err(panic)) => failure(
                        JobErrorKind::Panicked,
                        format!("job panicked: {}", panic_message(panic)),
                        failure_effect,
                    ),
                    Err(_) => {
                        cancel_on_timeout.send_replace(true);
                        failure(
                            JobErrorKind::TimedOut,
                            format!(
                                "job timed out after {:?}",
                                deadline.expect("timeout has a deadline")
                            ),
                            failure_effect,
                        )
                    }
                }
            };
            let outcome = if failure_effect == ExternalEffect::None {
                // Abandon local read-only waits even if the port ignores the
                // signal. This does not promise the remote provider stopped.
                if cancellation.cancellation_requested() {
                    cancelled()
                } else {
                    tokio::select! {
                        biased;
                        outcome = completion => outcome,
                        () = cancellation.wait_for_cancellation() => cancelled(),
                    }
                }
            } else {
                // Dropping a write could lose its real outcome. Cancellation
                // remains cooperative; timeout still reports Unknown.
                completion.await
            };
            Event::JobFinished { id, outcome }
        });
        self.running.insert(
            id,
            RunningJob {
                task,
                cancel: cancel_tx,
                progress: progress_rx,
                last_progress: None,
                failure_effect,
            },
        );
    }
}

fn failure(kind: JobErrorKind, message: String, external_effect: ExternalEffect) -> JobOutcome {
    JobOutcome {
        result: Err(JobError { kind, message }),
        external_effect,
    }
}

fn cancelled() -> JobOutcome {
    failure(
        JobErrorKind::Cancelled,
        "local read-only wait was cancelled".into(),
        ExternalEffect::None,
    )
}

fn panic_message(panic: Box<dyn Any + Send>) -> String {
    if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = panic.downcast_ref::<&str>() {
        (*message).to_owned()
    } else {
        "non-string panic payload".to_owned()
    }
}
