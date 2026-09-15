use std::{
    collections::BTreeMap,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use bone_core::{
    Agent, AgentError, AgentView, CallError, CallKind, CallStatus, ControlOutcome, ExternalEffect,
    Input as AgentInput, InputId as AgentInputId, InputStatus as AgentInputStatus, JobId,
    JobStatus, Owner, Record, RecordBody, Seq, ToolOutcome, WaitView,
};
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::{
    AcceptanceDecision, AcceptanceReceipt, AcceptanceSubmission, ActivityKind, ActivityView,
    AppProblem, CallRef, CloseReport, CommandReceipt, ConfigProblem, ConfigScope, DataStore, Error,
    HistoryCursor, HistoryPage, InputId, InputState, InputView, JobControl, JobOwner, JobRef,
    JobReport, JobState, JobView, RecentHistoryPage, Result, RuntimeConfig, RuntimeId,
    RuntimeState, SavedRuntime, SavedSession, SessionEvent, SessionId, SessionReleaseStatus,
    SessionRetentionReason, SessionSeq, SessionView, SubmissionReceipt, SubmitInput, WaitReason,
    WriteResolution,
    config::{MAX_PERSISTED_VALUE_BYTES, resolve_runtime},
    persistence::{AcceptError, AcceptanceError, ResolveWriteResult},
    storage::Lease,
    tools,
};

use crate::app::RuntimeBackend;

const COMMAND_QUEUE: usize = 64;
const HISTORY_PAGE_LIMIT: usize = 32;

/// A cloneable, frontend-neutral handle to one durable session.
#[derive(Clone)]
pub struct Session {
    id: SessionId,
    commands: mpsc::Sender<Command>,
    shutdown_target: watch::Receiver<Option<Agent>>,
    view: watch::Receiver<Arc<SessionView>>,
    closed: Arc<AtomicBool>,
    clean_shutdown: Arc<OnceLock<CloseReport>>,
}

impl Session {
    pub fn id(&self) -> SessionId {
        self.id
    }

    pub(crate) fn workspace(&self) -> crate::WorkspaceId {
        self.view.borrow().session.workspace
    }

    /// Reapply the currently persisted desired configuration.
    ///
    /// Frontends can call this after repairing an external prerequisite such
    /// as provider credentials; it does not write another configuration value.
    /// A running Runtime is fully reconfigured before this returns. A detached
    /// Session may only have scheduled asynchronous startup, whose result is
    /// reported through [`Session::observe`].
    pub async fn reload_config(&self) -> Result<()> {
        self.apply_persisted_config(true).await
    }

    pub(crate) async fn apply_persisted_config(&self, force: bool) -> Result<()> {
        self.request(|reply| Command::ConfigChanged { force, reply })
            .await
    }

    pub async fn submit(&self, input: SubmitInput) -> Result<SubmissionReceipt> {
        if input.text.len() > MAX_PERSISTED_VALUE_BYTES {
            return Err(Error::InvalidState("input exceeds 1 MiB".into()));
        }
        self.request(|reply| Command::Submit { input, reply }).await
    }

    pub async fn submit_acceptance(
        &self,
        submission: AcceptanceSubmission,
    ) -> Result<AcceptanceReceipt> {
        if submission.result.session != self.id {
            return Err(Error::InvalidState(
                "acceptance result belongs to another session".into(),
            ));
        }
        if submission.reason.len() > MAX_PERSISTED_VALUE_BYTES {
            return Err(Error::InvalidState(
                "acceptance reason exceeds 1 MiB".into(),
            ));
        }
        if submission.decision != AcceptanceDecision::Accepted
            && submission.reason.trim().is_empty()
        {
            return Err(Error::InvalidState(
                "this acceptance decision requires a reason".into(),
            ));
        }
        match (submission.decision, &submission.rework) {
            (AcceptanceDecision::Rejected, Some(input))
                if input.text.len() <= MAX_PERSISTED_VALUE_BYTES && input.reply_to.is_none() => {}
            (AcceptanceDecision::Rejected, None) => {
                return Err(Error::InvalidState(
                    "rejected acceptance requires a new rework input".into(),
                ));
            }
            (AcceptanceDecision::Rejected, Some(_)) => {
                return Err(Error::InvalidState(
                    "rework must be a new standalone input of at most 1 MiB".into(),
                ));
            }
            (_, Some(_)) => {
                return Err(Error::InvalidState(
                    "only a rejected result can include rework input".into(),
                ));
            }
            (_, None) => {}
        }
        self.request(|reply| Command::SubmitAcceptance { submission, reply })
            .await
    }

    pub async fn retry(&self, input: InputId) -> Result<CommandReceipt> {
        self.request(|reply| Command::Retry { input, reply }).await
    }

    pub async fn control(&self, target: JobRef, action: JobControl) -> Result<CommandReceipt> {
        self.request(|reply| Command::Control {
            target,
            action,
            reply,
        })
        .await
    }

    pub async fn stop(&self) -> Result<CommandReceipt> {
        self.request(|reply| Command::Stop { reply }).await
    }

    pub async fn snapshot(&self) -> Result<SessionView> {
        self.request(|reply| Command::Snapshot { reply }).await
    }

    pub fn observe(&self) -> watch::Receiver<Arc<SessionView>> {
        self.view.clone()
    }

    pub(crate) fn actor_closed(&self) -> bool {
        self.commands.is_closed()
    }

    pub async fn history(&self, after: SessionSeq, limit: usize) -> Result<HistoryPage> {
        self.request(|reply| Command::History {
            after,
            limit,
            reply,
        })
        .await
    }

    /// Read the newest durable history page, then page backward with the
    /// returned cursor. Concurrent appends never enter an existing snapshot.
    pub async fn recent_history(
        &self,
        cursor: Option<HistoryCursor>,
        limit: usize,
    ) -> Result<RecentHistoryPage> {
        if cursor.is_some_and(|cursor| cursor.session() != self.id()) {
            return Err(Error::InvalidState(
                "history cursor belongs to another session".into(),
            ));
        }
        self.request(|reply| Command::RecentHistory {
            cursor,
            limit,
            reply,
        })
        .await
    }

    pub async fn rename(&self, title: impl Into<String>) -> Result<()> {
        self.request(|reply| Command::Rename {
            title: title.into(),
            reply,
        })
        .await
    }

    /// Replace the creation placeholder with a title derived from the first
    /// submitted text. Returns false if the text is empty or the title has
    /// already been finalized (including by a manual rename).
    pub async fn title_from_first_input(&self, input: &str) -> Result<bool> {
        let Some(title) = crate::app::title_from_first_input(input) else {
            return Ok(false);
        };
        self.request(|reply| Command::RenameIfProvisional { title, reply })
            .await
    }

    pub async fn save_draft(&self, draft: impl Into<String>) -> Result<()> {
        let draft = draft.into();
        if draft.len() > MAX_PERSISTED_VALUE_BYTES {
            return Err(Error::InvalidState("draft exceeds 1 MiB".into()));
        }
        self.request(|reply| Command::SaveDraft { draft, reply })
            .await
    }

    pub async fn archive(&self, archived: bool) -> Result<()> {
        self.request(|reply| Command::Archive { archived, reply })
            .await
    }

    pub async fn resolve_write(
        &self,
        target: CallRef,
        resolution: WriteResolution,
    ) -> Result<CommandReceipt> {
        if resolution.evidence.len() > MAX_PERSISTED_VALUE_BYTES {
            return Err(Error::InvalidState(
                "write resolution evidence exceeds 1 MiB".into(),
            ));
        }
        self.request(|reply| Command::ResolveWrite {
            target,
            resolution,
            reply,
        })
        .await
    }

    pub async fn close_runtime(&self) -> Result<CloseReport> {
        self.request(|reply| Command::CloseRuntime { reply }).await
    }

    pub(crate) fn spawn(
        store: DataStore,
        backend: RuntimeBackend,
        saved: SavedSession,
        write_gate: Arc<tools::WriteGate>,
        closed: Arc<AtomicBool>,
    ) -> Result<Self> {
        let id = saved.info.id;
        let lease = Arc::new(store.claim_session(id).map_err(|error| match error {
            crate::storage::StoreError::Busy => Error::SessionBusy(id),
            error => error.into(),
        })?);
        let (saved, inputs) = store.recover_session(id)?;
        let inputs = inputs
            .into_iter()
            .filter(|input| !input.state.terminal())
            .collect::<Vec<_>>();
        let mut initial = crate::default_view(saved.info.clone());
        Arc::make_mut(&mut initial).draft = saved.draft.clone();
        Arc::make_mut(&mut initial).inputs = inputs.clone();
        Arc::make_mut(&mut initial).history_through = store.history_through(id)?;
        let (view_tx, view) = watch::channel(initial);
        let (commands, receiver) = mpsc::channel(COMMAND_QUEUE);
        let (shutdown_target_tx, shutdown_target) = watch::channel(None);
        let clean_shutdown = Arc::new(OnceLock::new());
        tokio::spawn(
            SessionTask {
                store,
                backend,
                info: saved.info,
                draft: saved.draft,
                inputs: inputs.into_iter().map(|input| (input.id, input)).collect(),
                runtime: None,
                starting: None,
                runtime_state: RuntimeState::Detached,
                problem: None,
                config_blocked: false,
                execution_blocked: false,
                write_gate,
                view: view_tx,
                commands: receiver,
                shutdown_target: shutdown_target_tx,
                durable_gate: Arc::new(tokio::sync::Mutex::new(())),
                command_tx: commands.downgrade(),
                clean_shutdown: Arc::clone(&clean_shutdown),
                lease: Some(lease),
            }
            .run(),
        );
        Ok(Self {
            id,
            commands,
            shutdown_target,
            view,
            closed,
            clean_shutdown,
        })
    }

    pub(crate) async fn shutdown(&self) -> Result<CloseReport> {
        match self
            .request_unchecked(|reply| Command::Shutdown { reply })
            .await
        {
            Err(Error::Closed) => self.clean_shutdown.get().cloned().ok_or(Error::Closed),
            result => result,
        }
    }

    pub(crate) async fn release_if_idle(&self) -> Result<SessionReleaseStatus> {
        let status = self
            .request_unchecked(|reply| Command::ReleaseIfIdle { reply })
            .await?;
        if status == SessionReleaseStatus::Released {
            self.commands.closed().await;
        }
        Ok(status)
    }

    async fn request<T>(
        &self,
        command: impl FnOnce(oneshot::Sender<Result<T>>) -> Command,
    ) -> Result<T> {
        if self.closed.load(Ordering::Acquire) {
            return Err(Error::Closed);
        }
        self.request_unchecked(command).await
    }

    async fn request_unchecked<T>(
        &self,
        command: impl FnOnce(oneshot::Sender<Result<T>>) -> Command,
    ) -> Result<T> {
        let (reply, result) = oneshot::channel();
        let command = command(reply);
        let closing = matches!(
            command,
            Command::CloseRuntime { .. } | Command::Shutdown { .. }
        );
        let request = async {
            self.commands
                .send(command)
                .await
                .map_err(|_| Error::Closed)?;
            result.await.map_err(|_| Error::Closed)?
        };
        if closing {
            with_shutdown_forwarding(self.shutdown_target.clone(), request).await
        } else {
            request.await
        }
    }
}

async fn with_shutdown_forwarding<T>(
    mut targets: watch::Receiver<Option<Agent>>,
    request: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    // RuntimeReady may install Core after the close request is queued, then
    // block the session actor on a commit. Watch future installations as well
    // as the current runtime; this forwarding future lives only as long as the
    // request, and forwards once per watch update rather than spawning tasks.
    let forward = async {
        loop {
            let agent = targets.borrow_and_update().clone();
            if let Some(agent) = agent {
                let _ = agent.shutdown().await;
            }
            if targets.changed().await.is_err() {
                return;
            }
        }
    };
    tokio::pin!(request);
    tokio::select! {
        result = &mut request => result,
        _ = forward => request.await,
    }
}

enum Command {
    Submit {
        input: SubmitInput,
        reply: oneshot::Sender<Result<SubmissionReceipt>>,
    },
    SubmitAcceptance {
        submission: AcceptanceSubmission,
        reply: oneshot::Sender<Result<AcceptanceReceipt>>,
    },
    Retry {
        input: InputId,
        reply: oneshot::Sender<Result<CommandReceipt>>,
    },
    Control {
        target: JobRef,
        action: JobControl,
        reply: oneshot::Sender<Result<CommandReceipt>>,
    },
    Stop {
        reply: oneshot::Sender<Result<CommandReceipt>>,
    },
    Snapshot {
        reply: oneshot::Sender<Result<SessionView>>,
    },
    History {
        after: SessionSeq,
        limit: usize,
        reply: oneshot::Sender<Result<HistoryPage>>,
    },
    RecentHistory {
        cursor: Option<HistoryCursor>,
        limit: usize,
        reply: oneshot::Sender<Result<RecentHistoryPage>>,
    },
    Rename {
        title: String,
        reply: oneshot::Sender<Result<()>>,
    },
    RenameIfProvisional {
        title: String,
        reply: oneshot::Sender<Result<bool>>,
    },
    SaveDraft {
        draft: String,
        reply: oneshot::Sender<Result<()>>,
    },
    Archive {
        archived: bool,
        reply: oneshot::Sender<Result<()>>,
    },
    ResolveWrite {
        target: CallRef,
        resolution: WriteResolution,
        reply: oneshot::Sender<Result<CommandReceipt>>,
    },
    CloseRuntime {
        reply: oneshot::Sender<Result<CloseReport>>,
    },
    ConfigChanged {
        force: bool,
        reply: oneshot::Sender<Result<()>>,
    },
    RuntimeDirty(RuntimeId),
    WriteCompleted(CallRef),
    RuntimeReady(RuntimeId),
    Shutdown {
        reply: oneshot::Sender<Result<CloseReport>>,
    },
    ReleaseIfIdle {
        reply: oneshot::Sender<Result<SessionReleaseStatus>>,
    },
}

struct RunningRuntime {
    id: RuntimeId,
    agent: Agent,
    agent_through: u64,
    view: Arc<AgentView>,
    dirty: Arc<AtomicBool>,
}

struct StartingRuntime {
    id: RuntimeId,
    task: tokio::task::JoinHandle<()>,
    ready: oneshot::Receiver<RuntimeReady>,
}

struct RuntimeReady {
    id: RuntimeId,
    config: RuntimeConfig,
    archive_from: u64,
    result: Result<(Agent, bone_core::Observation)>,
}

struct SessionTask {
    store: DataStore,
    backend: RuntimeBackend,
    info: crate::SessionInfo,
    draft: String,
    inputs: BTreeMap<InputId, InputView>,
    runtime: Option<RunningRuntime>,
    starting: Option<StartingRuntime>,
    runtime_state: RuntimeState,
    problem: Option<AppProblem>,
    config_blocked: bool,
    execution_blocked: bool,
    write_gate: Arc<tools::WriteGate>,
    view: watch::Sender<Arc<SessionView>>,
    // Taken explicitly at the end of `run`, before the command receiver closes
    // and wakes `Session::release_if_idle`.
    lease: Option<Arc<Lease>>,
    commands: mpsc::Receiver<Command>,
    shutdown_target: watch::Sender<Option<Agent>>,
    durable_gate: Arc<tokio::sync::Mutex<()>>,
    command_tx: mpsc::WeakSender<Command>,
    clean_shutdown: Arc<OnceLock<CloseReport>>,
}

impl SessionTask {
    async fn run(mut self) {
        while let Some(command) = self.commands.recv().await {
            if self.handle(command).await {
                break;
            }
        }
        if self.runtime.is_some() {
            let _ = self.close_runtime().await;
        }
        self.shutdown_target.send_replace(None);
        // `Session::release_if_idle` uses `Sender::closed()` as its completion
        // acknowledgement. Release the actor-owned OS writer lease before that
        // acknowledgement can fire; relying on async-generator field drop order
        // leaves a small but observable Busy window under parallel load.
        drop(self.lease.take());
    }

    async fn handle(&mut self, command: Command) -> bool {
        let mut shutdown = false;
        match command {
            Command::Submit { input, reply } => {
                let accepted = self.store.accept_input(self.info.id, &input);
                match accepted {
                    Ok((receipt, saved)) => {
                        if !saved.state.terminal() {
                            self.inputs.insert(saved.id, saved);
                        }
                        if let Err(error) = self.publish() {
                            self.record_execution_error(&error).await;
                        }
                        let _ = reply.send(Ok(receipt));
                        if let Err(error) = self.deliver_queued().await {
                            self.record_execution_error(&error).await;
                        }
                    }
                    Err(error) => {
                        let error = map_accept_error(error);
                        self.record_execution_error(&error).await;
                        let _ = reply.send(Err(error));
                    }
                }
            }
            Command::SubmitAcceptance { submission, reply } => {
                match self.store.record_acceptance(&submission) {
                    Ok((receipt, rework)) => {
                        if let Some(saved) = rework
                            && !saved.state.terminal()
                        {
                            self.inputs.insert(saved.id, saved);
                        }
                        if let Err(error) = self.publish() {
                            self.record_execution_error(&error).await;
                        }
                        let _ = reply.send(Ok(receipt));
                        if let Err(error) = self.deliver_queued().await {
                            self.record_execution_error(&error).await;
                        }
                    }
                    Err(error) => {
                        let error = match error {
                            AcceptanceError::ResultNotFound => {
                                Error::InvalidState("acceptance result not found".into())
                            }
                            AcceptanceError::Conflict => Error::RequestConflict,
                            AcceptanceError::Store(error) => error.into(),
                        };
                        self.record_execution_error(&error).await;
                        let _ = reply.send(Err(error));
                    }
                }
            }
            Command::Retry { input, reply } => {
                let result = self.retry(input).await;
                if let Err(error) = &result {
                    self.record_execution_error(error).await;
                }
                let _ = reply.send(result);
            }
            Command::Control {
                target,
                action,
                reply,
            } => {
                let result = self.control(target, action).await;
                if let Err(error) = &result {
                    self.record_execution_error(error).await;
                }
                let _ = reply.send(result);
            }
            Command::Stop { reply } => {
                let result = self.stop().await;
                if let Err(error) = &result {
                    self.record_execution_error(error).await;
                }
                let _ = reply.send(result);
            }
            Command::Snapshot { reply } => {
                let result = match self.refresh_runtime().await {
                    Ok(()) => self.publish().map(|()| self.view.borrow().as_ref().clone()),
                    Err(error) => Err(error),
                };
                if let Err(error) = &result {
                    self.record_execution_error(error).await;
                }
                let _ = reply.send(result);
            }
            Command::History {
                after,
                limit,
                reply,
            } => {
                let result = self
                    .store
                    .history(self.info.id, after, limit.clamp(1, HISTORY_PAGE_LIMIT))
                    .map_err(Into::into);
                if let Err(error) = &result {
                    self.record_execution_error(error).await;
                }
                let _ = reply.send(result);
            }
            Command::RecentHistory {
                cursor,
                limit,
                reply,
            } => {
                let result = self
                    .store
                    .recent_history(self.info.id, cursor, limit.clamp(1, HISTORY_PAGE_LIMIT))
                    .map_err(Into::into);
                if let Err(error) = &result {
                    self.record_execution_error(error).await;
                }
                let _ = reply.send(result);
            }
            Command::Rename { title, reply } => {
                let result = self.rename(title);
                if let Err(error) = &result {
                    self.record_execution_error(error).await;
                }
                let _ = reply.send(result);
            }
            Command::RenameIfProvisional { title, reply } => {
                let result = self.rename_if_provisional(title);
                let _ = reply.send(result);
            }
            Command::SaveDraft { draft, reply } => {
                let result = self
                    .store
                    .save_draft(self.info.id, draft.clone())
                    .map_err(Error::from)
                    .and_then(|()| {
                        self.draft = draft;
                        self.publish()
                    });
                if let Err(error) = &result {
                    self.record_execution_error(error).await;
                }
                let _ = reply.send(result);
            }
            Command::Archive { archived, reply } => {
                let result = self.archive(archived);
                if let Err(error) = &result {
                    self.record_execution_error(error).await;
                }
                let _ = reply.send(result);
            }
            Command::ResolveWrite {
                target,
                resolution,
                reply,
            } => {
                let result = self.resolve_write(target, resolution).await;
                if let Err(error) = &result {
                    self.record_execution_error(error).await;
                }
                let _ = reply.send(result);
            }
            Command::CloseRuntime { reply } => {
                let result = self.close_runtime().await;
                if let Err(error) = &result {
                    self.record_execution_error(error).await;
                }
                let _ = reply.send(result);
            }
            Command::ConfigChanged { force, reply } => {
                let result = self.apply_config(force).await;
                if let Err(error) = &result {
                    self.record_execution_error(error).await;
                }
                let _ = reply.send(result);
            }
            Command::RuntimeDirty(runtime) => {
                if self
                    .runtime
                    .as_ref()
                    .is_some_and(|current| current.id == runtime)
                {
                    self.runtime
                        .as_ref()
                        .expect("current runtime checked")
                        .dirty
                        .store(false, Ordering::Release);
                    let result = match self.refresh_runtime().await {
                        Ok(()) => self.deliver_queued().await,
                        Err(error) => Err(error),
                    };
                    if let Err(error) = result {
                        self.record_execution_error(&error).await;
                    } else {
                        let _ = self.publish();
                    }
                }
            }
            Command::WriteCompleted(call) => {
                let result = match self.resolve_known_write(call).await {
                    Ok(()) => self.refresh_runtime().await,
                    Err(error) => Err(error),
                };
                if let Err(error) = result {
                    self.record_execution_error(&error).await;
                } else {
                    let _ = self.publish();
                }
            }
            Command::RuntimeReady(runtime) => self.runtime_ready(runtime).await,
            Command::Shutdown { reply } => {
                let result = self.close_runtime().await;
                if let Ok(report) = &result {
                    let _ = self.clean_shutdown.set(report.clone());
                    shutdown = true;
                } else if let Err(error) = &result {
                    self.record_execution_error(error).await;
                }
                let _ = reply.send(result);
            }
            Command::ReleaseIfIdle { reply } => {
                let result = self.release_if_idle().await;
                if matches!(&result, Ok(SessionReleaseStatus::Released)) {
                    let _ = self.clean_shutdown.set(CloseReport {
                        unresolved_writes: Vec::new(),
                    });
                    shutdown = true;
                } else if let Err(error) = &result {
                    self.record_execution_error(error).await;
                }
                let _ = reply.send(result);
            }
        }
        shutdown
    }

    async fn release_if_idle(&mut self) -> Result<SessionReleaseStatus> {
        if self.starting.is_some() || matches!(self.runtime_state, RuntimeState::Starting) {
            return Ok(SessionReleaseStatus::Retained(
                SessionRetentionReason::Starting,
            ));
        }
        if matches!(self.runtime_state, RuntimeState::Closing { .. }) {
            return Ok(SessionReleaseStatus::Retained(
                SessionRetentionReason::ActiveWork,
            ));
        }
        if self.durable_gate.try_lock().is_err() {
            return Ok(SessionReleaseStatus::Retained(
                SessionRetentionReason::PersistenceInFlight,
            ));
        }
        self.refresh_runtime().await?;
        if let Some(runtime) = &self.runtime
            && (runtime
                .view
                .jobs
                .iter()
                .any(|job| !matches!(job.status, JobStatus::Finished(_)))
                || runtime
                    .view
                    .calls
                    .iter()
                    .any(|call| !matches!(call.status, CallStatus::Finished { .. })))
        {
            return Ok(SessionReleaseStatus::Retained(
                SessionRetentionReason::ActiveWork,
            ));
        }
        let unresolved = self
            .store
            .unresolved_writes(self.info.workspace, Some(self.info.id))?;
        if !unresolved.is_empty() {
            return Ok(SessionReleaseStatus::Retained(
                SessionRetentionReason::UnresolvedWrites,
            ));
        }
        if self.runtime.is_none() {
            return Ok(SessionReleaseStatus::Released);
        }
        let report = self.close_runtime().await?;
        if !report.unresolved_writes.is_empty() {
            return Ok(SessionReleaseStatus::Retained(
                SessionRetentionReason::UnresolvedWrites,
            ));
        }
        Ok(SessionReleaseStatus::Released)
    }

    async fn retry(&mut self, input: InputId) -> Result<CommandReceipt> {
        if self.execution_blocked {
            self.refresh_runtime().await?;
            self.publish()?;
        }
        let state = match self.inputs.get(&input) {
            Some(input) => input.state.clone(),
            None => self
                .store
                .input(self.info.id, input)?
                .map(|input| input.state)
                .ok_or(Error::InvalidState("input does not exist".into()))?,
        };
        match state {
            InputState::Queued { .. } => {
                let was_starting = self.starting.is_some();
                self.deliver_queued().await?;
                Ok(
                    if self
                        .inputs
                        .get(&input)
                        .is_some_and(|input| !matches!(input.state, InputState::Queued { .. }))
                        || (!was_starting && self.starting.is_some())
                    {
                        CommandReceipt::Applied
                    } else {
                        CommandReceipt::Unchanged
                    },
                )
            }
            InputState::RoutingFailed { runtime, .. } => {
                let current = self.current_runtime(runtime)?;
                let outcome = current
                    .agent
                    .retry(AgentInputId(input.0))
                    .await
                    .map_err(agent_error)?;
                self.refresh_runtime().await?;
                let restored = restored_routing_inputs(
                    runtime,
                    &self.inputs,
                    &self.current_runtime(runtime)?.view,
                );
                if !restored.is_empty() {
                    for input in self.store.accept_inputs(self.info.id, &restored, runtime)? {
                        self.inputs.insert(input.id, input);
                    }
                }
                self.publish()?;
                Ok(
                    if outcome == ControlOutcome::Applied || !restored.is_empty() {
                        CommandReceipt::Applied
                    } else {
                        CommandReceipt::Unchanged
                    },
                )
            }
            _ => Ok(CommandReceipt::Unchanged),
        }
    }

    async fn control(&mut self, target: JobRef, action: JobControl) -> Result<CommandReceipt> {
        let runtime = self.current_runtime(target.runtime)?;
        let job = JobId(target.id);
        let outcome = match action {
            JobControl::Pause => runtime.agent.pause(job).await,
            JobControl::Resume => runtime.agent.resume(job).await,
            JobControl::Cancel => runtime.agent.cancel(job).await,
        }
        .map_err(agent_error)?;
        self.refresh_runtime().await?;
        self.publish()?;
        Ok(receipt(outcome))
    }

    async fn stop(&mut self) -> Result<CommandReceipt> {
        let queued = self
            .inputs
            .values()
            .filter(|input| matches!(input.state, InputState::Queued { .. }))
            .map(|input| input.id)
            .collect::<Vec<_>>();
        let had_queued = !queued.is_empty();
        if had_queued {
            self.store.cancel_inputs(self.info.id, &queued)?;
        }
        for id in queued {
            self.inputs.remove(&id);
        }
        let cancelled_start = self.cancel_start().await;
        let Some(runtime) = &self.runtime else {
            self.publish()?;
            return Ok(if had_queued || cancelled_start {
                CommandReceipt::Applied
            } else {
                CommandReceipt::Unchanged
            });
        };
        let outcome = runtime.agent.stop().await.map_err(agent_error)?;
        self.refresh_runtime().await?;
        self.publish()?;
        Ok(if had_queued || cancelled_start {
            CommandReceipt::Applied
        } else {
            receipt(outcome)
        })
    }

    async fn resolve_write(
        &mut self,
        target: CallRef,
        resolution: WriteResolution,
    ) -> Result<CommandReceipt> {
        if resolution.external_effect == bone_core::ExternalEffect::Unknown {
            return Err(Error::InvalidState(
                "a write resolution must decide whether the effect occurred".into(),
            ));
        }
        let write_guard = self
            .write_gate
            .lock
            .try_lock()
            .map_err(|_| Error::WriteInProgress)?;
        let result = self.store.resolve_write(
            self.info.workspace,
            self.info.id,
            target,
            resolution.clone(),
        )?;
        let (external_effect, receipt) = match result {
            ResolveWriteResult::Unchanged => return Ok(CommandReceipt::Unchanged),
            ResolveWriteResult::Conflicts(actual) => {
                return Err(Error::InvalidState(format!(
                    "the saved write effect is {actual:?}"
                )));
            }
            ResolveWriteResult::Applied => (resolution.external_effect, CommandReceipt::Applied),
            ResolveWriteResult::AlreadyResolved(saved) => (saved, CommandReceipt::Unchanged),
        };
        drop(write_guard);
        if let Some(runtime) = &self.runtime
            && self
                .store
                .core_call_origin(self.info.id, bone_core::CallId(target.id))?
                == Some(target)
        {
            runtime
                .agent
                .resolve_write(
                    bone_core::CallId(target.id),
                    host_resolution_outcome(external_effect),
                )
                .await
                .map_err(agent_error)?;
            self.refresh_runtime().await?;
        }
        self.publish()?;
        Ok(receipt)
    }

    fn rename(&mut self, title: String) -> Result<()> {
        if title.trim().is_empty() || title.trim() != title || title.len() > 200 {
            return Err(Error::InvalidState("invalid session title".into()));
        }
        self.info = self.store.rename_session(self.info.id, title)?;
        self.publish()
    }

    fn rename_if_provisional(&mut self, title: String) -> Result<bool> {
        if title.trim().is_empty() || title.trim() != title || title.len() > 200 {
            return Err(Error::InvalidState("invalid session title".into()));
        }
        let Some(info) = self
            .store
            .rename_session_if_provisional(self.info.id, title)?
        else {
            return Ok(false);
        };
        self.info = info;
        self.publish()?;
        Ok(true)
    }

    fn archive(&mut self, archived: bool) -> Result<()> {
        self.info = self.store.archive_session(self.info.id, archived)?;
        self.publish()
    }

    async fn deliver_queued(&mut self) -> Result<()> {
        if self
            .inputs
            .values()
            .all(|input| !matches!(input.state, InputState::Queued { .. }))
        {
            return Ok(());
        }
        if self.execution_blocked {
            return Ok(());
        }
        if self.config_blocked {
            if let Some(AppProblem::Configuration(problem)) = &self.problem {
                self.set_queued_problem(Some(problem.clone()))?;
                self.publish()?;
            }
            return Ok(());
        }
        if let Err(error) = self.ensure_runtime().await {
            if let Error::Configuration(problem) = &error {
                self.set_queued_problem(Some(problem.clone()))?;
                self.publish()?;
            }
            return Err(error);
        }
        if self.runtime.is_none() {
            return Ok(());
        }
        let queued = self
            .inputs
            .values()
            .filter(|input| matches!(input.state, InputState::Queued { .. }))
            .map(|input| input.id)
            .collect::<Vec<_>>();
        for id in queued {
            if !self.deliver(id).await? {
                break;
            }
        }
        self.publish()
    }

    async fn deliver(&mut self, id: InputId) -> Result<bool> {
        let input = self.inputs[&id].clone();
        let runtime = self.runtime.as_ref().expect("runtime ensured");
        if input
            .reply_to
            .is_some_and(|question| question.runtime != runtime.id)
        {
            self.reject_input(id, "the referenced question belongs to an old runtime")?;
            return Ok(true);
        }
        let runtime_id = runtime.id;
        let agent = runtime.agent.clone();
        let (posting, _) = self.store.update_input(
            self.info.id,
            id,
            InputState::Posting {
                runtime: runtime_id,
            },
            None,
        )?;
        self.inputs.insert(id, posting);

        let mut agent_input = AgentInput::new(AgentInputId(id.0), input.text);
        if let Some(question) = input.reply_to {
            agent_input =
                agent_input.answering(AgentInputId(question.reply_to.0), Seq(question.record));
        }
        match agent.post(agent_input).await {
            Ok(_) => {
                let (accepted, _) = self.store.update_input(
                    self.info.id,
                    id,
                    InputState::Accepted {
                        runtime: runtime_id,
                    },
                    Some(SessionEvent::InputAccepted {
                        input: id,
                        runtime: runtime_id,
                    }),
                )?;
                self.inputs.insert(id, accepted);
                self.refresh_runtime().await?;
                Ok(true)
            }
            Err(AgentError::Admission(bone_core::AdmissionError::Busy)) => {
                let (queued, _) = self.store.update_input(
                    self.info.id,
                    id,
                    InputState::Queued { problem: None },
                    None,
                )?;
                self.inputs.insert(id, queued);
                Ok(false)
            }
            Err(AgentError::Admission(error)) => {
                self.reject_input(id, &error.to_string())?;
                Ok(true)
            }
            Err(error) => {
                let (interrupted, _) = self.store.update_input(
                    self.info.id,
                    id,
                    InputState::Interrupted {
                        runtime: runtime_id,
                    },
                    Some(SessionEvent::Interrupted {
                        runtime: runtime_id,
                        inputs: vec![id],
                    }),
                )?;
                self.inputs.remove(&interrupted.id);
                Err(agent_error(error))
            }
        }
    }

    fn reject_input(&mut self, id: InputId, message: &str) -> Result<()> {
        let message = message.to_owned();
        let (rejected, _) = self.store.update_input(
            self.info.id,
            id,
            InputState::Rejected {
                message: message.clone(),
            },
            Some(SessionEvent::InputRejected { input: id, message }),
        )?;
        self.inputs.remove(&rejected.id);
        Ok(())
    }

    fn set_queued_problem(&mut self, problem: Option<ConfigProblem>) -> Result<()> {
        let queued = self
            .inputs
            .values()
            .filter(|input| matches!(input.state, InputState::Queued { .. }))
            .map(|input| input.id)
            .collect::<Vec<_>>();
        for id in queued {
            if matches!(
                &self.inputs[&id].state,
                InputState::Queued { problem: current } if current == &problem
            ) {
                continue;
            }
            let (input, _) = self.store.update_input(
                self.info.id,
                id,
                InputState::Queued {
                    problem: problem.clone(),
                },
                None,
            )?;
            self.inputs.insert(id, input);
        }
        Ok(())
    }

    async fn apply_config(&mut self, force: bool) -> Result<()> {
        let config = match self.resolve_config() {
            Ok(config) => config,
            Err(Error::Configuration(problem)) => {
                let _ = self.cancel_start().await;
                self.suspend_for_config().await?;
                self.set_queued_problem(Some(problem.clone()))?;
                return if self.runtime.is_some() {
                    Err(Error::Configuration(problem))
                } else {
                    self.problem = Some(AppProblem::Configuration(problem));
                    self.publish()
                };
            }
            Err(error) => {
                let _ = self.cancel_start().await;
                self.suspend_for_config().await?;
                return Err(error);
            }
        };

        if let Some(runtime) = &self.runtime {
            let current = match &self.runtime_state {
                RuntimeState::Running { config, .. } => config.as_ref(),
                _ => return Err(Error::InvalidState("running runtime has no config".into())),
            };
            if current == &config && !self.config_blocked && !force {
                self.set_queued_problem(None)?;
                return self.publish();
            }

            let id = runtime.id;
            let agent = runtime.agent.clone();
            self.suspend_for_config().await?;
            self.set_queued_problem(None)?;
            let ports = self.runtime_tools(&config, id)?;
            let model = self.backend.connect(&config).await?;
            agent
                .reconfigure(model, ports, config.limits.clone())
                .await
                .map_err(reconfigure_error)?;
            self.store
                .reconfigure_runtime(self.info.id, id, config.clone())?;
            self.runtime_state = RuntimeState::Running {
                id,
                config: Box::new(config),
            };
            self.refresh_runtime().await?;
            agent.resume_scheduling().await.map_err(agent_error)?;
            self.config_blocked = false;
            self.problem = None;
            self.deliver_queued().await?;
            return self.publish();
        }

        if self.starting.is_some() {
            self.cancel_start().await;
        }
        self.config_blocked = false;
        self.problem = None;
        self.set_queued_problem(None)?;
        self.deliver_queued().await?;
        self.publish()
    }

    async fn suspend_for_config(&mut self) -> Result<()> {
        self.config_blocked = true;
        if let Some(runtime) = &self.runtime {
            runtime.agent.suspend().await.map_err(agent_error)?;
        }
        Ok(())
    }

    async fn ensure_runtime(&mut self) -> Result<()> {
        if self.runtime.is_some() || self.starting.is_some() {
            return Ok(());
        }
        let config = self.resolve_config()?;
        let runtime_id = RuntimeId::new();
        let ports = self.runtime_tools(&config, runtime_id)?;
        let commit_guard = self
            .durable_gate
            .try_lock()
            .map_err(|_| Error::Agent("previous durable commit is still pending".into()))?;
        let restore = self.store.load_core_restore(self.info.id)?;
        let archive_from = self.store.core_projection_through(self.info.id)?;
        let durable = self.store.core_durable_port(
            self.info.id,
            runtime_id,
            Arc::clone(self.lease.as_ref().expect("live session owns its lease")),
            Arc::clone(&self.durable_gate),
        );
        drop(commit_guard);
        self.runtime_state = RuntimeState::Starting;
        self.problem = None;

        let backend = self.backend.clone();
        let ready_commands = self.command_tx.clone();
        let ready_config = config.clone();
        let (ready_tx, ready) = oneshot::channel();
        let task = tokio::spawn(async move {
            let result = async {
                let model = backend.connect(&ready_config).await?;
                let agent = Agent::with_durable_ports(
                    model,
                    ports,
                    ready_config.limits.clone(),
                    durable,
                    restore,
                )
                .await
                .map_err(|error| Error::Agent(error.to_string()))?;
                let observation = agent.observe().await.map_err(agent_error)?;
                Ok((agent, observation))
            }
            .await;
            if ready_tx
                .send(RuntimeReady {
                    id: runtime_id,
                    config: ready_config,
                    archive_from,
                    result,
                })
                .is_err()
            {
                return;
            }
            if let Some(commands) = ready_commands.upgrade() {
                let _ = commands.send(Command::RuntimeReady(runtime_id)).await;
            }
        });
        self.starting = Some(StartingRuntime {
            id: runtime_id,
            task,
            ready,
        });
        self.publish()
    }

    fn runtime_tools(
        &self,
        config: &RuntimeConfig,
        runtime: RuntimeId,
    ) -> Result<Vec<Arc<dyn bone_core::ToolPort>>> {
        let commands = self.command_tx.clone();
        let notify = Arc::new(move |call| {
            if let Some(commands) = commands.upgrade() {
                tokio::spawn(async move {
                    let _ = commands.send(Command::WriteCompleted(call)).await;
                });
            }
        });
        self.backend.tools(
            config,
            tools::ToolContext {
                store: self.store.clone(),
                workspace: self.info.workspace,
                session: self.info.id,
                runtime,
                write_gate: Arc::clone(&self.write_gate),
                lease: Arc::clone(self.lease.as_ref().expect("live session owns its lease")),
                notify,
            },
        )
    }

    async fn runtime_ready(&mut self, runtime: RuntimeId) {
        if self.starting.as_ref().map(|starting| starting.id) != Some(runtime) {
            return;
        }
        let starting = self.starting.take().expect("starting runtime checked");
        let _ = starting.task.await;
        let Ok(ready) = starting.ready.await else {
            self.runtime_state = RuntimeState::Detached;
            let error = Error::Agent("runtime startup was interrupted".into());
            self.record_execution_error(&error).await;
            return;
        };
        match ready.result {
            Ok((agent, observation)) => {
                let agent_through = ready.archive_from;
                let saved_runtime = SavedRuntime {
                    id: ready.id,
                    config: ready.config.clone(),
                };
                if let Err(error) =
                    self.store
                        .start_runtime(self.info.id, saved_runtime.clone(), agent_through)
                {
                    let _ = agent.shutdown().await;
                    self.runtime_state = RuntimeState::Detached;
                    self.record_execution_error(&error.into()).await;
                    return;
                }
                self.runtime = Some(RunningRuntime {
                    id: ready.id,
                    agent: agent.clone(),
                    agent_through,
                    view: observation.baseline,
                    dirty: Arc::new(AtomicBool::new(false)),
                });
                self.shutdown_target.send_replace(Some(agent));
                self.runtime_state = RuntimeState::Running {
                    id: ready.id,
                    config: Box::new(ready.config),
                };
                let dirty =
                    Arc::clone(&self.runtime.as_ref().expect("runtime was installed").dirty);
                observe_runtime(
                    ready.id,
                    observation.records,
                    self.command_tx.clone(),
                    dirty,
                );
                if let Err(error) = self.refresh_runtime().await {
                    self.record_execution_error(&error).await;
                    return;
                }
                if let Err(error) = self.deliver_queued().await {
                    self.record_execution_error(&error).await;
                }
            }
            Err(error) => {
                self.runtime_state = RuntimeState::Detached;
                self.record_execution_error(&error).await;
            }
        }
        if let Err(error) = self.publish() {
            self.record_execution_error(&error).await;
        }
    }

    async fn cancel_start(&mut self) -> bool {
        let Some(starting) = self.starting.take() else {
            return false;
        };
        starting.task.abort();
        let _ = starting.task.await;
        if let Ok(ready) = starting.ready.await {
            shutdown_ready(ready).await;
        }
        self.runtime_state = RuntimeState::Detached;
        true
    }

    fn resolve_config(&self) -> Result<RuntimeConfig> {
        let workspace = self
            .store
            .workspace_by_id(self.info.workspace)?
            .ok_or(Error::WorkspaceNotFound)?;
        let global = self.store.global_settings()?;
        let workspace_config = self.store.config(ConfigScope::Workspace(workspace.id))?;
        let session_config = self.store.config(ConfigScope::Session(self.info.id))?;
        resolve_runtime(
            &global,
            Some(&workspace_config),
            Some(&session_config),
            &self.store.profiles()?,
            workspace.root,
        )
        .map_err(Error::Configuration)
    }

    async fn sync_runtime(&mut self) -> Result<()> {
        let agent = match &self.runtime {
            Some(runtime) => runtime.agent.clone(),
            None => return Ok(()),
        };
        let snapshot = agent.observe().await.map_err(agent_error)?.baseline;
        self.archive_snapshot(snapshot, true).await
    }

    async fn archive_snapshot(
        &mut self,
        snapshot: Arc<AgentView>,
        reconcile_writes: bool,
    ) -> Result<()> {
        let Some(runtime) = &self.runtime else {
            return Ok(());
        };
        let runtime_id = runtime.id;
        let through = runtime.agent_through;
        let records = snapshot
            .records
            .iter()
            .filter(|record| record.seq.0 > through)
            .cloned()
            .collect::<Vec<_>>();
        for record in records {
            let record_runtime = self
                .store
                .core_record_origin(self.info.id, record.seq.0)?
                .ok_or(Error::Agent("Core record has no runtime provenance".into()))?;
            let changes = input_changes(record_runtime, &record, &self.inputs);
            self.store
                .save_agent_record(self.info.id, record_runtime, &record, &changes)?;
            for (id, state) in changes {
                if state.terminal() {
                    self.inputs.remove(&id);
                } else if let Some(input) = self.inputs.get_mut(&id) {
                    input.state = state;
                }
            }
            if let Some((call, job)) = write_job(runtime_id, &record)
                && let Some(origin) = self
                    .store
                    .core_call_origin(self.info.id, bone_core::CallId(call.id))?
            {
                self.store.attach_write_job(
                    self.info.workspace,
                    origin,
                    JobRef {
                        runtime: origin.runtime,
                        id: job.id,
                    },
                )?;
            }
            if let Some(runtime) = &mut self.runtime
                && runtime.id == runtime_id
            {
                runtime.agent_through = record.seq.0;
            }
        }
        if let Some(runtime) = &mut self.runtime
            && runtime.id == runtime_id
        {
            runtime.view = Arc::clone(&snapshot);
        }
        if reconcile_writes {
            for call in &snapshot.calls {
                if matches!(
                    call.status,
                    CallStatus::Finished {
                        external_effect: ExternalEffect::Unknown,
                        ..
                    }
                ) && let Some(origin) = self.store.core_call_origin(self.info.id, call.id)?
                {
                    self.resolve_known_write(origin).await?;
                }
            }
        }
        Ok(())
    }

    async fn resolve_known_write(&self, call: CallRef) -> Result<()> {
        let Some(runtime) = self.runtime.as_ref() else {
            return Ok(());
        };
        if self
            .store
            .core_call_origin(self.info.id, bone_core::CallId(call.id))?
            != Some(call)
        {
            return Ok(());
        }
        let outcome = match self.store.finished_write(self.info.workspace, call)? {
            Some(outcome) => Some(outcome),
            None => self
                .store
                .resolved_write_effect(self.info.workspace, self.info.id, call)?
                .map(host_resolution_outcome),
        };
        let Some(outcome) = outcome else {
            return Ok(());
        };
        let view = runtime.agent.observe().await.map_err(agent_error)?.baseline;
        let unresolved = view.calls.iter().any(|candidate| {
            candidate.id.0 == call.id
                && matches!(
                    candidate.status,
                    CallStatus::Finished {
                        external_effect: ExternalEffect::Unknown,
                        ..
                    }
                )
        });
        if unresolved {
            runtime
                .agent
                .resolve_write(bone_core::CallId(call.id), outcome)
                .await
                .map_err(agent_error)?;
        }
        Ok(())
    }

    async fn close_runtime(&mut self) -> Result<CloseReport> {
        let _ = self.cancel_start().await;
        let Some(runtime) = &self.runtime else {
            let guard = self.durable_gate.try_lock().map_err(|_| {
                Error::Agent(
                    "startup durable commit is still pending; close is not confirmed".into(),
                )
            })?;
            let (_, inputs) = self.store.recover_session(self.info.id)?;
            self.inputs = inputs
                .into_iter()
                .filter(|input| !input.state.terminal())
                .map(|input| (input.id, input))
                .collect();
            drop(guard);
            self.runtime_state = RuntimeState::Detached;
            self.config_blocked = false;
            self.execution_blocked = false;
            self.problem = None;
            self.publish()?;
            return Ok(CloseReport {
                unresolved_writes: tools::unresolved_after_write_gate(
                    &self.store,
                    &self.write_gate,
                    self.info.workspace,
                    Some(self.info.id),
                )
                .await?,
            });
        };
        let id = runtime.id;
        let agent = runtime.agent.clone();
        self.runtime_state = RuntimeState::Closing { id };
        self.publish()?;
        let report = agent.shutdown().await.map_err(agent_error)?;
        if let Some(commit) = &report.pending_commit {
            let guard = self.durable_gate.try_lock().map_err(|_| {
                Error::Agent(format!(
                    "durable commit {commit} is still pending; close is not confirmed"
                ))
            })?;
            let (_, inputs) = self.store.recover_session(self.info.id)?;
            self.inputs = inputs
                .into_iter()
                .filter(|input| !input.state.terminal())
                .map(|input| (input.id, input))
                .collect();
            self.runtime = None;
            self.shutdown_target.send_replace(None);
            self.runtime_state = RuntimeState::Detached;
            self.config_blocked = false;
            self.execution_blocked = false;
            self.problem = None;
            drop(guard);
            self.publish()?;
            return Ok(CloseReport {
                unresolved_writes: tools::unresolved_after_write_gate(
                    &self.store,
                    &self.write_gate,
                    self.info.workspace,
                    Some(self.info.id),
                )
                .await?,
            });
        }
        self.archive_snapshot(report.final_view, false).await?;
        for write in report.unresolved_writes {
            self.store.attach_write_job(
                self.info.workspace,
                CallRef {
                    runtime: id,
                    id: write.call.0,
                },
                JobRef {
                    runtime: id,
                    id: write.job.0,
                },
            )?;
        }
        self.store.close_runtime(self.info.id, id)?;
        self.runtime = None;
        self.shutdown_target.send_replace(None);
        self.runtime_state = RuntimeState::Detached;
        self.config_blocked = false;
        self.execution_blocked = false;
        self.problem = None;
        self.publish()?;
        Ok(CloseReport {
            unresolved_writes: tools::unresolved_after_write_gate(
                &self.store,
                &self.write_gate,
                self.info.workspace,
                Some(self.info.id),
            )
            .await?,
        })
    }

    fn current_runtime(&self, id: RuntimeId) -> Result<&RunningRuntime> {
        self.runtime
            .as_ref()
            .filter(|runtime| runtime.id == id)
            .ok_or(Error::StaleRuntime(id))
    }

    async fn refresh_runtime(&mut self) -> Result<()> {
        self.sync_runtime().await?;
        if self.execution_blocked {
            self.execution_blocked = false;
            if !self.config_blocked {
                self.problem = None;
            }
        }
        Ok(())
    }

    async fn record_execution_error(&mut self, error: &Error) {
        let Some(problem) = app_problem(error) else {
            return;
        };
        self.problem = Some(problem);
        if !self.config_blocked
            && matches!(error, Error::Storage(_) | Error::Agent(_))
            && !self.execution_blocked
            && let Some(runtime) = &self.runtime
        {
            self.execution_blocked = true;
            let _ = runtime.agent.stop().await;
        }
        self.publish_cached();
    }

    fn publish(&mut self) -> Result<()> {
        let history_through = self.store.history_through(self.info.id)?;
        self.publish_state(history_through);
        Ok(())
    }

    fn publish_cached(&mut self) {
        let history_through = self.view.borrow().history_through;
        self.publish_state(history_through);
    }

    fn publish_state(&mut self, history_through: SessionSeq) {
        let (jobs, activity) = self
            .runtime
            .as_ref()
            .map(|runtime| project_runtime(runtime.id, &runtime.view))
            .unwrap_or_default();
        let view = SessionView {
            session: self.info.clone(),
            runtime: self.runtime_state.clone(),
            draft: self.draft.clone(),
            inputs: self.inputs.values().cloned().collect(),
            jobs,
            activity,
            history_through,
            problem: self.problem.clone(),
        };
        self.view.send_replace(Arc::new(view));
    }
}

pub(crate) fn host_resolution_outcome(external_effect: ExternalEffect) -> ToolOutcome {
    ToolOutcome {
        result: Err(CallError::failed(
            "write effect resolved by host; original tool result unavailable",
        )),
        external_effect,
    }
}

async fn shutdown_ready(ready: RuntimeReady) {
    if let Ok((agent, _)) = ready.result {
        let _ = agent.shutdown().await;
    }
}

fn observe_runtime(
    runtime: RuntimeId,
    mut records: broadcast::Receiver<Arc<Record>>,
    commands: mpsc::WeakSender<Command>,
    dirty: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        loop {
            match records.recv().await {
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {
                    if dirty.swap(true, Ordering::AcqRel) {
                        continue;
                    }
                    let Some(commands) = commands.upgrade() else {
                        return;
                    };
                    if commands.send(Command::RuntimeDirty(runtime)).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    });
}

pub(crate) fn input_changes(
    runtime: RuntimeId,
    record: &Record,
    inputs: &BTreeMap<InputId, InputView>,
) -> Vec<(InputId, InputState)> {
    match &record.body {
        RecordBody::Input(answer) => {
            let (Some(reply_to), Some(expected)) = (answer.reply_to, answer.expected_question)
            else {
                return Vec::new();
            };
            match inputs.get(&InputId(reply_to.0)).map(|input| &input.state) {
                Some(InputState::WaitingForUser {
                    runtime: owner,
                    question,
                    ..
                }) if *owner == runtime && question.record == expected.0 => inputs
                    .iter()
                    .filter_map(|(id, input)| match &input.state {
                        InputState::WaitingForUser {
                            runtime: owner,
                            question,
                            ..
                        } if *owner == runtime && question.record == expected.0 => {
                            Some((*id, InputState::Accepted { runtime }))
                        }
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            }
        }
        RecordBody::RoutingStarted { inputs, .. } => inputs
            .iter()
            .map(|input| (InputId(input.0), InputState::Accepted { runtime }))
            .collect(),
        RecordBody::Clarification { inputs, question } => inputs
            .iter()
            .map(|input| {
                let id = InputId(input.0);
                (
                    id,
                    InputState::WaitingForUser {
                        runtime,
                        question: crate::QuestionId {
                            runtime,
                            record: record.seq.0,
                            reply_to: id,
                        },
                        text: question.clone(),
                    },
                )
            })
            .collect(),
        RecordBody::InputRoutingFailed { inputs, message } => inputs
            .iter()
            .map(|input| {
                (
                    InputId(input.0),
                    InputState::RoutingFailed {
                        runtime,
                        message: message.clone(),
                    },
                )
            })
            .collect(),
        RecordBody::InputFinished { input, outcome } => vec![(
            InputId(input.0),
            InputState::Finished {
                runtime,
                outcome: outcome.clone(),
            },
        )],
        _ => Vec::new(),
    }
}

pub(crate) fn restored_routing_inputs(
    runtime: RuntimeId,
    inputs: &BTreeMap<InputId, InputView>,
    agent: &AgentView,
) -> Vec<InputId> {
    agent
        .inputs
        .iter()
        .filter_map(|input| {
            let id = InputId(input.input.id.0);
            (matches!(
                inputs.get(&id).map(|input| &input.state),
                Some(InputState::RoutingFailed { runtime: owner, .. }) if *owner == runtime
            ) && matches!(
                input.status,
                AgentInputStatus::Routing | AgentInputStatus::Handled
            ))
            .then_some(id)
        })
        .collect()
}

fn app_problem(error: &Error) -> Option<AppProblem> {
    match error {
        Error::Configuration(problem) => Some(AppProblem::Configuration(problem.clone())),
        Error::LoginRequired(profile) => Some(AppProblem::LoginRequired(profile.clone())),
        Error::ProfileBusy(profile) => Some(AppProblem::ProfileBusy(profile.clone())),
        Error::Provider(message) => Some(AppProblem::Provider(message.clone())),
        Error::Storage(message) => Some(AppProblem::Storage(message.clone())),
        Error::Tools(message) => Some(AppProblem::Tools(message.clone())),
        Error::Agent(message) => Some(AppProblem::Agent(message.clone())),
        _ => None,
    }
}

fn write_job(runtime: RuntimeId, record: &Record) -> Option<(CallRef, JobRef)> {
    match &record.body {
        RecordBody::ToolFinished { call, job, .. } => Some((
            CallRef {
                runtime,
                id: call.0,
            },
            JobRef { runtime, id: job.0 },
        )),
        _ => None,
    }
}

fn project_runtime(runtime: RuntimeId, view: &AgentView) -> (Vec<JobView>, Vec<ActivityView>) {
    let jobs = view
        .jobs
        .iter()
        .map(|job| JobView {
            id: JobRef {
                runtime,
                id: job.id.0,
            },
            owner: match job.owner {
                Owner::User => JobOwner::User,
                Owner::Job(owner) => JobOwner::Job(JobRef {
                    runtime,
                    id: owner.0,
                }),
                Owner::Routing(_) => JobOwner::Routing,
            },
            inputs: job.inputs.iter().map(|input| InputId(input.0)).collect(),
            goal: job.spec.goal.clone(),
            scope: job.spec.scope.clone(),
            done_when: job.spec.done_when.clone(),
            state: match &job.status {
                JobStatus::Ready => JobState::Ready,
                JobStatus::Running => JobState::Running,
                JobStatus::Waiting(wait) => JobState::Waiting(wait_reason(runtime, wait)),
                JobStatus::Paused => JobState::Paused,
                JobStatus::Finished(outcome) => JobState::Finished {
                    outcome: outcome.kind,
                    summary: outcome.completion.summary.clone(),
                },
            },
            report: job.report.and_then(|report| {
                view.records
                    .iter()
                    .find(|record| record.seq == report)
                    .and_then(|record| match &record.body {
                        RecordBody::Report { report, .. } => Some(JobReport {
                            summary: report.summary.clone(),
                        }),
                        _ => None,
                    })
            }),
        })
        .collect();
    let activity = view
        .calls
        .iter()
        .filter(|call| !matches!(call.status, CallStatus::Finished { .. }))
        .map(|call| ActivityView {
            call: CallRef {
                runtime,
                id: call.id.0,
            },
            job: call.job.map(|job| JobRef { runtime, id: job.0 }),
            kind: match call.kind {
                CallKind::Coordinate => ActivityKind::Coordinate,
                CallKind::Work => ActivityKind::Work,
                CallKind::Compact => ActivityKind::Compact,
                CallKind::Tool => ActivityKind::Tool {
                    name: call
                        .tool
                        .as_ref()
                        .map_or_else(|| "tool".into(), |tool| tool.name.clone()),
                },
            },
            progress: call
                .progress
                .as_ref()
                .map(|progress| progress.message.clone()),
        })
        .collect();
    (jobs, activity)
}

fn wait_reason(runtime: RuntimeId, wait: &WaitView) -> WaitReason {
    match wait {
        WaitView::Tool(call) => WaitReason::Tool(CallRef {
            runtime,
            id: call.0,
        }),
        WaitView::Until(_) => WaitReason::Timer,
        WaitView::Jobs(jobs) => WaitReason::Jobs(
            jobs.iter()
                .map(|job| JobRef { runtime, id: job.0 })
                .collect(),
        ),
        WaitView::User { .. } => WaitReason::User,
        WaitView::Job { job, .. } => WaitReason::Job(JobRef { runtime, id: job.0 }),
        WaitView::Result { job, .. } => WaitReason::Result(JobRef { runtime, id: job.0 }),
        WaitView::Inquiry(_) => WaitReason::Inquiry,
        WaitView::Coordination(_) => WaitReason::Coordination,
        WaitView::Commit => WaitReason::Commit,
    }
}

fn receipt(outcome: ControlOutcome) -> CommandReceipt {
    match outcome {
        ControlOutcome::Applied => CommandReceipt::Applied,
        ControlOutcome::Unchanged => CommandReceipt::Unchanged,
    }
}

fn map_accept_error(error: AcceptError) -> Error {
    match error {
        AcceptError::NotFound => Error::SessionNotFound,
        AcceptError::Conflict => Error::RequestConflict,
        AcceptError::Store(error) => error.into(),
    }
}

fn agent_error(error: AgentError) -> Error {
    Error::Agent(error.to_string())
}

fn reconfigure_error(error: AgentError) -> Error {
    match error {
        AgentError::InvalidConfiguration(message) => {
            Error::Configuration(ConfigProblem::Invalid(message))
        }
        error => agent_error(error),
    }
}

#[cfg(test)]
mod shutdown_forwarding_tests {
    use super::*;
    use bone_core::{
        AgentLimits, CallContext, CheckpointDraft, CompactInput, CoordinateInput, KernelDecision,
        ModelPort, PortFuture, WorkInput, WorkProposal,
    };
    use std::{future::pending, time::Duration};

    struct PendingModel(mpsc::UnboundedSender<CallContext>);

    impl ModelPort for PendingModel {
        fn coordinate(
            &self,
            _: CoordinateInput,
            context: CallContext,
        ) -> PortFuture<std::result::Result<KernelDecision, CallError>> {
            self.0.send(context).unwrap();
            Box::pin(pending())
        }

        fn work(
            &self,
            _: WorkInput,
            _: CallContext,
        ) -> PortFuture<std::result::Result<WorkProposal, CallError>> {
            Box::pin(pending())
        }

        fn compact(
            &self,
            _: CompactInput,
            _: CallContext,
        ) -> PortFuture<std::result::Result<CheckpointDraft, CallError>> {
            Box::pin(pending())
        }
    }

    #[tokio::test]
    async fn close_forwards_to_runtime_installed_after_request_and_releases_observer() {
        let (targets, target) = watch::channel(None);
        let (queued, queued_rx) = oneshot::channel();
        let (reply, reply_rx) = oneshot::channel();
        let request = tokio::spawn(with_shutdown_forwarding(target, async move {
            let _ = queued.send(());
            reply_rx.await.map_err(|_| Error::Closed)
        }));
        queued_rx.await.unwrap();
        assert!(targets.borrow().is_none());

        let (contexts, mut received) = mpsc::unbounded_channel();
        let agent = Agent::with_ports(
            Arc::new(PendingModel(contexts)),
            vec![],
            AgentLimits::default(),
        )
        .unwrap();
        agent
            .post(AgentInput::new(AgentInputId(1), "work"))
            .await
            .unwrap();
        let mut context = received.recv().await.unwrap();
        targets.send_replace(Some(agent));
        tokio::time::timeout(Duration::from_secs(1), context.wait_for_cancellation())
            .await
            .expect("close must reach a runtime installed after it was queued");
        assert!(!request.is_finished(), "App cleanup still owns the reply");
        reply.send(()).unwrap();
        request.await.unwrap().unwrap();
        assert_eq!(
            targets.receiver_count(),
            0,
            "forwarder must not outlive request"
        );
    }

    #[tokio::test]
    async fn completed_close_without_runtime_releases_observer() {
        let (targets, target) = watch::channel(None);
        with_shutdown_forwarding(target, async { Ok(()) })
            .await
            .unwrap();
        assert_eq!(targets.receiver_count(), 0);
    }
}
