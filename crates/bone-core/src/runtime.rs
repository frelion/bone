use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    panic::AssertUnwindSafe,
    sync::Arc,
    time::Duration,
};

use futures_util::{FutureExt, future};
use tokio::{
    sync::{broadcast, mpsc, oneshot, watch},
    task::{Id, JoinError, JoinSet},
    time::{Instant, sleep_until},
};

use crate::{
    AdmissionError, AgentLimits, AgentView, Call, CallContext, CallError, CallErrorKind, CallId,
    ControlOutcome, DurableCommit, DurableError, DurablePort, DurableRestore, Effect, Event,
    ExternalEffect, Input, InputId, InputReceipt, JobId, ModelPort, MonoTime, Record, Seq,
    ToolEffect, ToolOutcome, ToolPort,
    kernel::{Kernel, KernelControl},
};

const INPUT_QUEUE: usize = 64;
const CONTROL_QUEUE: usize = 16;
const PROGRESS_QUEUE: usize = 64;
const RECORD_QUEUE: usize = 256;

type RuntimeTools = BTreeMap<String, (Arc<dyn ToolPort>, ToolEffect)>;

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("an active Tokio runtime is required")]
    NoTokioRuntime,
    #[error("invalid agent configuration: {0}")]
    InvalidConfiguration(String),
    #[error(transparent)]
    Durable(#[from] DurableError),
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AgentError {
    #[error("the agent runtime has closed")]
    Closed,
    #[error("the agent runtime is shutting down")]
    ShuttingDown,
    #[error("shutdown interrupted durable commit {commit_id}; its outcome is unknown")]
    CommitInterrupted { commit_id: String },
    #[error("invalid agent configuration: {0}")]
    InvalidConfiguration(String),
    #[error(transparent)]
    Durable(#[from] DurableError),
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
    /// A commit whose acknowledgement was not received before shutdown. Reload
    /// durable state before continuing; `final_view` only contains acknowledged state.
    /// The field must be present in serialized reports, using null when absent.
    #[serde(deserialize_with = "serde::Deserialize::deserialize")]
    pub pending_commit: Option<String>,
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
        self.control(KernelControl::Retry(input)).await
    }

    pub async fn pause(&self, job: JobId) -> Result<ControlOutcome, AgentError> {
        self.control(KernelControl::Pause(job)).await
    }

    pub async fn resume(&self, job: JobId) -> Result<ControlOutcome, AgentError> {
        self.control(KernelControl::Resume(job)).await
    }

    pub async fn cancel(&self, job: JobId) -> Result<ControlOutcome, AgentError> {
        self.control(KernelControl::Cancel(job)).await
    }

    pub async fn stop(&self) -> Result<ControlOutcome, AgentError> {
        self.control(KernelControl::Stop).await
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
        self.control(KernelControl::ResolveWrite { call, result })
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

    async fn control(&self, kind: KernelControl) -> Result<ControlOutcome, AgentError> {
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
        let (ports, specifications) = assemble_tools(tools);
        let kernel = Kernel::new(limits, specifications)
            .map_err(|error| RuntimeError::InvalidConfiguration(error.to_string()))?;
        Self::spawn_actor(kernel, model, ports, None, 0)
    }

    /// Construct an Agent whose state transitions are committed before their
    /// records are observed or their calls are started.
    pub async fn with_durable_ports(
        model: Arc<dyn ModelPort>,
        tools: Vec<Arc<dyn ToolPort>>,
        limits: AgentLimits,
        durable: Arc<dyn DurablePort>,
        restore: Option<DurableRestore>,
    ) -> Result<Self, RuntimeError> {
        let (ports, specifications) = assemble_tools(tools);
        let (kernel, expected_revision, records) = match restore {
            Some(restore) => {
                let (kernel, effects) =
                    Kernel::restore(restore.snapshot, restore.records, limits, specifications)?;
                let records = notified_records(&effects);
                (kernel, restore.revision, records)
            }
            None => {
                let kernel = Kernel::new(limits, specifications)
                    .map_err(|error| RuntimeError::InvalidConfiguration(error.to_string()))?;
                (kernel, 0, Vec::new())
            }
        };
        let snapshot = kernel.durable_snapshot()?;
        let commit_id = format!("{}:{}", snapshot.epoch(), expected_revision + 1);
        let receipt = durable
            .commit(DurableCommit {
                commit_id: commit_id.clone(),
                expected_revision,
                snapshot,
                records,
            })
            .await?;
        validate_receipt(
            &receipt,
            &commit_id,
            expected_revision + 1,
            kernel.view().sequence,
        )?;
        Self::spawn_actor(kernel, model, ports, Some(durable), receipt.revision)
    }

    fn spawn_actor(
        kernel: Kernel,
        model: Arc<dyn ModelPort>,
        ports: BTreeMap<String, (Arc<dyn ToolPort>, ToolEffect)>,
        durable: Option<Arc<dyn DurablePort>>,
        durable_revision: u64,
    ) -> Result<Self, RuntimeError> {
        let executor =
            tokio::runtime::Handle::try_current().map_err(|_| RuntimeError::NoTokioRuntime)?;
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
                durable,
                durable_revision,
                inputs: input_rx,
                controls: control_rx,
                progress_tx: progress,
                progress_rx,
                records,
                shutdown: shutdown_tx,
                tasks: JoinSet::new(),
                running: BTreeMap::new(),
                deferred: VecDeque::new(),
                cancelled_before_start: BTreeSet::new(),
                started: Instant::now(),
                shutting_down: false,
                shutdown_at: None,
                interrupted_commit: None,
            }
            .run(),
        );
        Ok(handle)
    }
}

fn assemble_tools(tools: Vec<Arc<dyn ToolPort>>) -> (RuntimeTools, Vec<crate::ToolSpec>) {
    let mut ports = BTreeMap::new();
    let mut specifications = Vec::new();
    for tool in tools {
        let spec = tool.specification();
        specifications.push(spec.clone());
        ports.insert(spec.name, (tool, spec.effect));
    }
    (ports, specifications)
}

fn notified_records(effects: &[Effect]) -> Vec<Arc<Record>> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Notify(record) => Some(Arc::clone(record)),
            Effect::Start { .. } | Effect::Cancel(_) => None,
        })
        .collect()
}

fn validate_receipt(
    receipt: &crate::DurableReceipt,
    commit_id: &str,
    revision: u64,
    through: Seq,
) -> Result<(), DurableError> {
    if receipt.commit_id != commit_id || receipt.revision != revision || receipt.through != through
    {
        return Err(DurableError::Invalid(
            "durable receipt does not match the committed transition".into(),
        ));
    }
    Ok(())
}

struct InputCommand {
    input: Input,
    reply: oneshot::Sender<Result<InputReceipt, AgentError>>,
}

enum Control {
    Command {
        kind: KernelControl,
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

struct RunningCall {
    cancel: watch::Sender<bool>,
    task: Id,
    failure: CallFailure,
}

enum Deferred {
    Control(Control),
    Event(Event),
}

struct Actor {
    kernel: Kernel,
    model: Arc<dyn ModelPort>,
    tools: BTreeMap<String, (Arc<dyn ToolPort>, ToolEffect)>,
    durable: Option<Arc<dyn DurablePort>>,
    durable_revision: u64,
    inputs: mpsc::Receiver<InputCommand>,
    controls: mpsc::Receiver<Control>,
    progress_tx: mpsc::Sender<(CallId, crate::CallProgress)>,
    progress_rx: mpsc::Receiver<(CallId, crate::CallProgress)>,
    records: broadcast::Sender<Arc<Record>>,
    shutdown: watch::Sender<Option<ShutdownReport>>,
    tasks: JoinSet<(CallId, Event)>,
    running: BTreeMap<CallId, RunningCall>,
    deferred: VecDeque<Deferred>,
    cancelled_before_start: BTreeSet<CallId>,
    started: Instant,
    shutting_down: bool,
    shutdown_at: Option<Instant>,
    interrupted_commit: Option<String>,
}

impl Actor {
    async fn run(mut self) {
        let mut controls_open = true;
        loop {
            if self.interrupted_commit.is_some() || self.shutdown_expired() {
                self.publish_shutdown();
                return;
            }
            if let Some(deferred) = self.deferred.pop_front() {
                match deferred {
                    Deferred::Control(control) => self.handle_control(control).await,
                    Deferred::Event(event) => self.apply(event).await,
                }
                continue;
            }
            if self.shutdown_complete() {
                self.publish_shutdown();
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
                        Some(control) => self.handle_control(control).await,
                        None => {
                            // The last Agent handle has released both host channels.
                            controls_open = false;
                            self.begin_shutdown().await;
                        }
                    }
                }
                Some(command) = self.inputs.recv(), if !self.shutting_down => {
                    self.handle_input(command).await;
                }
                Some((call, progress)) = self.progress_rx.recv() => {
                    self.apply(Event::Progress { call, progress }).await;
                }
                Some(result) = self.tasks.join_next(), if !self.tasks.is_empty() => {
                    match result {
                        Ok((call, event)) => {
                            self.running.remove(&call);
                            self.apply(event).await;
                        }
                        Err(error) => self.handle_join_error(error).await,
                    }
                }
                _ = wait_until(deadline) => self.apply(Event::Tick).await,
            }
        }
    }

    async fn handle_control(&mut self, control: Control) {
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
                let mut candidate = self.kernel.clone();
                let effects = candidate.suspend(self.now());
                self.cancel_execution(&effects);
                let result = self.commit_candidate(candidate, effects).await;
                let _ = reply.send(result);
            }
            Control::ResumeScheduling(reply) => {
                let mut candidate = self.kernel.clone();
                let effects = candidate.resume_scheduling(self.now());
                let result = self.commit_candidate(candidate, effects).await;
                let _ = reply.send(result);
            }
            Control::Reconfigure {
                model,
                tools,
                specifications,
                limits,
                reply,
            } => {
                let mut candidate = self.kernel.clone();
                match candidate.reconfigure(self.now(), limits, specifications) {
                    Ok(effects) => {
                        self.cancel_execution(&effects);
                        match self.commit_state(candidate, &effects).await {
                            Ok(()) => {
                                self.model = model;
                                self.tools = tools;
                                self.dispatch(effects);
                                let _ = reply.send(Ok(()));
                            }
                            Err(error) => {
                                let _ = reply.send(Err(error));
                            }
                        }
                    }
                    Err(error) => {
                        let _ =
                            reply.send(Err(AgentError::InvalidConfiguration(error.to_string())));
                    }
                }
            }
            Control::Command { kind, reply } => {
                let mut candidate = self.kernel.clone();
                let (outcome, effects) = candidate.control(self.now(), kind);
                self.cancel_execution(&effects);
                let result = self.commit_candidate(candidate, effects).await;
                let _ = reply.send(result.map(|()| outcome));
            }
            Control::Shutdown => self.begin_shutdown().await,
        }
    }

    async fn handle_input(&mut self, command: InputCommand) {
        let mut candidate = self.kernel.clone();
        match candidate.accept(self.now(), command.input) {
            Ok((receipt, effects)) => {
                let result = self.commit_candidate(candidate, effects).await;
                let _ = command.reply.send(result.map(|()| receipt));
            }
            Err(error) => {
                let _ = command.reply.send(Err(error.into()));
            }
        }
    }

    async fn apply(&mut self, event: Event) {
        let mut candidate = self.kernel.clone();
        let effects = candidate.step(self.now(), event);
        if self.commit_candidate(candidate, effects).await.is_err() {
            self.fail_closed();
        }
    }

    async fn commit_candidate(
        &mut self,
        candidate: Kernel,
        effects: Vec<Effect>,
    ) -> Result<(), AgentError> {
        self.commit_state(candidate, &effects).await?;
        self.dispatch(effects);
        Ok(())
    }

    async fn commit_state(
        &mut self,
        candidate: Kernel,
        effects: &[Effect],
    ) -> Result<(), AgentError> {
        if self.shutdown_expired() {
            return Err(AgentError::ShuttingDown);
        }
        let mut cancelled_before_start = BTreeSet::new();
        if let Some(durable) = &self.durable {
            let durable = Arc::clone(durable);
            let snapshot = candidate.durable_snapshot()?;
            let revision = self
                .durable_revision
                .checked_add(1)
                .ok_or_else(|| DurableError::Invalid("durable revision exhausted".into()))?;
            let commit_id = format!("{}:{revision}", snapshot.epoch());
            let commit = durable.commit(DurableCommit {
                commit_id: commit_id.clone(),
                expected_revision: self.durable_revision,
                snapshot,
                records: notified_records(effects),
            });
            tokio::pin!(commit);
            let mut controls_open = true;
            // Keep receiving control and completion events while storage owns
            // the candidate. Only committed state is observable; queued events
            // are applied in receive order after this transaction is resolved.
            let receipt = loop {
                tokio::select! {
                    biased;
                    _ = wait_until(self.shutdown_at) => {
                        // Dropping an unacknowledged commit does not establish
                        // that it failed: the host may still complete its write.
                        // Never reuse this actor's revision or expose candidate.
                        self.interrupted_commit = Some(commit_id.clone());
                        self.shutting_down = true;
                        return Err(AgentError::CommitInterrupted { commit_id });
                    }
                    result = &mut commit => break result?,
                    control = self.controls.recv(), if controls_open => {
                        let control = match control {
                            Some(control) => control,
                            None => { controls_open = false; Control::Shutdown }
                        };
                        if let Control::Observe(reply) = control {
                            let records = self.records.subscribe();
                            let baseline = Arc::new(self.kernel.view());
                            let _ = reply.send(Observation { after: baseline.sequence, baseline, records });
                            continue;
                        }
                        let urgent = match &control {
                            Control::Shutdown => {
                                self.inputs.close();
                                if self.shutdown_at.is_none() {
                                    self.shutdown_at = Instant::now().checked_add(self.kernel.limits.shutdown_grace);
                                }
                                for running in self.running.values() {
                                    running.cancel.send_replace(true);
                                }
                                Some(KernelControl::Stop)
                            }
                            Control::Command { kind: kind @ (KernelControl::Cancel(_) | KernelControl::Pause(_) | KernelControl::Stop), .. } => Some(kind.clone()),
                            _ => None,
                        };
                        if let Some(kind) = urgent {
                            // Revoke execution promptly without acknowledging a
                            // state transition before its durable commit.
                            let mut preview = candidate.clone();
                            let (_, urgent_effects) = preview.control(self.now(), kind);
                            for effect in urgent_effects {
                                if let Effect::Cancel(call) = effect {
                                    if let Some(running) = self.running.get(&call) {
                                        running.cancel.send_replace(true);
                                    } else if effects.iter().any(|effect| matches!(effect, Effect::Start { id, .. } if *id == call)) {
                                        cancelled_before_start.insert(call);
                                    }
                                }
                            }
                        }
                        self.deferred.push_back(Deferred::Control(control));
                    }
                    Some(result) = self.tasks.join_next(), if !self.tasks.is_empty() => {
                        let event = match result {
                            Ok((call, event)) => { self.running.remove(&call); event }
                            Err(error) => self.join_error_event(error),
                        };
                        self.deferred.push_back(Deferred::Event(event));
                    }
                    Some((call, progress)) = self.progress_rx.recv() => {
                        self.deferred.push_back(Deferred::Event(Event::Progress { call, progress }));
                    }
                }
            };
            validate_receipt(&receipt, &commit_id, revision, candidate.view().sequence)?;
            self.durable_revision = receipt.revision;
        }
        self.cancelled_before_start.extend(cancelled_before_start);
        self.kernel = candidate;
        Ok(())
    }

    fn cancel_execution(&self, effects: &[Effect]) {
        // Host controls revoke execution immediately. Their state, replies, and
        // replacement calls are still withheld until durable acknowledgement.
        for effect in effects {
            if let Effect::Cancel(call) = effect
                && let Some(running) = self.running.get(call)
            {
                running.cancel.send_replace(true);
            }
        }
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
                    if self.cancelled_before_start.remove(&id) {
                        let failure = match call.as_ref() {
                            Call::Coordinate(_) => CallFailure::Coordinate,
                            Call::Work(_) => CallFailure::Work,
                            Call::Compact(_) => CallFailure::Compact,
                            Call::Tool(_) => CallFailure::Tool(ToolEffect::ReadOnly),
                        };
                        self.deferred.push_back(Deferred::Event(failed_event(
                            id,
                            failure,
                            CallErrorKind::Cancelled,
                            "cancelled before dispatch",
                        )));
                        continue;
                    }
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

    async fn handle_join_error(&mut self, error: JoinError) {
        let event = self.join_error_event(error);
        self.apply(event).await;
    }

    fn join_error_event(&mut self, error: JoinError) -> Event {
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
        failed_event(call, running.failure, kind, message)
    }

    async fn begin_shutdown(&mut self) {
        if self.shutting_down {
            return;
        }
        self.shutting_down = true;
        self.inputs.close();
        for running in self.running.values() {
            running.cancel.send_replace(true);
        }
        if self.shutdown_at.is_none() {
            self.shutdown_at = Instant::now().checked_add(self.kernel.limits.shutdown_grace);
        }
        let mut candidate = self.kernel.clone();
        let (_, effects) = candidate.control(self.now(), KernelControl::Stop);
        if self.commit_candidate(candidate, effects).await.is_err() {
            self.fail_closed();
        }
    }

    fn fail_closed(&mut self) {
        self.shutting_down = true;
        self.inputs.close();
        self.shutdown_at = Some(Instant::now());
    }

    fn shutdown_complete(&self) -> bool {
        self.shutting_down && (self.tasks.is_empty() || self.shutdown_expired())
    }

    fn shutdown_expired(&self) -> bool {
        self.shutdown_at
            .is_some_and(|deadline| Instant::now() >= deadline)
    }

    fn publish_shutdown(&self) {
        self.shutdown.send_replace(Some(ShutdownReport {
            unresolved_writes: self.kernel.unresolved_writes(),
            final_view: Arc::new(self.kernel.view()),
            pending_commit: self.interrupted_commit.clone(),
        }));
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
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use serde_json::{Value, json};

    use super::*;
    use crate::{
        Await, CallKind, CheckpointDraft, CompactInput, CoordinateInput, DurableReceipt,
        PortFuture, ToolSpec, WorkInput, WorkProposal, WorkStep,
    };

    #[test]
    fn shutdown_report_requires_explicit_commit_status() {
        let report = ShutdownReport::default();
        let mut encoded = serde_json::to_value(&report).unwrap();
        assert_eq!(
            serde_json::from_value::<ShutdownReport>(encoded.clone()).unwrap(),
            report
        );
        encoded.as_object_mut().unwrap().remove("pending_commit");
        assert!(serde_json::from_value::<ShutdownReport>(encoded).is_err());
    }

    #[derive(Default)]
    struct MemoryDurable {
        revision: Mutex<u64>,
        commits: Mutex<Vec<DurableCommit>>,
        fail: AtomicBool,
    }

    impl DurablePort for MemoryDurable {
        fn commit(
            &self,
            commit: DurableCommit,
        ) -> PortFuture<Result<DurableReceipt, DurableError>> {
            let result = if self.fail.load(Ordering::Acquire) {
                Err(DurableError::Storage("injected failure".into()))
            } else {
                let mut revision = self.revision.lock().unwrap();
                if commit.expected_revision != *revision {
                    Err(DurableError::Conflict)
                } else {
                    *revision += 1;
                    let receipt = DurableReceipt {
                        commit_id: commit.commit_id.clone(),
                        revision: *revision,
                        through: commit.snapshot.through(),
                    };
                    self.commits.lock().unwrap().push(commit);
                    Ok(receipt)
                }
            };
            Box::pin(future::ready(result))
        }
    }

    #[derive(Default)]
    struct GatedDurable {
        memory: Arc<MemoryDurable>,
        gate: Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
    }

    impl DurablePort for GatedDurable {
        fn commit(
            &self,
            commit: DurableCommit,
        ) -> PortFuture<Result<DurableReceipt, DurableError>> {
            let gate = self.gate.lock().unwrap().take();
            let memory = Arc::clone(&self.memory);
            Box::pin(async move {
                if let Some((entered, release)) = gate {
                    let _ = entered.send(());
                    release.await.unwrap();
                }
                memory.commit(commit).await
            })
        }
    }

    struct CancellableWorkerModel {
        contexts: mpsc::UnboundedSender<(JobId, CallContext)>,
    }

    impl ModelPort for CancellableWorkerModel {
        fn coordinate(
            &self,
            input: CoordinateInput,
            _: CallContext,
        ) -> PortFuture<Result<crate::KernelDecision, CallError>> {
            Box::pin(future::ready(Ok(crate::KernelDecision::Assign(vec![
                crate::RouteDelivery {
                    inputs: input.inputs.iter().map(|input| input.id).collect(),
                    target: crate::RouteTarget::New,
                    handoff: "work".into(),
                },
            ]))))
        }
        fn work(
            &self,
            input: WorkInput,
            context: CallContext,
        ) -> PortFuture<Result<WorkProposal, CallError>> {
            self.contexts.send((input.job, context)).unwrap();
            Box::pin(future::pending())
        }
        fn compact(
            &self,
            _: CompactInput,
            _: CallContext,
        ) -> PortFuture<Result<CheckpointDraft, CallError>> {
            Box::pin(future::pending())
        }
    }

    #[tokio::test]
    async fn pending_commit_receives_cancel_and_shutdown_and_retains_results() {
        for shutdown in [false, true] {
            let durable = Arc::new(GatedDurable::default());
            let (contexts, mut received) = mpsc::unbounded_channel();
            let agent = Agent::with_durable_ports(
                Arc::new(CancellableWorkerModel { contexts }),
                vec![],
                AgentLimits::default(),
                durable.clone(),
                None,
            )
            .await
            .unwrap();
            agent.post(Input::new(InputId(1), "first")).await.unwrap();
            let (job, mut context) = received.recv().await.unwrap();
            let call = context.id();
            let (entered, entering) = oneshot::channel();
            let (release, released) = oneshot::channel();
            *durable.gate.lock().unwrap() = Some((entered, released));
            let posting = {
                let agent = agent.clone();
                tokio::spawn(async move { agent.post(Input::new(InputId(2), "second")).await })
            };
            entering.await.unwrap();
            let controlling = {
                let agent = agent.clone();
                tokio::spawn(async move {
                    if shutdown {
                        agent.shutdown().await.map(|_| ())
                    } else {
                        agent.cancel(job).await.map(|_| ())
                    }
                })
            };
            tokio::time::timeout(Duration::from_secs(1), context.wait_for_cancellation())
                .await
                .expect("cancel signal must not wait for durable ACK");
            assert!(
                !controlling.is_finished(),
                "control ACK must still wait for durable state"
            );
            let observed = tokio::time::timeout(Duration::from_secs(1), agent.observe())
                .await
                .unwrap()
                .unwrap();
            assert!(
                !observed
                    .baseline
                    .inputs
                    .iter()
                    .any(|input| input.input.id == InputId(2))
            );
            release.send(()).unwrap();
            posting.await.unwrap().unwrap();
            tokio::time::timeout(Duration::from_secs(2), controlling)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if !shutdown {
                agent.shutdown().await.unwrap();
            }
            let commits = durable.memory.commits.lock().unwrap();
            assert!(
                commits
                    .windows(2)
                    .all(|pair| pair[1].expected_revision == pair[0].expected_revision + 1)
            );
            assert!(commits.iter().flat_map(|commit| &commit.records).any(|record| {
                matches!(&record.body, crate::RecordBody::CallFinished { call: finished, .. } if *finished == call)
            }), "cancelled model result must be retained and committed");
        }
    }

    #[tokio::test]
    async fn reconfigure_dispatches_only_new_model_after_durable_ack() {
        let durable = Arc::new(GatedDurable::default());
        let (old_started, mut old_starts) = mpsc::unbounded_channel();
        let (new_started, mut new_starts) = mpsc::unbounded_channel();
        let agent = Agent::with_durable_ports(
            Arc::new(PendingModel {
                started: old_started,
            }),
            vec![],
            AgentLimits::default(),
            durable.clone(),
            None,
        )
        .await
        .unwrap();
        agent
            .post(Input::new(InputId(1), "active routing"))
            .await
            .unwrap();
        assert_eq!(old_starts.recv().await, Some(()));
        let (entered, entering) = oneshot::channel();
        let (release, released) = oneshot::channel();
        *durable.gate.lock().unwrap() = Some((entered, released));
        let reconfigure = {
            let agent = agent.clone();
            tokio::spawn(async move {
                agent
                    .reconfigure(
                        Arc::new(PendingModel {
                            started: new_started,
                        }),
                        vec![],
                        AgentLimits::default(),
                    )
                    .await
            })
        };
        entering.await.unwrap();
        assert!(old_starts.try_recv().is_err());
        assert!(new_starts.try_recv().is_err());
        release.send(()).unwrap();
        reconfigure.await.unwrap().unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), new_starts.recv())
                .await
                .unwrap(),
            Some(())
        );
        assert!(old_starts.try_recv().is_err());
        agent.shutdown().await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_and_cancel_signal_before_their_own_commit_ack() {
        for cancel_first in [false, true] {
            let durable = Arc::new(GatedDurable::default());
            let (contexts, mut received) = mpsc::unbounded_channel();
            let agent = Agent::with_durable_ports(
                Arc::new(CancellableWorkerModel { contexts }),
                vec![],
                AgentLimits {
                    shutdown_grace: Duration::from_secs(1),
                    ..AgentLimits::default()
                },
                durable.clone(),
                None,
            )
            .await
            .unwrap();
            agent.post(Input::new(InputId(1), "work")).await.unwrap();
            let (job, mut context) = received.recv().await.unwrap();
            let baseline = agent.observe().await.unwrap().baseline;
            let (entered, entering) = oneshot::channel();
            let (_release, released) = oneshot::channel();
            *durable.gate.lock().unwrap() = Some((entered, released));
            let cancel = cancel_first.then(|| {
                let agent = agent.clone();
                tokio::spawn(async move { agent.cancel(job).await })
            });
            let shutdown = (!cancel_first).then(|| {
                let agent = agent.clone();
                tokio::spawn(async move { agent.shutdown().await })
            });
            entering.await.unwrap();
            tokio::time::timeout(Duration::from_millis(50), context.wait_for_cancellation())
                .await
                .expect("execution cancellation must precede its own durable ACK");
            let shutdown = shutdown.unwrap_or_else(|| {
                let agent = agent.clone();
                tokio::spawn(async move { agent.shutdown().await })
            });
            let report = tokio::time::timeout(Duration::from_secs(2), shutdown)
                .await
                .expect("unacknowledged commit cannot extend shutdown grace")
                .unwrap()
                .unwrap();
            assert!(report.pending_commit.is_some());
            assert_eq!(report.final_view, baseline);
            if let Some(cancel) = cancel {
                assert!(matches!(
                    cancel.await.unwrap(),
                    Err(AgentError::CommitInterrupted { .. })
                ));
            }
        }
    }

    struct GatedWrite {
        contexts: mpsc::UnboundedSender<CallContext>,
        release: Mutex<Option<oneshot::Receiver<()>>>,
    }

    impl ToolPort for GatedWrite {
        fn specification(&self) -> ToolSpec {
            ToolSpec {
                name: "write".into(),
                description: "test write".into(),
                parameters: json!({"type":"object"}),
                effect: ToolEffect::ExternalWrite,
            }
        }
        fn run(&self, _: Value, context: CallContext) -> PortFuture<ToolOutcome> {
            self.contexts.send(context).unwrap();
            let release = self.release.lock().unwrap().take().unwrap();
            Box::pin(async move {
                release.await.unwrap();
                ToolOutcome {
                    result: Ok(json!("applied")),
                    external_effect: ExternalEffect::Applied,
                }
            })
        }
    }

    struct WriteModel;
    impl ModelPort for WriteModel {
        fn coordinate(
            &self,
            input: CoordinateInput,
            _: CallContext,
        ) -> PortFuture<Result<crate::KernelDecision, CallError>> {
            Box::pin(future::ready(Ok(crate::KernelDecision::Assign(vec![
                crate::RouteDelivery {
                    inputs: input.inputs.iter().map(|input| input.id).collect(),
                    target: crate::RouteTarget::New,
                    handoff: "write".into(),
                },
            ]))))
        }
        fn work(
            &self,
            _: WorkInput,
            _: CallContext,
        ) -> PortFuture<Result<WorkProposal, CallError>> {
            Box::pin(future::ready(Ok(WorkProposal::new(WorkStep::Tool(
                crate::ToolCall::new("write", json!({})),
            )))))
        }
        fn compact(
            &self,
            _: CompactInput,
            _: CallContext,
        ) -> PortFuture<Result<CheckpointDraft, CallError>> {
            Box::pin(future::pending())
        }
    }

    #[tokio::test]
    async fn tool_truth_arriving_during_pending_commit_survives_shutdown() {
        let durable = Arc::new(GatedDurable::default());
        let (contexts, mut received) = mpsc::unbounded_channel();
        let (finish_write, release_write) = oneshot::channel();
        let agent = Agent::with_durable_ports(
            Arc::new(WriteModel),
            vec![Arc::new(GatedWrite {
                contexts,
                release: Mutex::new(Some(release_write)),
            })],
            AgentLimits::default(),
            durable.clone(),
            None,
        )
        .await
        .unwrap();
        agent.post(Input::new(InputId(1), "write")).await.unwrap();
        let mut context = received.recv().await.unwrap();
        let write_call = context.id();
        let (entered, entering) = oneshot::channel();
        let (release, released) = oneshot::channel();
        *durable.gate.lock().unwrap() = Some((entered, released));
        let posting = {
            let agent = agent.clone();
            tokio::spawn(async move { agent.post(Input::new(InputId(2), "new input")).await })
        };
        entering.await.unwrap();
        let shutdown = {
            let agent = agent.clone();
            tokio::spawn(async move { agent.shutdown().await })
        };
        tokio::time::timeout(Duration::from_secs(1), context.wait_for_cancellation())
            .await
            .unwrap();
        finish_write.send(()).unwrap();
        // The committed view remains responsive while the real write completes.
        tokio::time::timeout(Duration::from_secs(1), agent.observe())
            .await
            .unwrap()
            .unwrap();
        release.send(()).unwrap();
        posting.await.unwrap().unwrap();
        tokio::time::timeout(Duration::from_secs(2), shutdown)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(durable.memory.commits.lock().unwrap().iter().flat_map(|commit| &commit.records).any(|record| {
            matches!(&record.body, crate::RecordBody::ToolFinished { call, outcome, .. } if *call == write_call && outcome.external_effect == ExternalEffect::Applied)
        }));
    }

    #[tokio::test(start_paused = true)]
    async fn pending_commit_shutdown_keeps_unacknowledged_write_truth_unresolved() {
        let durable = Arc::new(GatedDurable::default());
        let (contexts, mut received) = mpsc::unbounded_channel();
        let (finish_write, release_write) = oneshot::channel();
        let agent = Agent::with_durable_ports(
            Arc::new(WriteModel),
            vec![Arc::new(GatedWrite {
                contexts,
                release: Mutex::new(Some(release_write)),
            })],
            AgentLimits {
                shutdown_grace: Duration::from_secs(1),
                ..AgentLimits::default()
            },
            durable.clone(),
            None,
        )
        .await
        .unwrap();
        agent.post(Input::new(InputId(1), "write")).await.unwrap();
        let mut context = received.recv().await.unwrap();
        let write_call = context.id();
        let baseline = agent.observe().await.unwrap().baseline;
        let (entered, entering) = oneshot::channel();
        let (_release, released) = oneshot::channel();
        *durable.gate.lock().unwrap() = Some((entered, released));
        let posting = {
            let agent = agent.clone();
            tokio::spawn(async move { agent.post(Input::new(InputId(2), "pending")).await })
        };
        entering.await.unwrap();
        let shutdown = {
            let agent = agent.clone();
            tokio::spawn(async move { agent.shutdown().await })
        };
        tokio::time::timeout(Duration::from_millis(50), context.wait_for_cancellation())
            .await
            .unwrap();
        finish_write.send(()).unwrap();
        let report = tokio::time::timeout(Duration::from_secs(2), shutdown)
            .await
            .expect("shutdown must finish even when storage never acknowledges")
            .unwrap()
            .unwrap();
        assert!(matches!(
            posting.await.unwrap(),
            Err(AgentError::CommitInterrupted { commit_id }) if Some(&commit_id) == report.pending_commit.as_ref()
        ));
        assert_eq!(report.final_view, baseline);
        assert_eq!(report.unresolved_writes.len(), 1);
        assert_eq!(report.unresolved_writes[0].call, write_call);
        assert!(!durable.memory.commits.lock().unwrap().iter().flat_map(|commit| &commit.records).any(|record| {
            matches!(&record.body, crate::RecordBody::ToolFinished { call, .. } if *call == write_call)
        }));
    }

    #[tokio::test]
    async fn durable_commit_precedes_input_receipt_and_model_start() {
        let (started, mut starts) = mpsc::unbounded_channel();
        let model = Arc::new(PendingModel { started });
        let durable = Arc::new(MemoryDurable::default());
        let agent = Agent::with_durable_ports(
            model,
            Vec::new(),
            AgentLimits::default(),
            durable.clone(),
            None,
        )
        .await
        .unwrap();

        agent
            .post(Input::new(InputId(1), "persist first"))
            .await
            .unwrap();
        assert_eq!(starts.recv().await, Some(()));
        {
            let commits = durable.commits.lock().unwrap();
            assert_eq!(commits.len(), 2);
            assert!(
                commits[1]
                    .records
                    .iter()
                    .any(|record| matches!(record.body, crate::RecordBody::Input(_)))
            );
            assert!(commits[1].records.iter().any(|record| matches!(
                record.body,
                crate::RecordBody::CallStarted {
                    kind: CallKind::Coordinate,
                    ..
                }
            )));
        }
        agent.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn durable_failure_does_not_accept_input_or_start_model() {
        let (started, mut starts) = mpsc::unbounded_channel();
        let model = Arc::new(PendingModel { started });
        let durable = Arc::new(MemoryDurable::default());
        let agent = Agent::with_durable_ports(
            model,
            Vec::new(),
            AgentLimits::default(),
            durable.clone(),
            None,
        )
        .await
        .unwrap();
        durable.fail.store(true, Ordering::Release);

        assert!(matches!(
            agent.post(Input::new(InputId(1), "must not run")).await,
            Err(AgentError::Durable(DurableError::Storage(_)))
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), starts.recv())
                .await
                .is_err()
        );
        durable.fail.store(false, Ordering::Release);
        agent.shutdown().await.unwrap();
    }

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
            Box::pin(async { Ok(crate::KernelDecision::Clarify("retry routing".into())) })
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
            let inputs = input.inputs.into_iter().map(|input| input.id).collect();
            Box::pin(async move {
                Ok(crate::KernelDecision::Assign(vec![crate::RouteDelivery {
                    inputs,
                    target: crate::RouteTarget::New,
                    handoff: "wait".into(),
                }]))
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
