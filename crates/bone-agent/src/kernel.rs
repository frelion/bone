use crate::*;
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct KernelConfig {
    pub kernel_timeout: Duration,
    pub work_timeout: Duration,
    pub background_concurrency: usize,
    pub input_capacity: usize,
    pub tool_concurrency: usize,
}
impl Default for KernelConfig {
    fn default() -> Self {
        Self {
            kernel_timeout: Duration::from_secs(30),
            work_timeout: Duration::from_secs(120),
            background_concurrency: 2,
            input_capacity: 32,
            tool_concurrency: 8,
        }
    }
}
#[derive(Debug, thiserror::Error)]
pub enum KernelError {
    #[error("{0} must be greater than zero")]
    ZeroDuration(&'static str),
    #[error("{0} must be greater than zero")]
    ZeroCapacity(&'static str),
    #[error("invalid tool specification: {0}")]
    InvalidTool(String),
    #[error("duplicate tool name: {0}")]
    DuplicateTool(String),
}

struct Job {
    view: JobSnapshot,
    interactive: bool,
    investigation: bool,
    candidate: Option<(CallId, WorkProposal)>,
}
#[derive(Clone)]
struct Routing {
    inputs: Vec<InputId>,
    source: Option<JobId>,
    source_version: u64,
    request: Option<String>,
}

/// The only state owner. No I/O, wall clock, or model inference lives here.
pub struct Kernel {
    config: KernelConfig,
    tools: BTreeMap<String, ToolSpec>,
    inputs: BTreeMap<InputId, InputSnapshot>,
    jobs: BTreeMap<JobId, Job>,
    calls: BTreeMap<CallId, CallSnapshot>,
    ready: VecDeque<JobId>,
    last_root: Option<JobId>,
    candidates: VecDeque<JobId>,
    coordination: VecDeque<(JobId, u64, String)>,
    investigations: BTreeMap<JobId, Routing>,
    wakes: BTreeMap<WakeId, (JobId, u64)>,
    record: Vec<RecordEntry>,
    constraints: String,
    generation: u64,
    draining: bool,
    next_job: u64,
    next_call: u64,
    next_wake: u64,
}

impl Kernel {
    pub fn new(config: KernelConfig, tools: Vec<ToolSpec>) -> Result<Self, KernelError> {
        for (name, value) in [
            ("kernel_timeout", config.kernel_timeout),
            ("work_timeout", config.work_timeout),
        ] {
            if value.is_zero() {
                return Err(KernelError::ZeroDuration(name));
            }
        }
        for (name, value) in [
            ("background_concurrency", config.background_concurrency),
            ("input_capacity", config.input_capacity),
            ("tool_concurrency", config.tool_concurrency),
        ] {
            if value == 0 {
                return Err(KernelError::ZeroCapacity(name));
            }
        }
        let mut registry = BTreeMap::new();
        for tool in tools {
            if tool.name.trim().is_empty() || !tool.parameters.is_object() {
                return Err(KernelError::InvalidTool(tool.name));
            }
            let name = tool.name.clone();
            if registry.insert(name.clone(), tool).is_some() {
                return Err(KernelError::DuplicateTool(name));
            }
        }
        Ok(Self {
            config,
            tools: registry,
            inputs: BTreeMap::new(),
            jobs: BTreeMap::new(),
            calls: BTreeMap::new(),
            ready: VecDeque::new(),
            last_root: None,
            candidates: VecDeque::new(),
            coordination: VecDeque::new(),
            investigations: BTreeMap::new(),
            wakes: BTreeMap::new(),
            record: Vec::new(),
            constraints: String::new(),
            generation: 0,
            draining: false,
            next_job: 1,
            next_call: 1,
            next_wake: 1,
        })
    }

    pub fn record_cursor(&self) -> u64 {
        self.record.len() as u64
    }
    pub(crate) fn records_since(&self, cursor: u64) -> &[RecordEntry] {
        &self.record[cursor as usize..]
    }
    pub(crate) fn call(&self, id: CallId) -> &CallSnapshot {
        &self.calls[&id]
    }
    pub fn receipt(&self, id: InputId) -> Option<InputReceipt> {
        self.inputs.get(&id).map(|input| InputReceipt {
            id,
            record_cursor: input.received_at,
        })
    }
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            record_cursor: self.record_cursor(),
            generation: self.generation,
            constraints: self.constraints.clone(),
            inputs: self.inputs.values().cloned().collect(),
            jobs: self.jobs.values().map(|job| job.view.clone()).collect(),
            calls: self.calls.values().cloned().collect(),
            record: self.record.clone(),
            tools: self.tools.values().cloned().collect(),
        }
    }

    /// Check before accepting. A duplicate returns its original receipt even under load.
    pub fn admit(&self, input: &Input) -> Result<Option<InputReceipt>, AdmissionError> {
        if let Some(previous) = self.inputs.get(&input.id) {
            return if previous.input == *input {
                Ok(self.receipt(input.id))
            } else {
                Err(AdmissionError::ConflictingInput)
            };
        }
        let unresolved = self
            .inputs
            .values()
            .filter(|input| input.state.unresolved())
            .count();
        if let Some(target) = input.reply_to {
            if !self.accepts_reply(target) {
                return Err(AdmissionError::InvalidReply);
            }
            // One reserved clarification envelope can break a full queue's wait cycle.
            let clarification_pending = self.inputs.values().any(|input| {
                input.input.reply_to.is_some()
                    && matches!(input.state, InputState::Pending | InputState::Routing)
            });
            if clarification_pending || unresolved > self.config.input_capacity {
                return Err(AdmissionError::Busy);
            }
        } else if self.draining || unresolved >= self.config.input_capacity {
            return Err(AdmissionError::Busy);
        }
        Ok(None)
    }

    fn accepts_reply(&self, id: InputId) -> bool {
        self.inputs
            .get(&id)
            .is_some_and(|input| input.state.unresolved())
            || self.jobs.values().any(|job| {
                job.view.inputs.contains(&id)
                    && matches!(job.view.state, JobState::Waiting(WaitReason::User { .. }))
            })
    }
    pub fn validate_retry(&self, id: InputId) -> Result<(), String> {
        match self.inputs.get(&id).map(|input| &input.state) {
            Some(InputState::RoutingFailed { .. }) => Ok(()),
            _ => Err("only a failed input interpretation can be retried".into()),
        }
    }
    pub fn validate_resolution(&self, id: CallId, outcome: &CallOutcome) -> Result<bool, String> {
        let call = self.calls.get(&id).ok_or("unknown call")?;
        if !call.external_write || outcome.external_effect == ExternalEffect::Unknown {
            return Err("a write resolution must establish a known external outcome".into());
        }
        if matches!(
            outcome.result,
            Ok(CallOutput::Kernel(_) | CallOutput::Work(_))
        ) {
            return Err("a tool resolution cannot contain a model proposal".into());
        }
        match &call.state {
            CallState::Finished(previous) if previous == outcome => Ok(false),
            CallState::Finished(previous)
                if previous.external_effect == ExternalEffect::Unknown =>
            {
                Ok(true)
            }
            _ => Err("the call is not awaiting external reconciliation".into()),
        }
    }

    pub fn step(&mut self, event: Event) -> Vec<Effect> {
        let mut effects = Vec::new();
        match event {
            Event::Input(input) => match self.admit(&input) {
                Ok(None) => self.accept(input),
                Ok(Some(_)) => {}
                Err(error) => self.publish(
                    Notice::Error {
                        message: error.to_string(),
                    },
                    &mut effects,
                ),
            },
            Event::RetryInput { id } => {
                if self.validate_retry(id).is_ok() {
                    // A failed fixed batch is retried together, in its original order.
                    let siblings = self.failed_batch(id);
                    for input in siblings {
                        self.inputs.get_mut(&input).unwrap().state = InputState::Pending;
                    }
                }
            }
            Event::CallFinished { id, outcome } => {
                self.finish_call(id, outcome, false, &mut effects)
            }
            Event::WriteResolved { id, outcome } => {
                if self.validate_resolution(id, &outcome) == Ok(true) {
                    self.finish_call(id, outcome, true, &mut effects);
                }
            }
            Event::CallProgress { id, progress } => {
                if let Some(call) = self.calls.get_mut(&id)
                    && call.is_running()
                    && call.progress.as_ref() != Some(&progress)
                {
                    call.progress = Some(progress.clone());
                    let job_id = call.job;
                    if let Some(job) = job_id.and_then(|id| self.jobs.get_mut(&id))
                        && call.version == job.view.version
                        && call.generation == self.generation
                        && matches!(call.state, CallState::Running)
                        && !job.view.state.terminal()
                        && !matches!(job.view.state, JobState::Paused)
                    {
                        job.view.progress = Some(progress.clone());
                    }
                    self.publish(
                        Notice::CallProgress {
                            id,
                            job: job_id,
                            progress,
                        },
                        &mut effects,
                    );
                }
            }
            Event::Wake { id } => {
                if let Some((job, version)) = self.wakes.remove(&id)
                    && self.jobs.get(&job).is_some_and(|job| {
                        job.view.version == version
                            && matches!(job.view.state, JobState::Waiting(WaitReason::Timer))
                    })
                {
                    self.make_ready(job, false);
                }
            }
            Event::Stop => self.stop(&mut effects),
        }
        self.advance(&mut effects);
        effects
    }

    fn accept(&mut self, input: Input) {
        if let Some(target) = input.reply_to
            && self
                .inputs
                .get(&target)
                .is_some_and(|input| matches!(input.state, InputState::WaitingForUser { .. }))
        {
            let batch = self.failed_batch(target);
            for id in batch {
                self.inputs.get_mut(&id).unwrap().state = InputState::Pending;
            }
        }
        self.append(RecordKind::InputAccepted(input.clone()));
        self.inputs.insert(
            input.id,
            InputSnapshot {
                input,
                received_at: self.record_cursor(),
                state: InputState::Pending,
                required_jobs: Vec::new(),
            },
        );
    }

    // The last Kernel call containing an input is its fixed-batch identity.
    fn failed_batch(&self, id: InputId) -> Vec<InputId> {
        self.calls
            .values()
            .rev()
            .find_map(|call| match &call.request {
                CallRequest::Kernel { inputs, .. } if inputs.contains(&id) => Some(inputs.clone()),
                _ => None,
            })
            .unwrap_or_else(|| vec![id])
    }

    fn finish_call(
        &mut self,
        id: CallId,
        mut outcome: CallOutcome,
        resolved: bool,
        effects: &mut Vec<Effect>,
    ) {
        let Some(call) = self.calls.get(&id).cloned() else {
            return;
        };
        if !resolved && !call.is_running() {
            return;
        }
        if !call.external_write && outcome.external_effect != ExternalEffect::None {
            outcome = CallOutcome::failed("a read-only call reported an external effect");
        }
        let valid_kind = matches!(
            (&call.request, &outcome.result),
            (_, Err(_))
                | (CallRequest::Kernel { .. }, Ok(CallOutput::Kernel(_)))
                | (CallRequest::Work { .. }, Ok(CallOutput::Work(_)))
                | (CallRequest::Tool(_), Ok(CallOutput::Artifact(_)))
        );
        if !valid_kind {
            outcome.result = Err(CallError::new("call returned the wrong result kind"));
        }
        self.calls.get_mut(&id).unwrap().state = CallState::Finished(outcome.clone());
        self.publish(
            Notice::CallFinished {
                id,
                job: call.job,
                outcome: outcome.clone(),
            },
            effects,
        );
        match (&call.request, outcome.result) {
            (
                CallRequest::Kernel {
                    inputs,
                    source,
                    request,
                },
                result,
            ) => {
                let routing = Routing {
                    inputs: inputs.clone(),
                    source: *source,
                    source_version: call.version,
                    request: request.clone(),
                };
                if call.generation != self.generation || !self.routing_current(&routing, &call) {
                    return;
                }
                match result {
                    Ok(CallOutput::Kernel(decision)) => {
                        if let Err(message) =
                            self.validate_decision(&routing, &decision, call.as_of)
                        {
                            self.routing_failed(&routing, message, effects);
                        } else {
                            self.apply_decision(routing, decision, effects);
                        }
                    }
                    Err(error) => self.routing_failed(&routing, error.message, effects),
                    _ => self.routing_failed(
                        &routing,
                        "Kernel model returned the wrong result kind".into(),
                        effects,
                    ),
                }
            }
            (CallRequest::Work { job, .. }, result) => {
                let current = self.jobs.get(job).is_some_and(|job| {
                    job.view.active_call == Some(id)
                        && job.view.version == call.version
                        && call.generation == self.generation
                        && !job.view.state.terminal()
                        && !matches!(job.view.state, JobState::Paused)
                });
                if let Ok(CallOutput::Work(proposal)) = &result {
                    self.append(RecordKind::Material {
                        job: *job,
                        note: proposal.note.clone(),
                    });
                }
                if !current {
                    self.discard(id, "call no longer owns this job's execution", effects);
                    return;
                }
                self.jobs.get_mut(job).unwrap().view.active_call = None;
                match result {
                    Ok(CallOutput::Work(proposal)) => {
                        let entry = self.jobs.get_mut(job).unwrap();
                        entry.view.note = proposal.note.clone();
                        entry.candidate = Some((id, proposal));
                        if !self.candidates.contains(job) {
                            self.candidates.push_back(*job);
                        }
                    }
                    Err(error) => self.fail_job(*job, error.message, effects),
                    _ => self.fail_job(
                        *job,
                        "worker returned the wrong result kind".into(),
                        effects,
                    ),
                }
            }
            (CallRequest::Tool(_), result) => {
                let job_id = call.job.unwrap();
                let material = match result {
                    Ok(CallOutput::Artifact(value)) => value,
                    Err(error) => json!({"error": error.message, "kind": error.kind}),
                    _ => json!({"error": "tool returned the wrong result kind"}),
                };
                self.jobs
                    .get_mut(&job_id)
                    .unwrap()
                    .view
                    .results
                    .push(json!({
                        "call": id, "effect": call.effect_id, "value": material,
                        "external_effect": outcome.external_effect, "version": call.version,
                    }));
                if self.jobs[&job_id].view.version == call.version
                    && matches!(
                        self.jobs[&job_id].view.state,
                        JobState::Waiting(WaitReason::Tools)
                    )
                {
                    self.make_ready(job_id, false);
                }
                self.changed(job_id, effects);
                self.wake_dependents(job_id);
                if resolved {
                    // Reconciliation can prove an action already happened.
                    // Proposals based on Unknown must reconsider, not blindly
                    // execute a queued replacement with a new effect ID.
                    let affected: Vec<_> = self
                        .jobs
                        .iter()
                        .filter_map(|(id, job)| {
                            ((*id == job_id || job.view.references.contains(&job_id))
                                && !job.view.state.terminal()
                                && !matches!(job.view.state, JobState::Paused))
                            .then_some(*id)
                        })
                        .collect();
                    for id in affected {
                        self.invalidate(id, effects);
                        self.make_ready(id, false);
                        self.changed(id, effects);
                    }
                }
            }
        }
    }

    fn routing_current(&self, routing: &Routing, call: &CallSnapshot) -> bool {
        routing
            .inputs
            .iter()
            .all(|id| matches!(self.inputs[id].state, InputState::Routing))
            && routing.source.is_none_or(|id| {
                self.jobs.get(&id).is_some_and(|job| {
                    job.view.version == call.version
                        && !job.view.state.terminal()
                        && !matches!(job.view.state, JobState::Paused)
                })
            })
    }

    fn validate_decision(
        &self,
        routing: &Routing,
        decision: &KernelDecision,
        basis: u64,
    ) -> Result<(), String> {
        if decision.disposition != RoutingDisposition::Apply {
            if !decision.changes.is_empty() || decision.constraints.is_some() {
                return Err(
                    "an unresolved interpretation cannot also commit control changes".into(),
                );
            }
            return match &decision.disposition {
                RoutingDisposition::Investigate { goal } if goal.trim().is_empty() => {
                    Err("investigation needs a goal".into())
                }
                RoutingDisposition::Clarify { question } if question.trim().is_empty() => {
                    Err("clarification needs a question".into())
                }
                _ => Ok(()),
            };
        }
        if routing.source.is_some() && decision.constraints.is_some() {
            return Err("worker evidence cannot rewrite session instructions".into());
        }
        let allowed_inputs = routing
            .source
            .map(|id| &self.jobs[&id].view.inputs)
            .unwrap_or(&routing.inputs);
        let mut touched = BTreeSet::new();
        let cancelled: Vec<_> = decision
            .changes
            .iter()
            .filter_map(|change| match change {
                JobChange::Update {
                    job,
                    action: JobAction::Cancel,
                    ..
                } => Some(*job),
                _ => None,
            })
            .collect();
        for change in &decision.changes {
            match change {
                JobChange::Create(spec) => {
                    if spec.goal.trim().is_empty()
                        || (routing.source.is_none() && spec.inputs.is_empty())
                    {
                        return Err("a new job needs a goal and original input attribution".into());
                    }
                    if !spec.inputs.iter().all(|id| allowed_inputs.contains(id)) {
                        return Err("job refers to input outside this routing request".into());
                    }
                    if spec.parent.is_some_and(|id| {
                        !self.jobs.contains_key(&id) || self.jobs[&id].view.state.terminal()
                    }) || spec.references.iter().any(|id| !self.jobs.contains_key(id))
                    {
                        return Err("job refers to an unavailable parent or evidence source".into());
                    }
                    if let Some(source) = routing.source
                        && spec.parent != Some(source)
                    {
                        return Err("a worker can create only owned child work".into());
                    }
                    if spec.parent.is_some_and(|parent| {
                        cancelled.iter().any(|root| self.owned_by(parent, *root))
                    }) {
                        return Err(
                            "cannot create work under a parent cancelled in this decision".into(),
                        );
                    }
                }
                JobChange::Update {
                    job,
                    goal,
                    inputs,
                    action,
                    required,
                } => {
                    let target = self.jobs.get(job).ok_or("unknown target job")?;
                    if target.view.version > basis {
                        return Err("target control changed after this routing snapshot".into());
                    }
                    if !touched.insert(*job) {
                        return Err("a job may be updated only once per decision".into());
                    }
                    if *action != JobAction::Cancel
                        && cancelled.iter().any(|root| self.owned_by(*job, *root))
                    {
                        return Err("a cancelled job tree cannot also be updated".into());
                    }
                    if target.view.state.terminal() {
                        return Err("terminal jobs cannot be restarted; create new work".into());
                    }
                    if goal.as_ref().is_some_and(|goal| goal.trim().is_empty()) {
                        return Err("a replacement goal cannot be empty".into());
                    }
                    if !inputs.iter().all(|id| allowed_inputs.contains(id)) {
                        return Err("update refers to input outside this routing request".into());
                    }
                    if routing.source.is_none()
                        && (goal.is_some() || *action == JobAction::Resume)
                        && inputs.is_empty()
                    {
                        return Err(
                            "goal changes and resumes must deliver their original input".into()
                        );
                    }
                    if let Some(source) = routing.source
                        && !self.owned_by(*job, source)
                    {
                        return Err("worker material cannot control unrelated jobs".into());
                    }
                    if routing.source.is_some()
                        && (goal.is_some()
                            || *action == JobAction::Resume
                            || !inputs.is_empty()
                            || *required)
                    {
                        return Err(
                            "worker coordination cannot acquire fresh user authority".into()
                        );
                    }
                }
            }
        }
        Ok(())
    }

    fn apply_decision(
        &mut self,
        routing: Routing,
        decision: KernelDecision,
        effects: &mut Vec<Effect>,
    ) {
        match decision.disposition {
            RoutingDisposition::Investigate { goal } => {
                let refs = self.jobs.keys().copied().collect();
                let job = self.create_job(
                    JobSpec {
                        goal,
                        inputs: routing.inputs.clone(),
                        parent: routing.source,
                        references: refs,
                    },
                    routing.source.is_none(),
                    effects,
                );
                for id in &routing.inputs {
                    self.inputs.get_mut(id).unwrap().state = InputState::Investigating { job };
                }
                self.jobs.get_mut(&job).unwrap().investigation = true;
                self.investigations.insert(job, routing);
                return;
            }
            RoutingDisposition::Clarify { question } => {
                for id in &routing.inputs {
                    self.inputs.get_mut(id).unwrap().state = InputState::WaitingForUser {
                        question: question.clone(),
                    };
                }
                if let Some(source) = routing.source {
                    self.jobs.get_mut(&source).unwrap().view.state =
                        JobState::Waiting(WaitReason::User {
                            question: question.clone(),
                        });
                    self.changed(source, effects);
                }
                let inputs = if routing.inputs.is_empty() {
                    routing
                        .source
                        .map(|id| self.jobs[&id].view.inputs.clone())
                        .unwrap_or_default()
                } else {
                    routing.inputs
                };
                self.publish(
                    Notice::Clarification {
                        inputs,
                        job: routing.source,
                        question,
                    },
                    effects,
                );
                return;
            }
            RoutingDisposition::Apply => {}
        }
        if let Some(constraints) = decision.constraints
            && constraints != self.constraints
        {
            self.constraints = constraints;
            let ids: Vec<_> = self
                .jobs
                .keys()
                .copied()
                .filter(|id| !self.jobs[id].view.state.terminal())
                .collect();
            for id in ids {
                let paused = matches!(self.jobs[&id].view.state, JobState::Paused);
                self.invalidate(id, effects);
                if !paused {
                    self.make_ready(id, false);
                }
            }
        }
        for change in decision.changes {
            match change {
                JobChange::Create(spec) => {
                    let inputs = spec.inputs.clone();
                    let job = self.create_job(spec, routing.source.is_none(), effects);
                    self.require(&inputs, job);
                }
                JobChange::Update {
                    job,
                    goal,
                    action,
                    inputs,
                    required,
                } => {
                    let paused = matches!(self.jobs[&job].view.state, JobState::Paused);
                    let control = goal.is_some()
                        || action != JobAction::Keep
                        || (routing.source.is_none() && required && !inputs.is_empty());
                    if control {
                        self.invalidate(job, effects);
                    }
                    if let Some(goal) = goal {
                        self.jobs.get_mut(&job).unwrap().view.goal = goal;
                    }
                    for input in &inputs {
                        if !self.jobs[&job].view.inputs.contains(input) {
                            self.jobs.get_mut(&job).unwrap().view.inputs.push(*input);
                        }
                    }
                    let mut ordered = self.jobs[&job].view.inputs.clone();
                    ordered.sort_by_key(|id| self.inputs[id].received_at);
                    ordered.dedup();
                    self.jobs.get_mut(&job).unwrap().view.inputs = ordered;
                    match action {
                        JobAction::Pause => {
                            self.jobs.get_mut(&job).unwrap().view.state = JobState::Paused
                        }
                        JobAction::Cancel => self.cancel_job(job, effects),
                        JobAction::Resume => self.make_ready(job, routing.source.is_none()),
                        JobAction::Keep if control && !paused => {
                            self.make_ready(job, routing.source.is_none())
                        }
                        JobAction::Keep
                            if !inputs.is_empty()
                                && matches!(self.jobs[&job].view.state, JobState::Waiting(_)) =>
                        {
                            self.cancel_wakes(job, effects);
                            self.make_ready(job, routing.source.is_none())
                        }
                        JobAction::Keep => {}
                    }
                    if required {
                        self.require(&inputs, job);
                    }
                    self.changed(job, effects);
                }
            }
        }
        let mut required_jobs = BTreeSet::new();
        for id in &routing.inputs {
            let input = self.inputs.get_mut(id).unwrap();
            input.state = InputState::Handled;
            required_jobs.extend(input.required_jobs.iter().copied());
        }
        self.publish(
            Notice::InputHandled {
                inputs: routing.inputs,
                required_jobs: required_jobs.into_iter().collect(),
            },
            effects,
        );
        if let Some(source) = routing.source
            && matches!(
                self.jobs[&source].view.state,
                JobState::Waiting(WaitReason::Coordination)
            )
        {
            self.make_ready(source, false);
        }
    }

    fn routing_failed(&mut self, routing: &Routing, message: String, effects: &mut Vec<Effect>) {
        for id in &routing.inputs {
            self.inputs.get_mut(id).unwrap().state = InputState::RoutingFailed {
                message: message.clone(),
            };
        }
        self.publish(
            Notice::InputRoutingFailed {
                inputs: routing.inputs.clone(),
                message: message.clone(),
            },
            effects,
        );
        if let Some(source) = routing.source {
            self.fail_job(source, message, effects);
        }
    }

    fn create_job(
        &mut self,
        mut spec: JobSpec,
        interactive: bool,
        effects: &mut Vec<Effect>,
    ) -> JobId {
        let id = JobId(self.next_job);
        self.next_job += 1;
        spec.inputs.sort_by_key(|id| self.inputs[id].received_at);
        spec.inputs.dedup();
        self.jobs.insert(
            id,
            Job {
                view: JobSnapshot {
                    id,
                    goal: spec.goal,
                    state: JobState::Ready,
                    version: 0,
                    inputs: spec.inputs,
                    parent: spec.parent,
                    references: spec.references,
                    note: String::new(),
                    results: Vec::new(),
                    active_call: None,
                    progress: None,
                },
                interactive,
                investigation: false,
                candidate: None,
            },
        );
        self.ready.push_back(id);
        self.changed(id, effects);
        id
    }
    fn require(&mut self, inputs: &[InputId], job: JobId) {
        for id in inputs {
            let input = self.inputs.get_mut(id).unwrap();
            if !input.required_jobs.contains(&job) {
                input.required_jobs.push(job);
            }
        }
    }

    fn validate_work(&self, id: JobId, proposal: &WorkProposal) -> Result<(), String> {
        if matches!(proposal.next, Next::AskUser { .. }) && proposal.reply.is_some() {
            return Err("AskUser publishes its question, not an unvalidated final reply".into());
        }
        if let Some(call) = &proposal.operation {
            if !self.tools.contains_key(&call.name) || !call.arguments.is_object() {
                return Err("worker requested an unknown tool or invalid arguments".into());
            }
            if matches!(proposal.next, Next::Finish) {
                return Err("a job cannot finish while starting a tool".into());
            }
            if self.is_investigation(id)
                && self.tools[&call.name].effect == ToolEffect::ExternalWrite
            {
                return Err("input investigation cannot perform a business write".into());
            }
        }
        match &proposal.next {
            Next::WaitForJob { job } | Next::WaitForResult { job } => {
                if !self.jobs.contains_key(job) || self.wait_cycle(id, *job) {
                    return Err("wait target is unknown or would create a cycle".into());
                }
                if matches!(proposal.next, Next::WaitForJob { .. }) && self.owned_by(id, *job) {
                    return Err("a child cannot wait for its owning job to finish".into());
                }
            }
            Next::AskUser { question } if question.trim().is_empty() => {
                return Err("clarification needs a question".into());
            }
            Next::Coordinate { request } if request.trim().is_empty() => {
                return Err("coordination needs a request".into());
            }
            _ => {}
        }
        Ok(())
    }

    fn wait_cycle(&self, origin: JobId, mut target: JobId) -> bool {
        let mut visited = BTreeSet::new();
        loop {
            if target == origin || !visited.insert(target) {
                return true;
            }
            match self.jobs.get(&target).map(|job| &job.view.state) {
                Some(JobState::Waiting(WaitReason::Job { job } | WaitReason::Result { job })) => {
                    target = *job
                }
                _ => return false,
            }
        }
    }
    fn owned_by(&self, mut job: JobId, owner: JobId) -> bool {
        loop {
            if job == owner {
                return true;
            }
            match self.jobs[&job].view.parent {
                Some(parent) => job = parent,
                None => return false,
            }
        }
    }

    fn candidate_wait(&self, id: JobId, proposal: &WorkProposal) -> Option<WaitReason> {
        let investigation = self.is_investigation(id);
        let writing = proposal
            .operation
            .as_ref()
            .is_some_and(|call| self.tools[&call.name].effect == ToolEffect::ExternalWrite);
        let delivering = !investigation
            && (proposal.reply.is_some() || matches!(proposal.next, Next::Finish))
            && !matches!(proposal.next, Next::AskUser { .. });
        if self.inputs.values().any(|input| input.state.unresolved()) && (writing || delivering) {
            return Some(WaitReason::Coordination);
        }
        if writing
            && self
                .calls
                .values()
                .any(|call| call.external_write && call.is_unresolved())
        {
            return Some(WaitReason::Capacity);
        }
        if proposal.operation.is_some()
            && self
                .calls
                .values()
                .filter(|call| matches!(call.request, CallRequest::Tool(_)) && call.is_running())
                .count()
                >= self.config.tool_concurrency
        {
            return Some(WaitReason::Capacity);
        }
        if matches!(proposal.next, Next::Finish) {
            if self
                .calls
                .values()
                .any(|call| call.job == Some(id) && call.external_write && call.is_unresolved())
            {
                return Some(WaitReason::Capacity);
            }
            if let Some(child) = self
                .jobs
                .values()
                .find(|job| job.view.parent == Some(id) && !job.view.state.terminal())
            {
                return Some(WaitReason::Job { job: child.view.id });
            }
        }
        None
    }

    fn commit_work(
        &mut self,
        id: JobId,
        call_id: CallId,
        proposal: WorkProposal,
        effects: &mut Vec<Effect>,
    ) {
        let as_of = self.calls[&call_id].as_of;
        if !proposal.note.is_empty() {
            self.jobs
                .get_mut(&id)
                .unwrap()
                .view
                .results
                .push(json!({"note": proposal.note, "as_of": as_of}));
        }
        if let Some(text) = proposal.reply {
            self.jobs
                .get_mut(&id)
                .unwrap()
                .view
                .results
                .push(json!({"reply": text, "as_of": as_of}));
            if self.jobs[&id].view.parent.is_none() && !self.is_investigation(id) {
                self.publish(
                    Notice::Reply {
                        job: id,
                        text,
                        reply_to: self.jobs[&id].view.inputs.clone(),
                        as_of,
                    },
                    effects,
                );
            }
        }
        if let Some(tool) = proposal.operation {
            self.start_call(
                Some(id),
                CallRequest::Tool(tool.clone()),
                Call::Tool(tool),
                None,
                effects,
            );
        }
        match proposal.next {
            Next::Continue => self.make_ready(id, false),
            Next::Wait {
                reconsider_after: Some(delay),
            } => {
                self.cancel_wakes(id, effects);
                self.jobs.get_mut(&id).unwrap().view.state = JobState::Waiting(WaitReason::Timer);
                let wake = WakeId(self.next_wake);
                self.next_wake += 1;
                self.wakes.insert(wake, (id, self.jobs[&id].view.version));
                effects.push(Effect::WakeAfter { id: wake, delay });
            }
            Next::Wait {
                reconsider_after: None,
            } => {
                if self.calls.values().any(|call| {
                    call.job == Some(id)
                        && matches!(call.request, CallRequest::Tool(_))
                        && call.is_running()
                }) {
                    self.jobs.get_mut(&id).unwrap().view.state =
                        JobState::Waiting(WaitReason::Tools);
                } else {
                    // The result may have arrived while this worker was still
                    // deciding to wait. Do not lose that wakeup.
                    self.make_ready(id, false);
                }
            }
            Next::WaitForResult { job } => {
                self.reference(id, job);
                self.jobs.get_mut(&id).unwrap().view.state =
                    JobState::Waiting(WaitReason::Result { job });
            }
            Next::WaitForJob { job } => {
                self.reference(id, job);
                self.jobs.get_mut(&id).unwrap().view.state =
                    JobState::Waiting(WaitReason::Job { job });
            }
            Next::AskUser { question } => {
                self.jobs.get_mut(&id).unwrap().view.state = JobState::Waiting(WaitReason::User {
                    question: question.clone(),
                });
                self.publish(
                    Notice::Clarification {
                        inputs: self.jobs[&id].view.inputs.clone(),
                        job: Some(id),
                        question,
                    },
                    effects,
                );
            }
            Next::Coordinate { request } => {
                self.jobs.get_mut(&id).unwrap().view.state =
                    JobState::Waiting(WaitReason::Coordination);
                self.coordination
                    .push_back((id, self.jobs[&id].view.version, request));
            }
            Next::Finish => {
                self.jobs.get_mut(&id).unwrap().view.state = JobState::Completed;
                self.cancel_calls(id, effects);
                self.cancel_wakes(id, effects);
                self.publish(
                    Notice::JobFinished {
                        id,
                        state: JobState::Completed,
                    },
                    effects,
                );
                if let Some(routing) = self.investigations.remove(&id) {
                    for input in &routing.inputs {
                        self.inputs.get_mut(input).unwrap().state = InputState::Pending;
                    }
                    if routing.inputs.is_empty()
                        && let Some(source) = routing.source
                        && self.jobs[&source].view.version == routing.source_version
                        && matches!(
                            self.jobs[&source].view.state,
                            JobState::Waiting(WaitReason::Coordination)
                        )
                    {
                        self.coordination.push_back((
                            source,
                            self.jobs[&source].view.version,
                            routing.request.unwrap_or_default(),
                        ));
                    }
                }
            }
        }
        self.changed(id, effects);
        self.wake_dependents(id);
    }

    fn advance(&mut self, effects: &mut Vec<Effect>) {
        // Successful candidates leave the queue. Blocked ones keep their turn;
        // a fast, low-ID job cannot repeatedly jump a waiting tool request.
        for _ in 0..self.candidates.len() {
            let id = self.candidates.pop_front().unwrap();
            let Some((_, proposal)) = &self.jobs[&id].candidate else {
                continue;
            };
            if let Err(message) = self.validate_work(id, proposal) {
                self.fail_job(id, message, effects);
                continue;
            }
            if let Some(reason) =
                self.candidate_wait(id, &self.jobs[&id].candidate.as_ref().unwrap().1)
            {
                let state = JobState::Waiting(reason);
                if self.jobs[&id].view.state != state {
                    self.jobs.get_mut(&id).unwrap().view.state = state;
                    self.changed(id, effects);
                }
                self.candidates.push_back(id);
                if self.inputs.values().any(|input| input.state.unresolved()) {
                    self.draining = true;
                }
                continue;
            }
            let (call, proposal) = self.jobs.get_mut(&id).unwrap().candidate.take().unwrap();
            self.commit_work(id, call, proposal, effects);
        }
        if !self.inputs.values().any(|input| input.state.unresolved()) {
            self.draining = false;
        }
        self.check_waits(effects);
        self.settle_inputs(effects);
        self.schedule_kernel(effects);
        self.schedule_workers(effects);
    }

    fn schedule_kernel(&mut self, effects: &mut Vec<Effect>) {
        if self
            .calls
            .values()
            .any(|call| matches!(call.request, CallRequest::Kernel { .. }) && call.is_running())
        {
            return;
        }
        let has_pending = self
            .inputs
            .values()
            .any(|input| matches!(input.state, InputState::Pending));
        let mut pending: Vec<_> = self
            .inputs
            .values()
            .filter(|input| has_pending && input.state.unresolved())
            .collect();
        pending.sort_by_key(|input| input.received_at);
        let ids: Vec<_> = pending.iter().map(|input| input.input.id).collect();
        if !ids.is_empty() {
            // A new input explicitly takes over the unresolved interpretation:
            // retain the old words in order, never let a later retry overwrite it.
            // Existing investigations may finish as evidence, but lose their
            // authority to reopen this batch behind the new interpretation.
            self.investigations
                .retain(|_, routing| !routing.inputs.iter().any(|id| ids.contains(id)));
            for id in &ids {
                self.inputs.get_mut(id).unwrap().state = InputState::Routing;
            }
            self.start_routing(
                Routing {
                    inputs: ids,
                    source: None,
                    source_version: 0,
                    request: None,
                },
                effects,
            );
            return;
        }
        while let Some((source, version, request)) = self.coordination.pop_front() {
            if self.jobs.get(&source).is_some_and(|job| {
                job.view.version == version
                    && matches!(job.view.state, JobState::Waiting(WaitReason::Coordination))
            }) {
                self.start_routing(
                    Routing {
                        inputs: Vec::new(),
                        source: Some(source),
                        source_version: version,
                        request: Some(request),
                    },
                    effects,
                );
                break;
            }
        }
    }

    fn start_routing(&mut self, routing: Routing, effects: &mut Vec<Effect>) {
        let task = ModelTask::Kernel {
            inputs: routing
                .inputs
                .iter()
                .map(|id| self.inputs[id].input.clone())
                .collect(),
            source: routing.source,
            request: routing.request.clone(),
        };
        let request = CallRequest::Kernel {
            inputs: routing.inputs,
            source: routing.source,
            request: routing.request,
        };
        let call = Call::Model(ModelInput {
            task,
            snapshot: self.snapshot(),
        });
        self.start_call(
            routing.source,
            request,
            call,
            Some(self.config.kernel_timeout),
            effects,
        );
    }

    fn schedule_workers(&mut self, effects: &mut Vec<Effect>) {
        let mut interactive_used = 0;
        let mut background_used = 0;
        for call in self.calls.values().filter(|call| call.is_running()) {
            if let CallRequest::Work { interactive, .. } = call.request {
                if interactive {
                    interactive_used += 1;
                } else {
                    background_used += 1;
                }
            }
        }
        self.ready.retain(|id| {
            let job = &self.jobs[id];
            matches!(job.view.state, JobState::Ready)
                && job.view.active_call.is_none()
                && job.candidate.is_none()
        });
        // The reserve belongs to new user work, not automatic continuations.
        if interactive_used == 0
            && let Some(index) = self.ready.iter().position(|id| self.jobs[id].interactive)
        {
            let id = self.ready.remove(index).unwrap();
            self.start_worker(id, true, effects);
        }
        for _ in background_used..self.config.background_concurrency {
            let roots: BTreeSet<_> = self.ready.iter().map(|id| self.root(*id)).collect();
            let next = roots
                .iter()
                .find(|&&root| self.last_root.is_none_or(|last| root > last))
                .or_else(|| roots.first())
                .copied();
            let Some(root) = next else {
                break;
            };
            let index = self
                .ready
                .iter()
                .position(|id| self.root(*id) == root)
                .unwrap();
            let id = self.ready.remove(index).unwrap();
            self.start_worker(id, false, effects);
            self.last_root = Some(root);
        }
    }

    fn start_worker(&mut self, id: JobId, interactive: bool, effects: &mut Vec<Effect>) {
        let task = ModelTask::Work {
            job: id,
            messages: self.jobs[&id]
                .view
                .inputs
                .iter()
                .map(|input| self.inputs[input].input.clone())
                .collect(),
        };
        let call = Call::Model(ModelInput {
            task,
            snapshot: self.snapshot(),
        });
        let call_id = self.start_call(
            Some(id),
            CallRequest::Work {
                job: id,
                interactive,
            },
            call,
            Some(self.config.work_timeout),
            effects,
        );
        let job = self.jobs.get_mut(&id).unwrap();
        job.interactive = false;
        job.view.active_call = Some(call_id);
        job.view.state = JobState::Running;
        self.changed(id, effects);
    }
    fn root(&self, mut id: JobId) -> JobId {
        while let Some(parent) = self.jobs[&id].view.parent {
            id = parent;
        }
        id
    }

    fn is_investigation(&self, mut id: JobId) -> bool {
        loop {
            if self.jobs[&id].investigation {
                return true;
            }
            match self.jobs[&id].view.parent {
                Some(parent) => id = parent,
                None => return false,
            }
        }
    }

    fn reference(&mut self, id: JobId, source: JobId) {
        let references = &mut self.jobs.get_mut(&id).unwrap().view.references;
        if !references.contains(&source) {
            references.push(source);
        }
    }

    fn start_call(
        &mut self,
        job: Option<JobId>,
        request: CallRequest,
        call: Call,
        timeout: Option<Duration>,
        effects: &mut Vec<Effect>,
    ) -> CallId {
        let id = CallId(self.next_call);
        self.next_call += 1;
        let external_write = matches!(&request, CallRequest::Tool(tool) if self.tools[&tool.name].effect == ToolEffect::ExternalWrite);
        self.calls.insert(
            id,
            CallSnapshot {
                id,
                effect_id: EffectId(id.0),
                job,
                request: request.clone(),
                version: job
                    .map(|job| self.jobs[&job].view.version)
                    .unwrap_or_default(),
                generation: self.generation,
                as_of: self.record_cursor(),
                external_write,
                state: CallState::Running,
                progress: None,
            },
        );
        self.publish(Notice::CallStarted { id, job, request }, effects);
        effects.push(Effect::Start { id, call, timeout });
        id
    }

    fn make_ready(&mut self, id: JobId, interactive: bool) {
        let job = self.jobs.get_mut(&id).unwrap();
        if job.view.state.terminal() {
            return;
        }
        job.view.state = JobState::Ready;
        job.interactive |= interactive;
        if !self.ready.contains(&id) {
            self.ready.push_back(id);
        }
    }
    fn invalidate(&mut self, id: JobId, effects: &mut Vec<Effect>) {
        let version = self.record_cursor() + 1;
        let job = self.jobs.get_mut(&id).unwrap();
        job.view.version = version;
        job.view.active_call = None;
        job.view.progress = None;
        job.candidate = None;
        self.cancel_calls(id, effects);
        self.cancel_wakes(id, effects);
    }
    fn cancel_calls(&mut self, job: JobId, effects: &mut Vec<Effect>) {
        for call in self
            .calls
            .values_mut()
            .filter(|call| call.job == Some(job) && matches!(call.state, CallState::Running))
        {
            call.state = CallState::CancelRequested;
            effects.push(Effect::RequestCancel { id: call.id });
        }
    }
    fn cancel_wakes(&mut self, job: JobId, effects: &mut Vec<Effect>) {
        let ids: Vec<_> = self
            .wakes
            .iter()
            .filter_map(|(id, (owner, _))| (*owner == job).then_some(*id))
            .collect();
        for id in ids {
            self.wakes.remove(&id);
            effects.push(Effect::CancelWake { id });
        }
    }
    fn cancel_job(&mut self, id: JobId, effects: &mut Vec<Effect>) {
        let ids: Vec<_> = self
            .jobs
            .keys()
            .copied()
            .filter(|job| self.owned_by(*job, id))
            .collect();
        for id in ids {
            if self.jobs[&id].view.state.terminal() {
                continue;
            }
            self.invalidate(id, effects);
            self.jobs.get_mut(&id).unwrap().view.state = JobState::Cancelled;
            self.publish(
                Notice::JobFinished {
                    id,
                    state: JobState::Cancelled,
                },
                effects,
            );
            self.changed(id, effects);
            self.wake_dependents(id);
            if let Some(routing) = self.investigations.remove(&id)
                && self.owns_investigation(id, &routing)
            {
                self.routing_failed(
                    &routing,
                    "input investigation was cancelled".into(),
                    effects,
                );
            }
        }
    }
    fn fail_job(&mut self, id: JobId, message: String, effects: &mut Vec<Effect>) {
        self.invalidate(id, effects);
        let state = JobState::Failed {
            message: message.clone(),
        };
        self.jobs.get_mut(&id).unwrap().view.state = state.clone();
        self.publish(Notice::JobFinished { id, state }, effects);
        self.changed(id, effects);
        if let Some(routing) = self.investigations.remove(&id)
            && self.owns_investigation(id, &routing)
        {
            self.routing_failed(&routing, message, effects);
        }
        self.wake_dependents(id);
    }

    fn owns_investigation(&self, id: JobId, routing: &Routing) -> bool {
        routing.inputs.iter().any(|input| {
            matches!(self.inputs[input].state, InputState::Investigating { job } if job == id)
        }) || routing.source.is_some_and(|source| {
            self.jobs[&source].view.version == routing.source_version
                && matches!(self.jobs[&source].view.state, JobState::Waiting(WaitReason::Coordination))
        })
    }

    fn wake_dependents(&mut self, source: JobId) {
        let parent = self.jobs[&source].view.parent;
        if let Some(parent) = parent
            && self.jobs[&parent]
                .candidate
                .as_ref()
                .is_some_and(|(_, proposal)| matches!(proposal.next, Next::Finish))
        {
            self.jobs.get_mut(&parent).unwrap().candidate = None;
            if !self.jobs[&parent].view.state.terminal()
                && !matches!(self.jobs[&parent].view.state, JobState::Paused)
            {
                self.make_ready(parent, false);
            }
        }
    }
    fn check_waits(&mut self, effects: &mut Vec<Effect>) {
        let waits: Vec<_> = self
            .jobs
            .iter()
            .filter_map(|(id, job)| match &job.view.state {
                JobState::Waiting(WaitReason::Job { job }) => Some((*id, *job, false)),
                JobState::Waiting(WaitReason::Result { job }) => Some((*id, *job, true)),
                _ => None,
            })
            .collect();
        for (id, target, result) in waits {
            let target = &self.jobs[&target].view;
            if result && !target.results.is_empty() || !result && target.state.terminal() {
                self.make_ready(id, false);
            } else if result && target.state.terminal() {
                self.fail_job(
                    id,
                    "dependency ended without producing a result".into(),
                    effects,
                );
            }
        }
    }
    fn settle_inputs(&mut self, effects: &mut Vec<Effect>) {
        let ids: Vec<_> = self
            .inputs
            .iter()
            .filter_map(|(id, input)| {
                (matches!(input.state, InputState::Handled)
                    && input
                        .required_jobs
                        .iter()
                        .all(|id| self.jobs[id].view.state.terminal()))
                .then_some(*id)
            })
            .collect();
        for id in ids {
            let jobs = &self.inputs[&id].required_jobs;
            let failure = jobs.iter().find_map(|id| match &self.jobs[id].view.state {
                JobState::Failed { message } => Some(message.clone()),
                _ => None,
            });
            let outcome = if let Some(message) = failure {
                InputOutcome::Failed { message }
            } else if jobs
                .iter()
                .any(|id| matches!(self.jobs[id].view.state, JobState::Cancelled))
            {
                InputOutcome::Cancelled
            } else {
                InputOutcome::Completed
            };
            self.inputs.get_mut(&id).unwrap().state = InputState::Finished(outcome.clone());
            self.publish(Notice::InputFinished { id, outcome }, effects);
        }
    }

    fn stop(&mut self, effects: &mut Vec<Effect>) {
        self.generation += 1;
        self.draining = false;
        self.coordination.clear();
        self.investigations.clear();
        let jobs: Vec<_> = self.jobs.keys().copied().collect();
        for id in jobs {
            if !self.jobs[&id].view.state.terminal() {
                self.cancel_job(id, effects);
            }
        }
        for call in self
            .calls
            .values_mut()
            .filter(|call| matches!(call.state, CallState::Running))
        {
            call.state = CallState::CancelRequested;
            effects.push(Effect::RequestCancel { id: call.id });
        }
        let inputs: Vec<_> = self
            .inputs
            .iter()
            .filter_map(|(id, input)| {
                (!matches!(input.state, InputState::Finished(_))).then_some(*id)
            })
            .collect();
        for id in inputs {
            self.inputs.get_mut(&id).unwrap().state = InputState::Finished(InputOutcome::Cancelled);
            self.publish(
                Notice::InputFinished {
                    id,
                    outcome: InputOutcome::Cancelled,
                },
                effects,
            );
        }
        self.publish(Notice::Stopped, effects);
    }
    fn changed(&mut self, id: JobId, effects: &mut Vec<Effect>) {
        self.publish(
            Notice::JobChanged {
                job: self.jobs[&id].view.clone(),
            },
            effects,
        );
    }
    fn discard(&mut self, call: CallId, reason: &str, _effects: &mut Vec<Effect>) {
        self.append(RecordKind::ProposalDiscarded {
            call,
            reason: reason.into(),
        });
    }
    fn append(&mut self, kind: RecordKind) {
        self.record.push(RecordEntry {
            cursor: self.record_cursor() + 1,
            kind,
        });
    }
    fn publish(&mut self, notice: Notice, effects: &mut Vec<Effect>) {
        self.append(RecordKind::Notice(notice.clone()));
        effects.push(Effect::Publish(notice));
    }
}
