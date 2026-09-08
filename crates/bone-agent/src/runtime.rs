//! Models and tools share one call registry and one completion path.

use std::{collections::HashMap, panic::AssertUnwindSafe, sync::Arc, time::Duration};

use futures_util::FutureExt;
use tokio::{
    sync::{Notify, broadcast, mpsc, oneshot, watch},
    task::{AbortHandle, JoinError, JoinSet},
    time::{Instant, sleep, sleep_until, timeout},
};

use crate::{
    AdmissionError, Call, CallContext, CallError, CallErrorKind, CallId, CallOutcome, CallProgress,
    CallSnapshot, Effect, EffectSummary, Event, ExternalEffect, Input, InputId, InputReceipt,
    Kernel, KernelConfig, ModelPort, Notice, Snapshot, StepEvent, ToolEffect, ToolPort, WakeId,
    kernel::KernelError,
};

const COMMAND_CAPACITY: usize = 64;
const CONTROL_CAPACITY: usize = 16;
const NOTICE_CAPACITY: usize = 256;
const EVENT_CAPACITY: usize = 256;
const PANIC_MESSAGE: &str = "call panicked";

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
    #[error(transparent)]
    Admission(#[from] AdmissionError),
    #[error("invalid input retry: {0}")]
    InvalidRetry(String),
    #[error("invalid write resolution: {0}")]
    InvalidResolution(String),
}

#[derive(Clone, Debug)]
pub struct ShutdownReport {
    /// Includes running calls and completed calls with unknown external effects.
    pub unresolved_calls: Vec<CallSnapshot>,
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
    controls: mpsc::Sender<Control>,
    notices: broadcast::WeakSender<Notice>,
    shutdown_report: watch::Receiver<Option<ShutdownReport>>,
}

impl AgentHandle {
    /// Resolves on acceptance, before work finishes. Reposting the same input
    /// returns its original receipt. Busy means this input was not accepted.
    /// Explicit clarification replies use the independent control queue.
    pub async fn post(&self, input: Input) -> Result<InputReceipt, HandleError> {
        if input.reply_to.is_some() {
            Self::request(&self.controls, |reply| Control::Clarify { input, reply }).await
        } else {
            Self::request(&self.commands, |reply| Command::Post { input, reply }).await
        }
    }

    pub async fn stop(&self) -> Result<(), HandleError> {
        Self::request(&self.controls, Control::Stop).await
    }

    /// Explicitly retry an accepted input whose routing failed. This does not
    /// create a new input or silently retry an unresolved external write.
    pub async fn retry_input(&self, id: InputId) -> Result<(), HandleError> {
        Self::request(&self.controls, |reply| Control::RetryInput { id, reply }).await
    }

    pub async fn snapshot(&self) -> Result<Snapshot, HandleError> {
        Self::request(&self.commands, Command::Snapshot).await
    }

    /// Submit an externally verified result for a write previously reported as
    /// Unknown. This is a host control, never a conclusion inferred by a model.
    /// Repeating the same confirmed result is harmless; changing it is rejected.
    pub async fn resolve_write(&self, id: CallId, outcome: CallOutcome) -> Result<(), HandleError> {
        Self::request(&self.controls, |reply| Control::ResolveWrite {
            id,
            outcome,
            reply,
        })
        .await
    }

    /// Subscribe to complete kernel transitions without a snapshot/subscription race.
    /// Each receiver is independent and bounded. Slow or dropped observers neither
    /// block execution nor keep the runtime alive. Live steps share their payload;
    /// the complete history is copied only when taking the baseline snapshot.
    pub async fn observe(&self) -> Result<Observation, HandleError> {
        Self::request(&self.commands, Command::Observe).await
    }

    pub async fn shutdown(&self) -> Result<ShutdownReport, HandleError> {
        let mut report = self.shutdown_report.clone();
        let finished = report.borrow().is_some();
        if !finished {
            // The actor can finish between this check and send. Its cached
            // report is authoritative even after the command channel closes.
            let _ = self.controls.send(Control::Shutdown).await;
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

    async fn request<T, C>(
        sender: &mpsc::Sender<C>,
        command: impl FnOnce(Reply<T>) -> C,
    ) -> Result<T, HandleError> {
        let (reply, response) = oneshot::channel();
        sender
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
        let (controls, control_rx) = mpsc::channel(CONTROL_CAPACITY);
        let (notices, _) = broadcast::channel(NOTICE_CAPACITY);
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let (shutdown_report, shutdown_rx) = watch::channel(None);
        let handle = AgentHandle {
            commands,
            controls,
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
                controls: control_rx,
                notices,
                events,
                event_sequence: 0,
                started: Instant::now(),
                shutdown_report,
                tasks: JoinSet::new(),
                running: HashMap::new(),
                wakes: HashMap::new(),
                progress_ready: Arc::new(Notify::new()),
            }
            .run(),
        );
        Ok(handle)
    }
}

type Reply<T> = oneshot::Sender<Result<T, HandleError>>;

enum Command {
    Post {
        input: Input,
        reply: Reply<InputReceipt>,
    },
    Snapshot(Reply<Snapshot>),
    Observe(Reply<Observation>),
}

enum Control {
    Stop(Reply<()>),
    Clarify {
        input: Input,
        reply: Reply<InputReceipt>,
    },
    RetryInput {
        id: InputId,
        reply: Reply<()>,
    },
    ResolveWrite {
        id: CallId,
        outcome: CallOutcome,
        reply: Reply<()>,
    },
    Shutdown,
}

struct RunningCall {
    task: AbortHandle,
    cancel: watch::Sender<bool>,
    progress: watch::Receiver<Option<CallProgress>>,
    last_progress: Option<CallProgress>,
    failure_effect: ExternalEffect,
}

struct Actor {
    kernel: Kernel,
    model: Arc<dyn ModelPort>,
    registry: HashMap<String, (Arc<dyn ToolPort>, ToolEffect)>,
    config: RuntimeConfig,
    commands: mpsc::Receiver<Command>,
    controls: mpsc::Receiver<Control>,
    notices: broadcast::Sender<Notice>,
    events: broadcast::Sender<Arc<StepEvent>>,
    event_sequence: u64,
    started: Instant,
    shutdown_report: watch::Sender<Option<ShutdownReport>>,
    tasks: JoinSet<Event>,
    running: HashMap<CallId, RunningCall>,
    wakes: HashMap<WakeId, AbortHandle>,
    progress_ready: Arc<Notify>,
}

impl Actor {
    async fn run(mut self) {
        let mut shutdown: Option<Instant> = None;
        let mut commands_open = true;
        let mut controls_open = true;
        loop {
            if shutdown.is_some() && self.running.is_empty() {
                break;
            }
            let deadline = shutdown.unwrap_or_else(Instant::now);
            tokio::select! {
                command = self.commands.recv(), if commands_open => {
                    match command {
                        Some(Command::Post { input, reply }) => {
                            let result = if shutdown.is_some() {
                                Err(HandleError::ShuttingDown)
                            } else {
                                self.post(input)
                            };
                            let _ = reply.send(result);
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
                        None => {
                            commands_open = false;
                            shutdown.get_or_insert_with(|| self.begin_shutdown());
                        }
                    }
                }
                control = self.controls.recv(), if controls_open => {
                    match control {
                        Some(Control::Stop(reply)) => {
                            if shutdown.is_none() { self.apply(Event::Stop); }
                            let _ = reply.send(Ok(()));
                        }
                        Some(Control::Clarify { input, reply }) => {
                            let result = if shutdown.is_some() {
                                Err(HandleError::ShuttingDown)
                            } else {
                                self.post(input)
                            };
                            let _ = reply.send(result);
                        }
                        Some(Control::RetryInput { id, reply }) => {
                            let result = if shutdown.is_some() {
                                Err(HandleError::ShuttingDown)
                            } else {
                                self.retry_input(id)
                            };
                            let _ = reply.send(result);
                        }
                        Some(Control::ResolveWrite { id, outcome, reply }) => {
                            let result = if shutdown.is_some() {
                                Err(HandleError::ShuttingDown)
                            } else {
                                self.resolve_write(id, outcome)
                            };
                            let _ = reply.send(result);
                        }
                        Some(Control::Shutdown) => {
                            shutdown.get_or_insert_with(|| self.begin_shutdown());
                        }
                        None => {
                            controls_open = false;
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
            unresolved_calls: self
                .kernel
                .snapshot()
                .calls
                .into_iter()
                .filter(CallSnapshot::is_unresolved)
                .collect(),
        };
        // Dropping a local future proves nothing about external effects. The
        // report therefore preserves calls still unresolved at the deadline.
        self.tasks.abort_all();
        self.shutdown_report.send_replace(Some(report));
    }

    fn post(&mut self, input: Input) -> Result<InputReceipt, HandleError> {
        if let Some(receipt) = self.kernel.admit(&input)? {
            return Ok(receipt);
        }
        let id = input.id;
        self.apply(Event::Input(input));
        Ok(self
            .kernel
            .receipt(id)
            .expect("an admitted input has a receipt"))
    }

    fn retry_input(&mut self, id: InputId) -> Result<(), HandleError> {
        self.kernel
            .validate_retry(id)
            .map_err(HandleError::InvalidRetry)?;
        self.apply(Event::RetryInput { id });
        Ok(())
    }

    fn begin_shutdown(&mut self) -> Instant {
        self.apply(Event::Stop);
        Instant::now() + self.config.shutdown_grace_period
    }

    fn resolve_write(&mut self, id: CallId, outcome: CallOutcome) -> Result<(), HandleError> {
        if self
            .kernel
            .validate_resolution(id, &outcome)
            .map_err(HandleError::InvalidResolution)?
        {
            self.apply(Event::WriteResolved { id, outcome });
        }
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
                // JoinError can contain the original panic payload. Only its
                // classification belongs in shared state or model context.
                let (kind, message) = if error.is_cancelled() {
                    (CallErrorKind::Cancelled, "call task was cancelled")
                } else {
                    (CallErrorKind::Panicked, PANIC_MESSAGE)
                };
                Event::CallFinished {
                    id,
                    outcome: failure(kind, message.into(), running.failure_effect),
                }
            }
        };
        match &event {
            Event::CallFinished { id, .. } => {
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
                    updates.push(Event::CallProgress { id: *id, progress });
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
                let call = self.kernel.call(*id);
                EffectSummary::Start {
                    id: *id,
                    job: call.job,
                    request: call.request.clone(),
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
            Effect::Start { id, call, timeout } => self.start_call(id, call, timeout),
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

    fn start_call(&mut self, id: CallId, call: Call, deadline: Option<Duration>) {
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
        let context = CallContext::new(progress_tx, self.progress_ready.clone(), cancel_rx);
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
                            CallOutcome::failed(format!("tool {:?} is not registered", call.name))
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
                    Ok(Err(_)) => failure(
                        CallErrorKind::Panicked,
                        PANIC_MESSAGE.into(),
                        failure_effect,
                    ),
                    Err(_) => {
                        cancel_on_timeout.send_replace(true);
                        failure(
                            CallErrorKind::TimedOut,
                            format!(
                                "call timed out after {:?}",
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
            Event::CallFinished { id, outcome }
        });
        self.running.insert(
            id,
            RunningCall {
                task,
                cancel: cancel_tx,
                progress: progress_rx,
                last_progress: None,
                failure_effect,
            },
        );
    }
}

fn failure(kind: CallErrorKind, message: String, external_effect: ExternalEffect) -> CallOutcome {
    CallOutcome {
        result: Err(CallError { kind, message }),
        external_effect,
    }
}

fn cancelled() -> CallOutcome {
    failure(
        CallErrorKind::Cancelled,
        "local read-only wait was cancelled".into(),
        ExternalEffect::None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CallOutput, CallRequest, CallState, JobChange, JobSpec, KernelDecision, ModelInput,
        ModelTask, Next, ToolCall, ToolSpec, WorkProposal,
    };
    use futures_util::future::BoxFuture;
    use serde_json::{Value, json};

    struct Request {
        input: ModelInput,
        context: CallContext,
        reply: oneshot::Sender<CallOutcome>,
    }

    struct ControlledModel(mpsc::UnboundedSender<Request>);

    impl ModelPort for ControlledModel {
        fn infer(
            &self,
            input: ModelInput,
            context: CallContext,
        ) -> BoxFuture<'static, CallOutcome> {
            let (reply, response) = oneshot::channel();
            self.0
                .send(Request {
                    input,
                    context,
                    reply,
                })
                .unwrap();
            // Ignore cancellation so these tests exercise runtime supervision.
            Box::pin(async move { response.await.unwrap() })
        }
    }

    struct ToolRequest {
        context: CallContext,
        reply: oneshot::Sender<CallOutcome>,
    }

    struct ControlledWrite(mpsc::UnboundedSender<ToolRequest>);

    impl ToolPort for ControlledWrite {
        fn specification(&self) -> ToolSpec {
            ToolSpec {
                name: "write".into(),
                description: "controlled external write".into(),
                parameters: json!({"type": "object"}),
                effect: ToolEffect::ExternalWrite,
            }
        }

        fn run(&self, _: Value, context: CallContext) -> BoxFuture<'static, CallOutcome> {
            let (reply, response) = oneshot::channel();
            self.0.send(ToolRequest { context, reply }).unwrap();
            Box::pin(async move { response.await.unwrap() })
        }
    }

    fn session(
        config: KernelConfig,
        tools: Vec<Arc<dyn ToolPort>>,
    ) -> (AgentHandle, mpsc::UnboundedReceiver<Request>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let handle = Runtime::spawn(
            Arc::new(ControlledModel(sender)),
            tools,
            config,
            RuntimeConfig::default(),
        )
        .unwrap();
        (handle, receiver)
    }

    async fn receive<T>(receiver: &mut mpsc::UnboundedReceiver<T>) -> T {
        timeout(Duration::from_secs(2), receiver.recv())
            .await
            .unwrap()
            .unwrap()
    }

    async fn step(
        observation: &mut Observation,
        matches: impl Fn(&Event) -> bool,
    ) -> Arc<StepEvent> {
        timeout(Duration::from_secs(2), async {
            loop {
                let step = observation.events.recv().await.unwrap();
                if matches(&step.event) {
                    return step;
                }
            }
        })
        .await
        .unwrap()
    }

    async fn start_write(handle: &AgentHandle, requests: &mut mpsc::UnboundedReceiver<Request>) {
        let input = Input::new(InputId(1), "perform the authorized write");
        handle.post(input.clone()).await.unwrap();
        let routing = receive(requests).await;
        assert!(matches!(routing.input.task, ModelTask::Kernel { .. }));
        routing
            .reply
            .send(CallOutcome::kernel(KernelDecision {
                changes: vec![JobChange::Create(JobSpec {
                    goal: input.text,
                    inputs: vec![input.id],
                    parent: None,
                    references: vec![],
                })],
                ..KernelDecision::default()
            }))
            .unwrap();
        let work = receive(requests).await;
        assert!(matches!(work.input.task, ModelTask::Work { .. }));
        work.reply
            .send(CallOutcome::work(WorkProposal {
                operation: Some(ToolCall::new("write", json!({}))),
                next: Next::Wait {
                    reconsider_after: None,
                },
                ..WorkProposal::default()
            }))
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn admission_rejects_without_recording_and_duplicates_keep_the_original_receipt() {
        let (handle, mut requests) = session(
            KernelConfig {
                input_capacity: 1,
                ..KernelConfig::default()
            },
            vec![],
        );
        let input = Input::new(InputId(91), "held input");
        let receipt = handle.post(input.clone()).await.unwrap();
        let mut held = receive(&mut requests).await;
        let before = handle.observe().await.unwrap();
        assert_eq!(handle.post(input.clone()).await.unwrap(), receipt);
        assert_eq!(
            handle.post(Input::new(input.id, "different")).await,
            Err(HandleError::Admission(AdmissionError::ConflictingInput))
        );
        assert_eq!(
            handle.post(Input::new(InputId(92), "not accepted")).await,
            Err(HandleError::Admission(AdmissionError::Busy))
        );
        assert!(matches!(
            handle.retry_input(input.id).await,
            Err(HandleError::InvalidRetry(_))
        ));
        assert!(matches!(
            handle
                .resolve_write(CallId(999), CallOutcome::artifact(json!(null)))
                .await,
            Err(HandleError::InvalidResolution(_))
        ));
        let after = handle.observe().await.unwrap();
        assert_eq!(after.sequence, before.sequence);
        assert_eq!(after.snapshot, before.snapshot);
        handle.stop().await.unwrap();
        timeout(Duration::from_secs(2), held.reply.closed())
            .await
            .unwrap();
        assert!(handle.shutdown().await.unwrap().unresolved_calls.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn clarification_bypasses_full_transport_and_input_capacity_without_bypassing_admission()
    {
        let (handle, mut requests) = session(
            KernelConfig {
                input_capacity: 1,
                ..KernelConfig::default()
            },
            vec![],
        );
        let original = Input::new(InputId(1), "which task should change?");
        let mut observed = handle.observe().await.unwrap();
        handle.post(original.clone()).await.unwrap();
        receive(&mut requests)
            .await
            .reply
            .send(CallOutcome::kernel(KernelDecision {
                disposition: crate::RoutingDisposition::Clarify {
                    question: "which task?".into(),
                },
                ..KernelDecision::default()
            }))
            .unwrap();
        step(&mut observed, |event| {
            matches!(event, Event::CallFinished { .. })
        })
        .await;
        assert_eq!(
            handle.post(Input::new(InputId(2), "ordinary input")).await,
            Err(HandleError::Admission(AdmissionError::Busy))
        );

        // Keep every ordinary queue slot occupied until the control reply arrives.
        // Reserving slots makes saturation deterministic even as the actor runs.
        let occupied = handle
            .commands
            .reserve_many(COMMAND_CAPACITY)
            .await
            .unwrap();
        assert!(
            handle
                .post(Input::new(InputId(2), "ordinary input"))
                .now_or_never()
                .is_none()
        );
        assert_eq!(
            timeout(
                Duration::from_secs(2),
                handle.post(Input::new(InputId(3), "invalid target").replying_to(InputId(999)))
            )
            .await
            .unwrap(),
            Err(HandleError::Admission(AdmissionError::InvalidReply))
        );
        let clarification = Input::new(InputId(4), "the report").replying_to(original.id);
        let receipt = timeout(Duration::from_secs(2), handle.post(clarification.clone()))
            .await
            .unwrap()
            .unwrap();
        let request = receive(&mut requests).await;
        assert!(
            matches!(&request.input.task, ModelTask::Kernel { inputs, .. }
            if inputs == &vec![original.clone(), clarification.clone()])
        );
        assert_eq!(
            timeout(Duration::from_secs(2), handle.post(clarification.clone()))
                .await
                .unwrap()
                .unwrap(),
            receipt
        );
        assert_eq!(
            timeout(
                Duration::from_secs(2),
                handle
                    .post(Input::new(clarification.id, "different reply").replying_to(original.id))
            )
            .await
            .unwrap(),
            Err(HandleError::Admission(AdmissionError::ConflictingInput))
        );
        drop(occupied);
        let snapshot = handle.snapshot().await.unwrap();
        assert_eq!(
            snapshot
                .inputs
                .iter()
                .map(|entry| entry.input.clone())
                .collect::<Vec<_>>(),
            vec![original, clarification]
        );
        assert!(handle.shutdown().await.unwrap().unresolved_calls.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn clarification_releases_a_held_answer_during_input_drain() {
        let (handle, mut requests) = session(KernelConfig::default(), vec![]);
        let mut observed = handle.observe().await.unwrap();
        handle
            .post(Input::new(InputId(1), "prepare a report"))
            .await
            .unwrap();
        receive(&mut requests)
            .await
            .reply
            .send(CallOutcome::kernel(KernelDecision {
                changes: vec![JobChange::Create(JobSpec {
                    goal: "prepare a report".into(),
                    inputs: vec![InputId(1)],
                    parent: None,
                    references: vec![],
                })],
                ..KernelDecision::default()
            }))
            .unwrap();
        let work = receive(&mut requests).await;
        handle
            .post(Input::new(InputId(2), "a correction"))
            .await
            .unwrap();
        let routing = receive(&mut requests).await;
        work.reply
            .send(CallOutcome::work(WorkProposal {
                reply: Some("the held report".into()),
                next: Next::Finish,
                ..WorkProposal::default()
            }))
            .unwrap();
        step(&mut observed, |event| {
            matches!(
                event,
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
        routing
            .reply
            .send(CallOutcome::kernel(KernelDecision {
                disposition: crate::RoutingDisposition::Clarify {
                    question: "which report?".into(),
                },
                ..KernelDecision::default()
            }))
            .unwrap();
        step(&mut observed, |event| {
            matches!(
                event,
                Event::CallFinished {
                    outcome: CallOutcome {
                        result: Ok(CallOutput::Kernel(_)),
                        ..
                    },
                    ..
                }
            )
        })
        .await;
        assert_eq!(
            handle.post(Input::new(InputId(3), "ordinary input")).await,
            Err(HandleError::Admission(AdmissionError::Busy))
        );
        let occupied = handle
            .commands
            .reserve_many(COMMAND_CAPACITY)
            .await
            .unwrap();
        timeout(
            Duration::from_secs(2),
            handle.post(Input::new(InputId(4), "no changes needed").replying_to(InputId(2))),
        )
        .await
        .unwrap()
        .unwrap();
        let clarified = receive(&mut requests).await;
        clarified
            .reply
            .send(CallOutcome::kernel(KernelDecision::default()))
            .unwrap();
        let delivered = step(&mut observed, |event| {
            matches!(event, Event::CallFinished { .. })
        })
        .await;
        assert!(delivered.effects.iter().any(|effect| matches!(effect,
            EffectSummary::Publish(Notice::Reply { text, .. }) if text == "the held report")));
        drop(occupied);
        handle
            .post(Input::new(InputId(3), "ordinary input"))
            .await
            .unwrap();
        assert!(handle.shutdown().await.unwrap().unresolved_calls.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn failed_routing_requires_explicit_retry_and_stop_cancels_noncooperative_calls() {
        let (handle, mut requests) = session(KernelConfig::default(), vec![]);
        let mut observed = handle.observe().await.unwrap();
        let input = Input::new(InputId(7), "keep this original input");
        let receipt = handle.post(input.clone()).await.unwrap();
        let first = receive(&mut requests).await;
        first
            .reply
            .send(CallOutcome::failed("temporary provider error"))
            .unwrap();
        step(&mut observed, |event| {
            matches!(event, Event::CallFinished { .. })
        })
        .await;
        let before = handle.observe().await.unwrap();
        assert_eq!(handle.post(input.clone()).await.unwrap(), receipt);
        assert_eq!(handle.observe().await.unwrap().sequence, before.sequence);
        handle.retry_input(input.id).await.unwrap();
        let mut retried = receive(&mut requests).await;
        assert!(
            matches!(retried.input.task, ModelTask::Kernel { inputs, .. } if inputs == vec![input])
        );
        for index in 0..10_000 {
            assert!(retried.context.report_progress(CallProgress {
                message: format!("routing {index}"),
                percent: None,
            }));
        }
        let progress = step(&mut observed, |event| {
            matches!(event, Event::CallProgress { .. })
        })
        .await;
        assert!(
            matches!(&progress.event, Event::CallProgress { progress, .. } if progress.message == "routing 9999")
        );
        handle.stop().await.unwrap();
        timeout(Duration::from_secs(2), retried.reply.closed())
            .await
            .unwrap();
        assert!(retried.context.cancellation_requested());
        let (first, second) = tokio::join!(handle.shutdown(), handle.shutdown());
        let report = first.unwrap();
        assert!(report.unresolved_calls.is_empty());
        assert_eq!(second.unwrap().unresolved_calls, report.unresolved_calls);
        assert_eq!(
            handle.shutdown().await.unwrap().unresolved_calls,
            report.unresolved_calls
        );
    }

    #[tokio::test(start_paused = true)]
    async fn observation_has_an_atomic_baseline_and_lag_does_not_block_controls() {
        let (handle, mut requests) = session(KernelConfig::default(), vec![]);
        let mut observed = handle.observe().await.unwrap();
        let input = Input::new(InputId(1), "held");
        let receipt = handle.post(input.clone()).await.unwrap();
        let first = step(&mut observed, |event| matches!(event, Event::Input(_))).await;
        assert_eq!(first.sequence, observed.sequence + 1);
        assert_eq!(first.event, Event::Input(input));
        assert!(
            first
                .records
                .iter()
                .any(|entry| entry.cursor == receipt.record_cursor)
        );
        assert!(first.effects.iter().any(|effect| matches!(
            effect,
            EffectSummary::Start {
                job: None,
                request: CallRequest::Kernel { .. },
                ..
            }
        )));
        let _held = receive(&mut requests).await;
        for _ in 0..=EVENT_CAPACITY {
            handle.stop().await.unwrap();
        }
        assert!(matches!(
            observed.events.recv().await,
            Err(broadcast::error::RecvError::Lagged(_))
        ));
        let mut recovered = handle.observe().await.unwrap();
        handle.stop().await.unwrap();
        let next = step(&mut recovered, |event| matches!(event, Event::Stop)).await;
        assert_eq!(next.sequence, recovered.sequence + 1);
        handle.shutdown().await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_a_write_preserves_its_actual_outcome_and_call_identity() {
        let (sender, mut writes) = mpsc::unbounded_channel();
        let (handle, mut requests) = session(
            KernelConfig::default(),
            vec![Arc::new(ControlledWrite(sender))],
        );
        let mut observed = handle.observe().await.unwrap();
        start_write(&handle, &mut requests).await;
        let write = receive(&mut writes).await;
        let snapshot = handle.snapshot().await.unwrap();
        let call = snapshot
            .calls
            .iter()
            .find(|call| call.external_write)
            .unwrap();
        let id = call.id;
        let job = call.job.unwrap();
        assert!(
            snapshot
                .calls
                .iter()
                .any(|other| other.job == Some(job) && other.id != id)
        );
        handle.stop().await.unwrap();
        assert!(write.context.cancellation_requested());
        assert!(!write.reply.is_closed());
        let outcome = CallOutcome {
            result: Ok(CallOutput::Artifact(json!("applied"))),
            external_effect: ExternalEffect::Applied,
        };
        write.reply.send(outcome.clone()).unwrap();
        step(
            &mut observed,
            |event| matches!(event, Event::CallFinished { id: finished, .. } if *finished == id),
        )
        .await;
        let snapshot = handle.snapshot().await.unwrap();
        assert_eq!(
            snapshot
                .calls
                .iter()
                .find(|call| call.id == id)
                .unwrap()
                .state,
            CallState::Finished(outcome)
        );
        assert!(handle.shutdown().await.unwrap().unresolved_calls.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn only_host_resolution_emits_write_resolved_and_duplicate_confirmation_is_silent() {
        let (sender, mut writes) = mpsc::unbounded_channel();
        let (handle, mut requests) = session(
            KernelConfig::default(),
            vec![Arc::new(ControlledWrite(sender))],
        );
        start_write(&handle, &mut requests).await;
        let write = receive(&mut writes).await;
        let id = handle
            .snapshot()
            .await
            .unwrap()
            .calls
            .iter()
            .find(|call| call.external_write)
            .unwrap()
            .id;
        handle.stop().await.unwrap();
        let mut observed = handle.observe().await.unwrap();
        write
            .reply
            .send(CallOutcome::unknown("remote reply lost"))
            .unwrap();
        step(
            &mut observed,
            |event| matches!(event, Event::CallFinished { id: finished, .. } if *finished == id),
        )
        .await;
        let outcome = CallOutcome::artifact(json!("confirmed absent"));
        handle.resolve_write(id, outcome.clone()).await.unwrap();
        let resolved = step(&mut observed, |event| {
            matches!(event, Event::WriteResolved { .. })
        })
        .await;
        assert_eq!(
            resolved.event,
            Event::WriteResolved {
                id,
                outcome: outcome.clone()
            }
        );
        let baseline = handle.observe().await.unwrap();
        handle.resolve_write(id, outcome).await.unwrap();
        assert_eq!(handle.observe().await.unwrap().sequence, baseline.sequence);
        assert!(matches!(
            handle
                .resolve_write(id, CallOutcome::artifact(json!("different")))
                .await,
            Err(HandleError::InvalidResolution(_))
        ));
        assert!(handle.shutdown().await.unwrap().unresolved_calls.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_reports_an_unconfirmed_write_after_its_job_is_stopped() {
        let (sender, mut writes) = mpsc::unbounded_channel();
        let (handle, mut requests) = session(
            KernelConfig::default(),
            vec![Arc::new(ControlledWrite(sender))],
        );
        start_write(&handle, &mut requests).await;
        let mut write = receive(&mut writes).await;
        let report = handle.shutdown().await.unwrap();
        assert_eq!(report.unresolved_calls.len(), 1);
        assert!(report.unresolved_calls[0].external_write);
        assert!(report.unresolved_calls[0].job.is_some());
        assert!(write.context.cancellation_requested());
        timeout(Duration::from_secs(2), write.reply.closed())
            .await
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn model_timeout_signals_cancellation_and_retains_the_call_outcome() {
        let (handle, mut requests) = session(
            KernelConfig {
                kernel_timeout: Duration::from_secs(3),
                ..KernelConfig::default()
            },
            vec![],
        );
        let mut observed = handle.observe().await.unwrap();
        handle
            .post(Input::new(InputId(1), "held routing"))
            .await
            .unwrap();
        let mut request = receive(&mut requests).await;
        tokio::time::advance(Duration::from_secs(3)).await;
        let finished = step(&mut observed, |event| {
            matches!(event, Event::CallFinished { .. })
        })
        .await;
        assert!(matches!(
            &finished.event,
            Event::CallFinished {
                outcome: CallOutcome {
                    result: Err(CallError {
                        kind: CallErrorKind::TimedOut,
                        ..
                    }),
                    external_effect: ExternalEffect::None,
                },
                ..
            }
        ));
        assert!(request.context.cancellation_requested());
        timeout(Duration::from_secs(2), request.reply.closed())
            .await
            .unwrap();
        assert!(handle.shutdown().await.unwrap().unresolved_calls.is_empty());
    }

    struct PanickingModel;

    impl ModelPort for PanickingModel {
        fn infer(&self, _: ModelInput, _: CallContext) -> BoxFuture<'static, CallOutcome> {
            panic!("failure while constructing the model future")
        }
    }

    struct DropPanickingModel;

    impl ModelPort for DropPanickingModel {
        fn infer(&self, _: ModelInput, _: CallContext) -> BoxFuture<'static, CallOutcome> {
            struct PanicOnDrop;
            impl Drop for PanicOnDrop {
                fn drop(&mut self) {
                    panic!("PRIVATE_JOIN_PANIC_SECRET");
                }
            }
            let guard = PanicOnDrop;
            Box::pin(async move {
                let _guard = guard;
                std::future::pending().await
            })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn join_error_from_dropping_a_future_does_not_expose_the_panic_payload() {
        let handle = Runtime::spawn(
            Arc::new(DropPanickingModel),
            vec![],
            KernelConfig {
                kernel_timeout: Duration::from_secs(1),
                ..KernelConfig::default()
            },
            RuntimeConfig::default(),
        )
        .unwrap();
        let mut observed = handle.observe().await.unwrap();
        handle
            .post(Input::new(InputId(1), "held routing"))
            .await
            .unwrap();
        // Timeout drops the pending port future outside catch_unwind's poll,
        // so the actor receives a JoinError rather than the normal completion.
        let finished = step(&mut observed, |event| {
            matches!(event, Event::CallFinished { .. })
        })
        .await;
        assert!(
            matches!(&finished.event, Event::CallFinished { outcome: CallOutcome {
            result: Err(CallError { kind: CallErrorKind::Panicked, message }),
            external_effect: ExternalEffect::None,
        }, .. } if message == PANIC_MESSAGE)
        );
        assert!(
            !serde_json::to_string(&*finished)
                .unwrap()
                .contains("PRIVATE_JOIN_PANIC_SECRET")
        );
        assert!(
            !serde_json::to_string(&handle.snapshot().await.unwrap())
                .unwrap()
                .contains("PRIVATE_JOIN_PANIC_SECRET")
        );
        assert!(handle.shutdown().await.unwrap().unresolved_calls.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn synchronous_model_panic_and_asynchronous_write_panic_remain_call_observations() {
        let handle = Runtime::spawn(
            Arc::new(PanickingModel),
            vec![],
            KernelConfig::default(),
            RuntimeConfig::default(),
        )
        .unwrap();
        let mut observed = handle.observe().await.unwrap();
        handle.post(Input::new(InputId(1), "panic")).await.unwrap();
        let finished = step(&mut observed, |event| {
            matches!(event, Event::CallFinished { .. })
        })
        .await;
        assert!(matches!(
            &finished.event,
            Event::CallFinished {
                outcome: CallOutcome {
                    result: Err(CallError {
                        kind: CallErrorKind::Panicked,
                        ..
                    }),
                    external_effect: ExternalEffect::None,
                },
                ..
            }
        ));
        assert!(handle.shutdown().await.unwrap().unresolved_calls.is_empty());

        let (sender, mut writes) = mpsc::unbounded_channel();
        let (handle, mut requests) = session(
            KernelConfig::default(),
            vec![Arc::new(ControlledWrite(sender))],
        );
        start_write(&handle, &mut requests).await;
        let write = receive(&mut writes).await;
        handle.stop().await.unwrap();
        let mut observed = handle.observe().await.unwrap();
        // ControlledWrite unwraps the channel result inside its returned future.
        drop(write.reply);
        let finished = step(&mut observed, |event| {
            matches!(event, Event::CallFinished { outcome, .. }
            if outcome.external_effect == ExternalEffect::Unknown)
        })
        .await;
        assert!(matches!(
            &finished.event,
            Event::CallFinished {
                outcome: CallOutcome {
                    result: Err(CallError {
                        kind: CallErrorKind::Panicked,
                        ..
                    }),
                    ..
                },
                ..
            }
        ));
        let report = handle.shutdown().await.unwrap();
        assert_eq!(report.unresolved_calls.len(), 1);
        assert!(report.unresolved_calls[0].external_write);
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_all_handles_cancels_reads_and_closes_observers() {
        let (handle, mut requests) = session(KernelConfig::default(), vec![]);
        let mut observed = handle.observe().await.unwrap();
        let mut notices = handle.subscribe();
        handle.post(Input::new(InputId(1), "held")).await.unwrap();
        let mut request = receive(&mut requests).await;
        drop(handle);
        timeout(Duration::from_secs(2), request.reply.closed())
            .await
            .unwrap();
        timeout(Duration::from_secs(2), async {
            while observed.events.recv().await.is_ok() {}
            while notices.recv().await.is_ok() {}
        })
        .await
        .unwrap();
        assert!(request.context.cancellation_requested());
    }
}
