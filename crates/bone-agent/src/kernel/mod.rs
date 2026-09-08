use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
};

use crate::{
    AdmissionError, AgentLimits, AgentLimitsError, AgentView, Assignment, Await, Call, CallError,
    CallId, CallKind, CallProgress, CallStatus, CallView, Completion, DeliveryKind, DeliveryTarget,
    Effect, Event, ExternalEffect, Input, InputId, InputOutcome, InputReceipt, InputStatus,
    InputView, InquiryResponse, InquiryResult, JobAction, JobChange, JobId, JobOutcome, JobSpec,
    JobStatus, JobView, KernelDecision, MonoTime, Origin, OutcomeKind, Owner, ReadQuery, Record,
    RecordBody, RecordRange, ReportDraft, Seq, ToolEffect, ToolOutcome, ToolSpec, WorkProposal,
    WorkStep,
    context::{self, PreparedWork},
    job::{Job, JobContext, JobState, PendingStep, WaitState},
};

mod exchange;
mod routing;
mod scheduler;
mod work;

#[derive(Clone, Debug)]
pub(crate) struct InputEntry {
    pub accepted_at: Seq,
    pub finished: Option<InputOutcome>,
    pub required_jobs: BTreeSet<JobId>,
    pub routing: Seq,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Requester {
    Inputs,
    Job { job: JobId, revision: u64 },
}

#[derive(Clone, Debug)]
pub(crate) struct Routing {
    pub requester: Requester,
    pub inputs: Vec<InputId>,
    pub request: Option<String>,
    pub records: Vec<Seq>,
    pub active_call: Option<CallId>,
    pub state: RoutingState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RoutingState {
    Ready,
    WaitingInquiry(Seq),
    WaitingJob(JobId),
    WaitingForUser(Seq),
    Failed(Seq),
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReplyTarget {
    Routing(Seq),
    Job(JobId),
}

#[derive(Clone, Debug)]
pub(crate) struct Inquiry {
    pub requester: DeliveryTarget,
    pub target: JobId,
    pub target_revision: u64,
    pub deadline: MonoTime,
}

#[derive(Clone, Debug)]
enum CallTask {
    Coordinate {
        routing: Seq,
    },
    Work {
        job: JobId,
        revision: u64,
        seen_through: Seq,
        inquiries: Vec<Seq>,
    },
    Compact {
        job: JobId,
        revision: u64,
        through: Seq,
    },
    Tool {
        job: JobId,
        revision: u64,
        effect: ToolEffect,
        request: Arc<crate::ToolCall>,
    },
}

#[derive(Clone, Debug)]
enum CallState {
    Running,
    CancelRequested,
    ModelFinished(Option<CallError>),
    ToolFinished(Arc<ToolOutcome>),
}

#[derive(Clone, Debug)]
pub(crate) struct CallEntry {
    task: CallTask,
    state: CallState,
    progress: Option<CallProgress>,
}

impl CallEntry {
    fn running(&self) -> bool {
        matches!(self.state, CallState::Running | CallState::CancelRequested)
    }

    fn view(&self, id: CallId) -> CallView {
        CallView {
            id,
            kind: self.kind(),
            job: self.job(),
            tool: self.tool_request().cloned(),
            status: self.status(),
            progress: self.progress.clone(),
        }
    }

    fn status(&self) -> CallStatus {
        match &self.state {
            CallState::Running => CallStatus::Running,
            CallState::CancelRequested => CallStatus::CancelRequested,
            CallState::ModelFinished(error) => CallStatus::Finished {
                error: error.clone(),
                external_effect: ExternalEffect::None,
            },
            CallState::ToolFinished(outcome) => CallStatus::Finished {
                error: outcome.result.as_ref().err().cloned(),
                external_effect: outcome.external_effect,
            },
        }
    }

    fn kind(&self) -> CallKind {
        match &self.task {
            CallTask::Coordinate { .. } => CallKind::Coordinate,
            CallTask::Work { .. } => CallKind::Work,
            CallTask::Compact { .. } => CallKind::Compact,
            CallTask::Tool { .. } => CallKind::Tool,
        }
    }

    fn job(&self) -> Option<JobId> {
        match &self.task {
            CallTask::Coordinate { .. } => None,
            CallTask::Work { job, .. }
            | CallTask::Compact { job, .. }
            | CallTask::Tool { job, .. } => Some(*job),
        }
    }

    fn revision(&self) -> u64 {
        match &self.task {
            CallTask::Coordinate { .. } => 0,
            CallTask::Work { revision, .. }
            | CallTask::Compact { revision, .. }
            | CallTask::Tool { revision, .. } => *revision,
        }
    }

    fn tool_effect(&self) -> Option<ToolEffect> {
        match &self.task {
            CallTask::Tool { effect, .. } => Some(*effect),
            _ => None,
        }
    }

    fn tool_request(&self) -> Option<&Arc<crate::ToolCall>> {
        match &self.task {
            CallTask::Tool { request, .. } => Some(request),
            _ => None,
        }
    }

    fn external_effect(&self) -> ExternalEffect {
        match &self.state {
            CallState::ToolFinished(outcome) => outcome.external_effect,
            _ => ExternalEffect::None,
        }
    }

    fn work_context(&self) -> (Seq, &[Seq]) {
        match &self.task {
            CallTask::Work {
                seen_through,
                inquiries,
                ..
            } => (*seen_through, inquiries),
            _ => unreachable!("work completion belongs to a work call"),
        }
    }

    fn compact_context(&self) -> (u64, Seq) {
        match &self.task {
            CallTask::Compact {
                revision, through, ..
            } => (*revision, *through),
            _ => unreachable!("checkpoint completion belongs to a compact call"),
        }
    }
}

pub(crate) struct Kernel {
    pub limits: AgentLimits,
    pub jobs: BTreeMap<JobId, Job>,
    pub inputs: BTreeMap<InputId, InputEntry>,
    pub calls: BTreeMap<CallId, CallEntry>,
    pub records: BTreeMap<Seq, Arc<Record>>,
    pub inquiries: BTreeMap<Seq, Inquiry>,
    pub routings: BTreeMap<Seq, Routing>,
    pub tools: BTreeMap<String, ToolSpec>,
    pub constraints: String,
    pub user_question: Option<JobId>,
    interactive_ready: VecDeque<JobId>,
    background_ready: VecDeque<JobId>,
    routing_ready: VecDeque<Seq>,
    next_job: u64,
    next_call: u64,
    next_seq: u64,
}

impl Kernel {
    pub fn new(limits: AgentLimits, tools: Vec<ToolSpec>) -> Result<Self, KernelError> {
        limits.validate()?;
        let mut registry = BTreeMap::new();
        for tool in tools {
            if tool.name.trim().is_empty() {
                return Err(KernelError::InvalidToolName);
            }
            let name = tool.name.clone();
            if registry.insert(name.clone(), tool).is_some() {
                return Err(KernelError::DuplicateTool(name));
            }
        }
        Ok(Self {
            limits,
            jobs: BTreeMap::new(),
            inputs: BTreeMap::new(),
            calls: BTreeMap::new(),
            records: BTreeMap::new(),
            inquiries: BTreeMap::new(),
            routings: BTreeMap::new(),
            tools: registry,
            constraints: String::new(),
            user_question: None,
            interactive_ready: VecDeque::new(),
            background_ready: VecDeque::new(),
            routing_ready: VecDeque::new(),
            next_job: 1,
            next_call: 1,
            next_seq: 1,
        })
    }

    pub fn accept(
        &mut self,
        now: MonoTime,
        input: Input,
    ) -> Result<(InputReceipt, Vec<Effect>), AdmissionError> {
        if let Some(existing) = self.inputs.get(&input.id) {
            if self.input(existing.accepted_at) == &input {
                return Ok((
                    InputReceipt {
                        id: input.id,
                        accepted_at: existing.accepted_at,
                    },
                    Vec::new(),
                ));
            }
            return Err(AdmissionError::ConflictingInput);
        }
        let reply = match input.reply_to {
            Some(reply_to) => self
                .reply_target(reply_to)
                .ok_or(AdmissionError::InvalidReply)?
                .into(),
            None => None,
        };
        let pending = self
            .inputs
            .values()
            .filter(|entry| {
                entry.finished.is_none() && self.input(entry.accepted_at).reply_to.is_none()
            })
            .count();
        let pending_reply = self.inputs.values().any(|entry| {
            entry.finished.is_none()
                && self.input(entry.accepted_at).reply_to.is_some()
                && matches!(
                    self.routings[&entry.routing].state,
                    RoutingState::Ready
                        | RoutingState::WaitingInquiry(_)
                        | RoutingState::WaitingJob(_)
                )
        });
        if reply.is_some() && pending_reply
            || reply.is_none() && pending >= self.limits.pending_inputs
        {
            return Err(AdmissionError::Busy);
        }

        let mut effects = Vec::new();
        let input_record = self.record(
            Origin::User(input.id),
            RecordBody::Input(input.clone()),
            &mut effects,
        );
        let routing = match reply {
            Some(ReplyTarget::Routing(routing)) => {
                let route = self
                    .routings
                    .get_mut(&routing)
                    .expect("input points to its routing");
                route.inputs.push(input.id);
                route.records.push(input_record.seq);
                route.state = RoutingState::Ready;
                self.enqueue_routing(routing);
                routing
            }
            Some(ReplyTarget::Job(job)) => {
                self.accept_user_reply(job, input.id, input_record.seq, &mut effects)
            }
            None => self.open_input_routing(input.id, input_record.seq, &mut effects),
        };
        let required_jobs = match reply {
            Some(ReplyTarget::Job(job)) => BTreeSet::from([job]),
            _ => BTreeSet::new(),
        };
        let receipt = InputReceipt {
            id: input.id,
            accepted_at: input_record.seq,
        };
        self.inputs.insert(
            input.id,
            InputEntry {
                accepted_at: input_record.seq,
                finished: None,
                required_jobs,
                routing,
            },
        );
        if let Some(ReplyTarget::Job(job)) = reply {
            self.deliver(
                DeliveryTarget::Job(job),
                input_record.seq,
                DeliveryKind::Input,
                &mut effects,
            );
            self.make_ready(job);
        }
        self.advance(now, &mut effects);
        Ok((receipt, effects))
    }

    pub fn step(&mut self, now: MonoTime, event: Event) -> Vec<Effect> {
        let mut effects = Vec::new();
        self.expire(now, &mut effects);
        match event {
            Event::CoordinateFinished { call, result } => {
                self.coordinate_finished(now, call, result, &mut effects)
            }
            Event::WorkFinished { call, result } => {
                self.work_finished(now, call, result, &mut effects)
            }
            Event::CompactFinished { call, result } => {
                self.compact_finished(call, result, &mut effects)
            }
            Event::ToolFinished { call, result } => self.tool_finished(call, result, &mut effects),
            Event::Progress { call, progress } => {
                let job = self
                    .calls
                    .get(&call)
                    .filter(|entry| matches!(entry.state, CallState::Running))
                    .and_then(CallEntry::job);
                if let Some(entry) = self.calls.get_mut(&call).filter(|entry| {
                    matches!(entry.state, CallState::Running)
                        && entry.progress.as_ref() != Some(&progress)
                }) {
                    entry.progress = Some(progress.clone());
                    self.record(
                        Origin::Call(call),
                        RecordBody::CallProgress {
                            call,
                            job,
                            progress,
                        },
                        &mut effects,
                    );
                }
            }
            Event::WriteResolved { call, result } => {
                self.write_resolved(call, result, &mut effects)
            }
            Event::Pause(job) => self.pause(job, &mut effects),
            Event::Resume(job) => self.resume(job),
            Event::Cancel(job) => self.cancel(job, &mut effects),
            Event::Retry(input) => self.retry(input),
            Event::Stop => self.stop(&mut effects),
            Event::Tick => {}
        }
        self.advance(now, &mut effects);
        effects
    }

    pub fn next_deadline(&self) -> Option<MonoTime> {
        let jobs = self.jobs.values().filter_map(|job| match job.state {
            JobState::Waiting(WaitState::Until(deadline)) => Some(deadline),
            _ => None,
        });
        jobs.chain(self.inquiries.values().map(|inquiry| inquiry.deadline))
            .min()
    }

    pub fn view(&self) -> AgentView {
        AgentView {
            sequence: Seq(self.next_seq.saturating_sub(1)),
            constraints: self.constraints.clone(),
            inputs: self
                .inputs
                .iter()
                .map(|(id, entry)| InputView {
                    input: self.input(entry.accepted_at).clone(),
                    accepted_at: entry.accepted_at,
                    status: self.input_status(*id, entry),
                    required_jobs: entry.required_jobs.iter().copied().collect(),
                })
                .collect(),
            jobs: self
                .jobs
                .keys()
                .copied()
                .map(|id| self.job_view(id))
                .collect(),
            calls: self
                .calls
                .iter()
                .filter(|(_, call)| {
                    call.running() || call.external_effect() == ExternalEffect::Unknown
                })
                .map(|(id, call)| call.view(*id))
                .collect(),
            records: self.records.values().cloned().collect(),
        }
    }

    pub(crate) fn input(&self, record: Seq) -> &Input {
        match &self.records[&record].body {
            RecordBody::Input(input) => input,
            _ => unreachable!("an input entry points to its input record"),
        }
    }

    fn input_status(&self, id: InputId, input: &InputEntry) -> InputStatus {
        if let Some(outcome) = &input.finished {
            return InputStatus::Finished(outcome.clone());
        }
        if let Some(job) = self.user_question
            && self.jobs[&job].inputs.contains(&id)
            && let JobState::Waiting(WaitState::User { question }) = self.jobs[&job].state
        {
            return match &self.records[&question].body {
                RecordBody::Clarification { question, .. } => InputStatus::WaitingForUser {
                    question: question.clone(),
                },
                _ => unreachable!("a user wait points to its clarification"),
            };
        }
        match self.routings[&input.routing].state {
            RoutingState::WaitingForUser(record) => match &self.records[&record].body {
                RecordBody::Clarification { question, .. } => InputStatus::WaitingForUser {
                    question: question.clone(),
                },
                _ => unreachable!("waiting routing points to its clarification"),
            },
            RoutingState::Failed(record) => match &self.records[&record].body {
                RecordBody::InputRoutingFailed { message, .. } | RecordBody::Audit { message } => {
                    InputStatus::RoutingFailed {
                        message: message.clone(),
                    }
                }
                _ => unreachable!("failed routing points to its audit record"),
            },
            RoutingState::Closed => InputStatus::Handled,
            RoutingState::Ready | RoutingState::WaitingInquiry(_) | RoutingState::WaitingJob(_) => {
                InputStatus::Routing
            }
        }
    }

    pub(crate) fn children_of(&self, parent: JobId) -> impl Iterator<Item = JobId> + '_ {
        self.jobs.iter().filter_map(move |(id, job)| {
            matches!(job.owner, Owner::Job(owner) if owner == parent).then_some(*id)
        })
    }

    pub(crate) fn calls_for(&self, job: JobId) -> Vec<CallView> {
        self.calls
            .iter()
            .filter(|(_, call)| {
                call.job() == Some(job)
                    && (call.running() || call.external_effect() == ExternalEffect::Unknown)
            })
            .map(|(id, call)| call.view(*id))
            .collect()
    }

    pub(crate) fn unresolved_writes(&self) -> Vec<crate::UnresolvedWrite> {
        self.calls
            .iter()
            .filter_map(|(id, call)| {
                let request = call.tool_request()?;
                if call.tool_effect()? != ToolEffect::ExternalWrite
                    || (!call.running() && call.external_effect() != ExternalEffect::Unknown)
                {
                    return None;
                }
                Some(crate::UnresolvedWrite {
                    call: *id,
                    job: call.job().expect("tool calls belong to a job"),
                    tool: request.name.clone(),
                })
            })
            .collect()
    }

    pub(crate) fn job_status(&self, id: JobId) -> JobStatus {
        let job = &self.jobs[&id];
        if let JobState::Finished(outcome) = &job.state {
            return JobStatus::Finished(outcome.clone());
        }
        if self.is_paused(id) {
            return JobStatus::Paused;
        }
        if job.active_call.is_some() {
            return JobStatus::Running;
        }
        match &job.state {
            JobState::Ready => JobStatus::Ready,
            JobState::Waiting(wait) => JobStatus::Waiting(wait.view()),
            JobState::Finished(_) => unreachable!(),
        }
    }

    fn job_view(&self, id: JobId) -> JobView {
        let job = &self.jobs[&id];
        JobView {
            id,
            spec: job.spec.clone(),
            owner: job.owner,
            revision: job.revision,
            status: self.job_status(id),
            inputs: job.inputs.clone(),
            report: job.report,
        }
    }

    fn peek_seq(&self) -> Seq {
        Seq(self.next_seq)
    }

    fn record(
        &mut self,
        origin: Origin,
        body: RecordBody,
        effects: &mut Vec<Effect>,
    ) -> Arc<Record> {
        let seq = Seq(self.next_seq);
        self.next_seq += 1;
        let record = Arc::new(Record { seq, origin, body });
        self.records.insert(seq, record.clone());
        effects.push(Effect::Notify(record.clone()));
        record
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum KernelError {
    #[error(transparent)]
    Limits(#[from] AgentLimitsError),
    #[error("tool names cannot be empty")]
    InvalidToolName,
    #[error("duplicate tool: {0}")]
    DuplicateTool(String),
}
