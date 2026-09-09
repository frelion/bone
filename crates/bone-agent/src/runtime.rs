use std::{collections::BTreeMap, panic::AssertUnwindSafe, sync::Arc, time::Duration};

use futures_util::{FutureExt, future};
use tokio::{
    sync::{broadcast, mpsc, oneshot, watch},
    task::{Id, JoinError, JoinSet},
    time::{Instant, sleep_until},
};

use crate::{
    AdmissionError, AgentLimits, AgentView, BootstrapContext, Call, CallContext, CallError,
    CallErrorKind, CallId, ControlOutcome, Effect, Event, ExternalEffect, Input, InputId,
    InputReceipt, JobId, ModelPort, MonoTime, Record, Seq, ToolEffect, ToolOutcome, ToolPort,
    kernel::{Kernel, KernelControl},
};

const INPUT_QUEUE: usize = 64;
const CONTROL_QUEUE: usize = 16;
const PROGRESS_QUEUE: usize = 64;
const RECORD_QUEUE: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("an active Tokio runtime is required")]
    NoTokioRuntime,
    #[error("invalid agent configuration: {0}")]
    InvalidConfiguration(String),
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AgentError {
    #[error("the agent runtime has closed")]
    Closed,
    #[error("the agent runtime is shutting down")]
    ShuttingDown,
    #[error("invalid agent configuration: {0}")]
    InvalidConfiguration(String),
    #[error(transparent)]
    Admission(#[from] AdmissionError),
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UnresolvedWrite {
    pub call: CallId,
    pub job: JobId,
    pub tool: String,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ShutdownReport {
    pub unresolved_writes: Vec<UnresolvedWrite>,
    pub final_view: Arc<AgentView>,
}

pub struct Observation {
    pub baseline: Arc<AgentView>,
    pub after: Seq,
    pub records: broadcast::Receiver<Arc<Record>>,
}

#[derive(Clone)]
pub struct Agent {
    inputs: mpsc::Sender<InputCommand>,
    controls: mpsc::Sender<Control>,
    shutdown: watch::Receiver<Option<ShutdownReport>>,
}

impl Agent {
    pub async fn post(&self, input: Input) -> Result<InputReceipt, AgentError> {
        let (reply, result) = oneshot::channel();
        self.inputs
            .send(InputCommand { input, reply })
            .await
            .map_err(|_| AgentError::Closed)?;
        result.await.map_err(|_| AgentError::Closed)?
    }

    pub async fn retry(&self, input: InputId) -> Result<ControlOutcome, AgentError> {
        self.control(ControlKind::Retry(input)).await
    }

    pub async fn pause(&self, job: JobId) -> Result<ControlOutcome, AgentError> {
        self.control(ControlKind::Pause(job)).await
    }

    pub async fn resume(&self, job: JobId) -> Result<ControlOutcome, AgentError> {
        self.control(ControlKind::Resume(job)).await
    }

    pub async fn cancel(&self, job: JobId) -> Result<ControlOutcome, AgentError> {
        self.control(ControlKind::Cancel(job)).await
    }

    pub async fn stop(&self) -> Result<ControlOutcome, AgentError> {
        self.control(ControlKind::Stop).await
    }

    /// Pause scheduling without discarding the current job graph. Running
    /// model calls are revoked; running tools are allowed to finish. The state
    /// persists across reconfiguration until [`Agent::resume_scheduling`].
    pub async fn suspend(&self) -> Result<(), AgentError> {
        let (reply, result) = oneshot::channel();
        self.controls
            .send(Control::Suspend(reply))
            .await
            .map_err(|_| AgentError::Closed)?;
        result.await.map_err(|_| AgentError::Closed)?
    }

    /// Resume global scheduling after [`Agent::suspend`]. Configuration
    /// installed while suspended becomes active at this boundary.
    pub async fn resume_scheduling(&self) -> Result<(), AgentError> {
        let (reply, result) = oneshot::channel();
        self.controls
            .send(Control::ResumeScheduling(reply))
            .await
            .map_err(|_| AgentError::Closed)?;
        result.await.map_err(|_| AgentError::Closed)?
    }

    pub async fn resolve_write(
        &self,
        call: CallId,
        result: ToolOutcome,
    ) -> Result<ControlOutcome, AgentError> {
        self.control(ControlKind::ResolveWrite { call, result })
            .await
    }

    /// Atomically replace execution ports and limits without replacing the
    /// current job graph or changing whether scheduling is suspended. In an
    /// active Agent, model calls restart immediately. Running tools finish on
    /// the ports with which they started.
    pub async fn reconfigure(
        &self,
        model: Arc<dyn ModelPort>,
        tools: Vec<Arc<dyn ToolPort>>,
        limits: AgentLimits,
    ) -> Result<(), AgentError> {
        let mut ports = BTreeMap::new();
        let specifications = tools
            .into_iter()
            .map(|tool| {
                let specification = tool.specification();
                ports.insert(specification.name.clone(), (tool, specification.effect));
                specification
            })
            .collect();
        let (reply, result) = oneshot::channel();
        self.controls
            .send(Control::Reconfigure {
                model,
                tools: ports,
                specifications,
                limits,
                reply,
            })
            .await
            .map_err(|_| AgentError::Closed)?;
        result.await.map_err(|_| AgentError::Closed)?
    }

    pub async fn observe(&self) -> Result<Observation, AgentError> {
        let (reply, result) = oneshot::channel();
        self.controls
            .send(Control::Observe(reply))
            .await
            .map_err(|_| AgentError::Closed)?;
        result.await.map_err(|_| AgentError::Closed)
    }

    pub async fn shutdown(&self) -> Result<ShutdownReport, AgentError> {
        let mut report = self.shutdown.clone();
        if report.borrow().is_none() {
            let _ = self.controls.send(Control::Shutdown).await;
        }
        loop {
            if let Some(report) = report.borrow_and_update().clone() {
                return Ok(report);
            }
            report.changed().await.map_err(|_| AgentError::Closed)?;
        }
    }

    async fn control(&self, kind: ControlKind) -> Result<ControlOutcome, AgentError> {
        let (reply, result) = oneshot::channel();
        self.controls
            .send(Control::Command { kind, reply })
            .await
            .map_err(|_| AgentError::Closed)?;
        result.await.map_err(|_| AgentError::Closed)?
    }
}

impl Agent {
    pub fn with_ports(
        model: Arc<dyn ModelPort>,
        tools: Vec<Arc<dyn ToolPort>>,
        limits: AgentLimits,
    ) -> Result<Self, RuntimeError> {
        Self::with_ports_and_background(model, tools, limits, BootstrapContext::default())
    }

    pub fn with_ports_and_background(
        model: Arc<dyn ModelPort>,
        tools: Vec<Arc<dyn ToolPort>>,
        limits: AgentLimits,
        background: BootstrapContext,
    ) -> Result<Self, RuntimeError> {
        let executor =
            tokio::runtime::Handle::try_current().map_err(|_| RuntimeError::NoTokioRuntime)?;
        let mut ports = BTreeMap::new();
        let mut specifications = Vec::new();
        for tool in tools {
            let spec = tool.specification();
            specifications.push(spec.clone());
            ports.insert(spec.name, (tool, spec.effect));
        }
        let kernel = Kernel::with_background(limits, specifications, background)
            .map_err(|error| RuntimeError::InvalidConfiguration(error.to_string()))?;
        let (inputs, input_rx) = mpsc::channel(INPUT_QUEUE);
        let (controls, control_rx) = mpsc::channel(CONTROL_QUEUE);
        let (progress, progress_rx) = mpsc::channel(PROGRESS_QUEUE);
        let (records, _) = broadcast::channel(RECORD_QUEUE);
        let (shutdown_tx, shutdown) = watch::channel(None);
        let handle = Self {
            inputs,
            controls,
            shutdown,
        };
        executor.spawn(
            Actor {
                kernel,
                model,
                tools: ports,
                inputs: input_rx,
                controls: control_rx,
                progress_tx: progress,
                progress_rx,
                records,
                shutdown: shutdown_tx,
                tasks: JoinSet::new(),
                running: BTreeMap::new(),
                started: Instant::now(),
                shutting_down: false,
                shutdown_at: None,
            }
            .run(),
        );
        Ok(handle)
    }
}

struct InputCommand {
    input: Input,
    reply: oneshot::Sender<Result<InputReceipt, AgentError>>,
}

enum Control {
    Command {
        kind: ControlKind,
        reply: oneshot::Sender<Result<ControlOutcome, AgentError>>,
    },
    Reconfigure {
        model: Arc<dyn ModelPort>,
        tools: BTreeMap<String, (Arc<dyn ToolPort>, ToolEffect)>,
        specifications: Vec<crate::ToolSpec>,
        limits: AgentLimits,
        reply: oneshot::Sender<Result<(), AgentError>>,
    },
    Suspend(oneshot::Sender<Result<(), AgentError>>),
    ResumeScheduling(oneshot::Sender<Result<(), AgentError>>),
    Observe(oneshot::Sender<Observation>),
    Shutdown,
}

enum ControlKind {
    Retry(InputId),
    Pause(JobId),
    Resume(JobId),
    Cancel(JobId),
    Stop,
    ResolveWrite { call: CallId, result: ToolOutcome },
}

struct RunningCall {
    cancel: watch::Sender<bool>,
    task: Id,
    failure: CallFailure,
}

struct Actor {
    kernel: Kernel,
    model: Arc<dyn ModelPort>,
    tools: BTreeMap<String, (Arc<dyn ToolPort>, ToolEffect)>,
    inputs: mpsc::Receiver<InputCommand>,
    controls: mpsc::Receiver<Control>,
    progress_tx: mpsc::Sender<(CallId, crate::CallProgress)>,
    progress_rx: mpsc::Receiver<(CallId, crate::CallProgress)>,
    records: broadcast::Sender<Arc<Record>>,
    shutdown: watch::Sender<Option<ShutdownReport>>,
    tasks: JoinSet<(CallId, Event)>,
    running: BTreeMap<CallId, RunningCall>,
    started: Instant,
    shutting_down: bool,
    shutdown_at: Option<Instant>,
}

impl Actor {
    async fn run(mut self) {
        let mut controls_open = true;
        loop {
            if self.shutdown_complete() {
                let final_view = Arc::new(self.kernel.view());
                self.shutdown.send_replace(Some(ShutdownReport {
                    unresolved_writes: self.kernel.unresolved_writes(),
                    final_view,
                }));
                return;
            }
            let deadline = earliest(
                self.kernel
                    .next_deadline()
                    .and_then(|time| self.started.checked_add(time.0)),
                self.shutdown_at,
            );
            tokio::select! {
                control = self.controls.recv(), if controls_open => {
                    match control {
                        Some(control) => self.handle_control(control),
                        None => {
                            // The last Agent handle has released both host channels.
                            controls_open = false;
                            self.begin_shutdown();
                        }
                    }
                }
                Some(command) = self.inputs.recv(), if !self.shutting_down => {
                    self.handle_input(command);
                }
                Some((call, progress)) = self.progress_rx.recv() => {
                    self.apply(Event::Progress { call, progress });
                }
                Some(result) = self.tasks.join_next(), if !self.tasks.is_empty() => {
                    match result {
                        Ok((call, event)) => {
                            self.running.remove(&call);
                            self.apply(event);
                        }
                        Err(error) => self.handle_join_error(error),
                    }
                }
                _ = wait_until(deadline) => self.apply(Event::Tick),
            }
        }
    }

    fn handle_control(&mut self, control: Control) {
        match control {
            Control::Observe(reply) => {
                let records = self.records.subscribe();
                let baseline = Arc::new(self.kernel.view());
                let _ = reply.send(Observation {
                    after: baseline.sequence,
                    baseline,
                    records,
                });
            }
            Control::Command { reply, .. } if self.shutting_down => {
                let _ = reply.send(Err(AgentError::ShuttingDown));
            }
            Control::Reconfigure { reply, .. } if self.shutting_down => {
                let _ = reply.send(Err(AgentError::ShuttingDown));
            }
            Control::Suspend(reply) if self.shutting_down => {
                let _ = reply.send(Err(AgentError::ShuttingDown));
            }
            Control::ResumeScheduling(reply) if self.shutting_down => {
                let _ = reply.send(Err(AgentError::ShuttingDown));
            }
            Control::Suspend(reply) => {
                let effects = self.kernel.suspend(self.now());
                self.dispatch(effects);
                let _ = reply.send(Ok(()));
            }
            Control::ResumeScheduling(reply) => {
                let effects = self.kernel.resume_scheduling(self.now());
                self.dispatch(effects);
                let _ = reply.send(Ok(()));
            }
            Control::Reconfigure {
                model,
                tools,
                specifications,
                limits,
                reply,
            } => match self.kernel.reconfigure(self.now(), limits, specifications) {
                Ok(effects) => {
                    self.model = model;
                    self.tools = tools;
                    self.dispatch(effects);
                    let _ = reply.send(Ok(()));
                }
                Err(error) => {
                    let _ = reply.send(Err(AgentError::InvalidConfiguration(error.to_string())));
                }
            },
            Control::Command { kind, reply } => {
                let command = match kind {
                    ControlKind::Retry(input) => KernelControl::Retry(input),
                    ControlKind::Pause(job) => KernelControl::Pause(job),
                    ControlKind::Resume(job) => KernelControl::Resume(job),
                    ControlKind::Cancel(job) => KernelControl::Cancel(job),
                    ControlKind::Stop => KernelControl::Stop,
                    ControlKind::ResolveWrite { call, result } => {
                        KernelControl::ResolveWrite { call, result }
                    }
                };
                let (outcome, effects) = self.kernel.control(self.now(), command);
                self.dispatch(effects);
                let _ = reply.send(Ok(outcome));
            }
            Control::Shutdown => self.begin_shutdown(),
        }
    }

    fn handle_input(&mut self, command: InputCommand) {
        match self.kernel.accept(self.now(), command.input) {
            Ok((receipt, effects)) => {
                self.dispatch(effects);
                let _ = command.reply.send(Ok(receipt));
            }
            Err(error) => {
                let _ = command.reply.send(Err(error.into()));
            }
        }
    }

    fn apply(&mut self, event: Event) {
        let effects = self.kernel.step(self.now(), event);
        self.dispatch(effects);
    }

    fn dispatch(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Notify(record) => {
                    let _ = self.records.send(record);
                }
                Effect::Cancel(call) => {
                    if let Some(task) = self.running.get(&call) {
                        task.cancel.send_replace(true);
                    }
                }
                Effect::Start { id, call, timeout } => {
                    let (cancel, cancellation) = watch::channel(false);
                    let context =
                        CallContext::new(id, self.progress_tx.clone(), cancellation.clone());
                    let model = self.model.clone();
                    let tool = match call.as_ref() {
                        Call::Tool(request) => Some(self.tools[&request.name].clone()),
                        _ => None,
                    };
                    let failure = match call.as_ref() {
                        Call::Coordinate(_) => CallFailure::Coordinate,
                        Call::Work(_) => CallFailure::Work,
                        Call::Compact(_) => CallFailure::Compact,
                        Call::Tool(_) => CallFailure::Tool(
                            tool.as_ref()
                                .expect("kernel only starts registered tools")
                                .1,
                        ),
                    };
                    let tool = tool.map(|(port, _)| port);
                    let task = self.tasks.spawn(async move {
                        let event =
                            execute(*call, model, tool, context, cancellation, timeout, failure)
                                .await;
                        (id, event)
                    });
                    self.running.insert(
                        id,
                        RunningCall {
                            cancel,
                            task: task.id(),
                            failure,
                        },
                    );
                }
            }
        }
    }

    fn handle_join_error(&mut self, error: JoinError) {
        let call = self
            .running
            .iter()
            .find_map(|(call, running)| (running.task == error.id()).then_some(*call))
            .expect("joined task is registered");
        let running = self.running.remove(&call).expect("running call exists");
        let (kind, message) = if error.is_cancelled() {
            (CallErrorKind::Cancelled, "call task was cancelled")
        } else {
            (CallErrorKind::Panicked, "call panicked")
        };
        self.apply(failed_event(call, running.failure, kind, message));
    }

    fn begin_shutdown(&mut self) {
        if self.shutting_down {
            return;
        }
        self.shutting_down = true;
        self.inputs.close();
        self.shutdown_at = Instant::now().checked_add(self.kernel.limits.shutdown_grace);
        let (_, effects) = self.kernel.control(self.now(), KernelControl::Stop);
        self.dispatch(effects);
    }

    fn shutdown_complete(&self) -> bool {
        self.shutting_down
            && (self.tasks.is_empty()
                || self
                    .shutdown_at
                    .is_some_and(|deadline| Instant::now() >= deadline))
    }

    fn now(&self) -> MonoTime {
        elapsed(self.started)
    }
}

async fn execute(
    call: Call,
    model: Arc<dyn ModelPort>,
    tool: Option<Arc<dyn ToolPort>>,
    context: CallContext,
    mut cancellation: watch::Receiver<bool>,
    timeout: Duration,
    failure: CallFailure,
) -> Event {
    let id = context.id();
    let operation = AssertUnwindSafe(run_call(id, call, model, tool, context)).catch_unwind();
    let completion = async {
        match tokio::time::timeout(timeout, operation).await {
            Ok(Ok(event)) => event,
            Ok(Err(_)) => failed_event(id, failure, CallErrorKind::Panicked, "call panicked"),
            Err(_) => failed_event(id, failure, CallErrorKind::TimedOut, "call timed out"),
        }
    };
    if matches!(failure, CallFailure::Tool(ToolEffect::ExternalWrite)) {
        return completion.await;
    }
    tokio::select! {
        biased;
        _ = cancellation.changed() => failed_event(id, failure, CallErrorKind::Cancelled, "call cancelled"),
        event = completion => event,
    }
}

async fn run_call(
    id: CallId,
    call: Call,
    model: Arc<dyn ModelPort>,
    tool: Option<Arc<dyn ToolPort>>,
    context: CallContext,
) -> Event {
    match call {
        Call::Coordinate(input) => {
            let result = model.coordinate(input, context).await;
            Event::CoordinateFinished { call: id, result }
        }
        Call::Work(input) => {
            let result = model.work(input, context).await;
            Event::WorkFinished { call: id, result }
        }
        Call::Compact(input) => {
            let result = model.compact(input, context).await;
            Event::CompactFinished { call: id, result }
        }
        Call::Tool(request) => {
            let tool = tool.expect("kernel only starts registered tools");
            let result = tool.run(request.arguments.clone(), context).await;
            Event::ToolFinished { call: id, result }
        }
    }
}

#[derive(Clone, Copy)]
enum CallFailure {
    Coordinate,
    Work,
    Compact,
    Tool(ToolEffect),
}

fn failed_event(id: CallId, call: CallFailure, kind: CallErrorKind, message: &str) -> Event {
    let error = CallError {
        kind,
        message: message.into(),
    };
    match call {
        CallFailure::Coordinate => Event::CoordinateFinished {
            call: id,
            result: Err(error),
        },
        CallFailure::Work => Event::WorkFinished {
            call: id,
            result: Err(error),
        },
        CallFailure::Compact => Event::CompactFinished {
            call: id,
            result: Err(error),
        },
        CallFailure::Tool(effect) => Event::ToolFinished {
            call: id,
            result: ToolOutcome {
                result: Err(error),
                external_effect: if effect == ToolEffect::ExternalWrite {
                    ExternalEffect::Unknown
                } else {
                    ExternalEffect::None
                },
            },
        },
    }
}

fn elapsed(started: Instant) -> MonoTime {
    MonoTime(started.elapsed())
}

fn earliest(left: Option<Instant>, right: Option<Instant>) -> Option<Instant> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left, right) => left.or(right),
    }
}

async fn wait_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => sleep_until(deadline).await,
        None => future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::{Value, json};

    use super::*;
    use crate::{
        Assignment, Await, CheckpointDraft, CompactInput, CoordinateInput, JobChange, JobSpec,
        PortFuture, ToolSpec, WorkInput, WorkProposal, WorkStep,
    };

    struct PanicOnceModel {
        attempts: AtomicUsize,
        started: mpsc::UnboundedSender<usize>,
    }

    impl ModelPort for PanicOnceModel {
        fn coordinate(
            &self,
            _: CoordinateInput,
            _: CallContext,
        ) -> PortFuture<Result<crate::KernelDecision, CallError>> {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
            let _ = self.started.send(attempt);
            if attempt == 0 {
                panic!("synchronous coordinate panic");
            }
            Box::pin(async {
                Ok(crate::KernelDecision::Apply {
                    changes: Vec::new(),
                    constraints: None,
                })
            })
        }

        fn work(
            &self,
            _: WorkInput,
            _: CallContext,
        ) -> PortFuture<Result<WorkProposal, CallError>> {
            panic!("unexpected work call")
        }

        fn compact(
            &self,
            _: CompactInput,
            _: CallContext,
        ) -> PortFuture<Result<CheckpointDraft, CallError>> {
            panic!("unexpected compact call")
        }
    }

    #[tokio::test]
    async fn synchronous_port_panic_releases_the_coordination_slot() {
        let (started, mut starts) = mpsc::unbounded_channel();
        let agent = Agent::with_ports(
            Arc::new(PanicOnceModel {
                attempts: AtomicUsize::new(0),
                started,
            }),
            Vec::new(),
            AgentLimits::default(),
        )
        .unwrap();

        agent.post(Input::new(InputId(1), "first")).await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), starts.recv())
                .await
                .unwrap(),
            Some(0)
        );
        agent.post(Input::new(InputId(2), "second")).await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), starts.recv())
                .await
                .unwrap(),
            Some(1)
        );

        let observation = agent.observe().await.unwrap();
        assert!(observation.baseline.records.iter().any(|record| matches!(
            &record.body,
            crate::RecordBody::CallFinished {
                error: Some(CallError {
                    kind: CallErrorKind::Panicked,
                    ..
                }),
                ..
            }
        )));
        agent.shutdown().await.unwrap();
    }

    struct AppliedWrite;

    impl ToolPort for AppliedWrite {
        fn specification(&self) -> ToolSpec {
            ToolSpec {
                name: "write".into(),
                description: "write externally".into(),
                parameters: json!({"type": "object"}),
                effect: ToolEffect::ExternalWrite,
            }
        }

        fn run(&self, _: Value, mut context: CallContext) -> PortFuture<ToolOutcome> {
            Box::pin(async move {
                context.wait_for_cancellation().await;
                ToolOutcome {
                    result: Ok(json!("written")),
                    external_effect: ExternalEffect::Applied,
                }
            })
        }
    }

    struct PendingModel {
        started: mpsc::UnboundedSender<()>,
    }

    impl ModelPort for PendingModel {
        fn coordinate(
            &self,
            _: CoordinateInput,
            _: CallContext,
        ) -> PortFuture<Result<crate::KernelDecision, CallError>> {
            let _ = self.started.send(());
            Box::pin(future::pending())
        }

        fn work(
            &self,
            _: WorkInput,
            _: CallContext,
        ) -> PortFuture<Result<WorkProposal, CallError>> {
            panic!("unexpected work call")
        }

        fn compact(
            &self,
            _: CompactInput,
            _: CallContext,
        ) -> PortFuture<Result<CheckpointDraft, CallError>> {
            panic!("unexpected compact call")
        }
    }

    #[tokio::test]
    async fn dropping_the_last_handle_releases_idle_model_and_tool_ports() {
        let model = Arc::new(PendingModel {
            started: mpsc::unbounded_channel().0,
        });
        let tool = Arc::new(AppliedWrite);
        let retained_model = Arc::downgrade(&model);
        let retained_tool = Arc::downgrade(&tool);
        let agent = Agent::with_ports(model, vec![tool], AgentLimits::default()).unwrap();
        let remaining = agent.clone();
        let mut shutdown = agent.shutdown.clone();

        drop(agent);
        remaining.observe().await.unwrap();
        assert!(shutdown.borrow().is_none());
        assert!(retained_model.upgrade().is_some());
        assert!(retained_tool.upgrade().is_some());

        drop(remaining);
        tokio::time::timeout(Duration::from_secs(1), shutdown.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(*shutdown.borrow(), Some(ShutdownReport::default()));
        assert!(retained_model.upgrade().is_none());
        assert!(retained_tool.upgrade().is_none());
    }

    #[tokio::test]
    async fn dropping_the_last_handle_cancels_a_running_model_call() {
        let (started, mut starts) = mpsc::unbounded_channel();
        let model = Arc::new(PendingModel { started });
        let retained_model = Arc::downgrade(&model);
        let agent = Agent::with_ports(model, vec![], AgentLimits::default()).unwrap();
        let mut shutdown = agent.shutdown.clone();
        agent.post(Input::new(InputId(1), "wait")).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), starts.recv())
            .await
            .unwrap()
            .unwrap();

        drop(agent);
        tokio::time::timeout(Duration::from_secs(1), shutdown.changed())
            .await
            .unwrap()
            .unwrap();
        let report = shutdown.borrow().clone().unwrap();
        assert!(report.unresolved_writes.is_empty());
        assert!(matches!(
            report.final_view.inputs[0].status,
            crate::InputStatus::Finished(crate::InputOutcome::Cancelled)
        ));
        assert!(retained_model.upgrade().is_none());
    }

    #[tokio::test]
    async fn reconfigure_moves_pending_coordination_to_the_new_model() {
        let (old_started, mut old_starts) = mpsc::unbounded_channel();
        let agent = Agent::with_ports(
            Arc::new(PendingModel {
                started: old_started,
            }),
            Vec::new(),
            AgentLimits::default(),
        )
        .unwrap();
        agent
            .post(Input::new(InputId(1), "switch models"))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), old_starts.recv())
            .await
            .unwrap()
            .unwrap();

        let (new_started, mut new_starts) = mpsc::unbounded_channel();
        agent
            .reconfigure(
                Arc::new(PanicOnceModel {
                    attempts: AtomicUsize::new(1),
                    started: new_started,
                }),
                Vec::new(),
                AgentLimits::default(),
            )
            .await
            .unwrap();

        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), new_starts.recv())
                .await
                .unwrap(),
            Some(1)
        );
        let view = agent.observe().await.unwrap().baseline;
        assert_eq!(
            view.records
                .iter()
                .filter(|record| matches!(record.body, crate::RecordBody::CallStarted { .. }))
                .count(),
            2
        );
        assert!(view.records.iter().any(|record| matches!(
            &record.body,
            crate::RecordBody::CallFinished {
                error: Some(CallError {
                    kind: CallErrorKind::Cancelled,
                    ..
                }),
                ..
            }
        )));
        agent.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn suspended_reconfigure_needs_an_explicit_scheduling_resume() {
        let (old_started, mut old_starts) = mpsc::unbounded_channel();
        let agent = Agent::with_ports(
            Arc::new(PendingModel {
                started: old_started,
            }),
            Vec::new(),
            AgentLimits::default(),
        )
        .unwrap();
        agent
            .post(Input::new(InputId(1), "switch while suspended"))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), old_starts.recv())
            .await
            .unwrap()
            .unwrap();

        agent.suspend().await.unwrap();
        let (new_started, mut new_starts) = mpsc::unbounded_channel();
        agent
            .reconfigure(
                Arc::new(PendingModel {
                    started: new_started,
                }),
                Vec::new(),
                AgentLimits::default(),
            )
            .await
            .unwrap();

        let view = agent.observe().await.unwrap().baseline;
        assert_eq!(
            view.records
                .iter()
                .filter(|record| matches!(record.body, crate::RecordBody::CallStarted { .. }))
                .count(),
            1
        );

        agent.resume_scheduling().await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), new_starts.recv())
            .await
            .unwrap()
            .unwrap();
        agent.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn controls_report_state_changes_and_shutdown_returns_the_final_view() {
        let (started, mut starts) = mpsc::unbounded_channel();
        let agent = Agent::with_ports(
            Arc::new(PendingModel { started }),
            Vec::new(),
            AgentLimits::default(),
        )
        .unwrap();
        agent.post(Input::new(InputId(1), "wait")).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), starts.recv())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(agent.stop().await.unwrap(), ControlOutcome::Applied);
        assert_eq!(agent.stop().await.unwrap(), ControlOutcome::Unchanged);
        assert_eq!(
            agent.pause(JobId(999)).await.unwrap(),
            ControlOutcome::Unchanged
        );

        let report = agent.shutdown().await.unwrap();
        assert!(matches!(
            report.final_view.inputs[0].status,
            crate::InputStatus::Finished(crate::InputOutcome::Cancelled)
        ));
        assert!(report.final_view.records.iter().any(|record| {
            matches!(
                record.body,
                crate::RecordBody::InputFinished {
                    input: InputId(1),
                    outcome: crate::InputOutcome::Cancelled,
                }
            )
        }));
    }

    struct MaxWaitModel;

    impl ModelPort for MaxWaitModel {
        fn coordinate(
            &self,
            input: CoordinateInput,
            _: CallContext,
        ) -> PortFuture<Result<crate::KernelDecision, CallError>> {
            let mut assignment = Assignment::new(JobSpec::new("wait", "wait", "wait"));
            assignment.inputs = input.inputs.into_iter().map(|input| input.id).collect();
            Box::pin(async move {
                Ok(crate::KernelDecision::Apply {
                    changes: vec![JobChange::Create(assignment)],
                    constraints: None,
                })
            })
        }

        fn work(
            &self,
            _: WorkInput,
            _: CallContext,
        ) -> PortFuture<Result<WorkProposal, CallError>> {
            Box::pin(async {
                Ok(WorkProposal::new(WorkStep::Wait(Await::After(
                    Duration::MAX,
                ))))
            })
        }

        fn compact(
            &self,
            _: CompactInput,
            _: CallContext,
        ) -> PortFuture<Result<CheckpointDraft, CallError>> {
            panic!("unexpected compact call")
        }
    }

    #[tokio::test]
    async fn maximum_wait_duration_does_not_kill_the_runtime() {
        let agent =
            Agent::with_ports(Arc::new(MaxWaitModel), Vec::new(), AgentLimits::default()).unwrap();
        agent.post(Input::new(InputId(1), "wait")).await.unwrap();

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let observation = agent.observe().await.unwrap();
                if observation.baseline.jobs.iter().any(|job| {
                    matches!(
                        job.status,
                        crate::JobStatus::Waiting(crate::WaitView::Until(_))
                    )
                }) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        agent.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn maximum_shutdown_grace_does_not_kill_the_runtime() {
        let (started, mut starts) = mpsc::unbounded_channel();
        let limits = AgentLimits {
            shutdown_grace: Duration::MAX,
            ..AgentLimits::default()
        };
        let agent =
            Agent::with_ports(Arc::new(PendingModel { started }), Vec::new(), limits).unwrap();
        agent.post(Input::new(InputId(1), "wait")).await.unwrap();
        starts.recv().await.unwrap();

        tokio::time::timeout(Duration::from_secs(1), agent.shutdown())
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn cancelling_an_external_write_preserves_its_applied_result() {
        let (progress, _progress_rx) = mpsc::channel(1);
        let (cancel, cancellation) = watch::channel(false);
        let call = CallId(7);
        let task = tokio::spawn(execute(
            Call::Tool(Arc::new(crate::ToolCall::new("write", json!({})))),
            Arc::new(PanicOnceModel {
                attempts: AtomicUsize::new(1),
                started: mpsc::unbounded_channel().0,
            }),
            Some(Arc::new(AppliedWrite)),
            CallContext::new(call, progress, cancellation.clone()),
            cancellation,
            Duration::from_secs(1),
            CallFailure::Tool(ToolEffect::ExternalWrite),
        ));

        cancel.send_replace(true);
        let event = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            event,
            Event::ToolFinished {
                call: CallId(7),
                result: ToolOutcome {
                    result: Ok(value),
                    external_effect: ExternalEffect::Applied,
                },
            } if value == json!("written")
        ));
    }
}
