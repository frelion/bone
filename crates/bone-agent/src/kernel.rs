use std::{collections::BTreeMap, time::Duration};

use crate::*;

#[derive(Clone, Debug)]
pub struct KernelConfig {
    pub soft_deadline: Duration,
    pub work_timeout: Duration,
    pub review_timeout: Duration,
}

impl Default for KernelConfig {
    fn default() -> Self {
        Self {
            soft_deadline: Duration::from_secs(30),
            work_timeout: Duration::from_secs(120),
            review_timeout: Duration::from_secs(120),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum KernelError {
    #[error("{0} must be greater than zero")]
    ZeroDuration(&'static str),
    #[error("invalid tool specification: {0}")]
    InvalidTool(String),
    #[error("duplicate tool name: {0}")]
    DuplicateTool(String),
}

/// A request owns a fixed batch. A cancelling request still owns its local slot.
struct Request {
    id: JobId,
    messages: Vec<Message>,
    generation: u64,
    revision: u64,
    as_of: u64,
}

struct Candidate {
    request: Request,
    result: WorkResult,
}

/// All session changes happen here. There is no I/O, clock access, or await.
pub struct Kernel {
    config: KernelConfig,
    tools: BTreeMap<String, ToolSpec>,
    record: Vec<RecordEntry>,
    jobs: BTreeMap<JobId, JobSnapshot>,
    /// Positions, rather than message IDs, determine input order and stop boundaries.
    seen_messages: BTreeMap<MessageId, u64>,
    pending: Vec<Message>,
    work: Option<Request>,
    candidate: Option<Candidate>,
    review: Option<Request>,
    requirement: Option<String>,
    autonomous: bool,
    generation: u64,
    revision: u64,
    /// No input at or before this Pause/Stop boundary can restart old work.
    input_boundary: u64,
    /// Coalesces further work. Retained input after a model failure cannot retry itself.
    need_work: bool,
    /// Some(job) is a tool's soft deadline; None is a model-requested reminder.
    wakes: BTreeMap<WakeId, Option<JobId>>,
    next_job: u64,
    next_wake: u64,
}

impl Kernel {
    pub fn new(config: KernelConfig, tools: Vec<ToolSpec>) -> Result<Self, KernelError> {
        for (name, duration) in [
            ("soft_deadline", config.soft_deadline),
            ("work_timeout", config.work_timeout),
            ("review_timeout", config.review_timeout),
        ] {
            if duration.is_zero() {
                return Err(KernelError::ZeroDuration(name));
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
            record: Vec::new(),
            jobs: BTreeMap::new(),
            seen_messages: BTreeMap::new(),
            pending: Vec::new(),
            work: None,
            candidate: None,
            review: None,
            requirement: None,
            autonomous: false,
            generation: 0,
            revision: 0,
            input_boundary: 0,
            need_work: false,
            wakes: BTreeMap::new(),
            next_job: 1,
            next_wake: 1,
        })
    }

    pub fn record_cursor(&self) -> u64 {
        self.record.len() as u64
    }

    pub(crate) fn records_since(&self, cursor: u64) -> &[RecordEntry] {
        &self.record[cursor as usize..]
    }

    pub(crate) fn job(&self, id: JobId) -> &JobSnapshot {
        self.jobs.get(&id).expect("registered job")
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            record_cursor: self.record_cursor(),
            revision: self.revision,
            generation: self.generation,
            requirement: self.requirement.clone(),
            autonomous: self.autonomous,
            work: self.work.as_ref().map(|request| request.id),
            review: self.review.as_ref().map(|request| request.id),
            candidate: self
                .candidate
                .as_ref()
                .map(|candidate| candidate.request.id),
            pending_messages: self.pending.clone(),
            jobs: self.jobs.values().cloned().collect(),
            record: self.record.clone(),
            tools: self.tools.values().cloned().collect(),
        }
    }

    /// Receive one event, complete its state transition, then request I/O.
    pub fn step(&mut self, event: Event) -> Vec<Effect> {
        let mut effects = Vec::new();
        match event {
            Event::UserMessage { id, text } => {
                if !self.seen_messages.contains_key(&id) {
                    let message = Message { id, text };
                    self.append(RecordKind::UserMessage(message.clone()));
                    self.seen_messages.insert(id, self.record_cursor());
                    self.pending.push(message);
                    // A busy Work needs classification, not an automatic extra turn.
                    if !self.has_current_work() && self.candidate.is_none() {
                        self.need_work = true;
                    }
                }
            }
            Event::JobFinished { id, outcome } => self.finish_job(id, outcome, &mut effects),
            Event::JobProgress { id, progress } => {
                if let Some(job) = self.jobs.get_mut(&id)
                    && job.is_running()
                    && job.progress.as_ref() != Some(&progress)
                {
                    job.progress = Some(progress.clone());
                    self.publish(Notice::JobProgress { id, progress }, &mut effects);
                }
            }
            Event::Wake { id } => self.wake(id),
            Event::Stop => {
                self.pause_at(self.record_cursor(), &mut effects);
                self.publish(Notice::Stopped, &mut effects);
            }
        }
        self.advance(&mut effects);
        effects
    }

    fn finish_job(&mut self, id: JobId, mut outcome: JobOutcome, effects: &mut Vec<Effect>) {
        let Some(job) = self.jobs.get(&id) else {
            return;
        };
        // Unknown writes can later be reconciled. All known completions are final.
        if let JobState::Finished(previous) = &job.state
            && (previous.external_effect != ExternalEffect::Unknown
                || outcome.external_effect == ExternalEffect::Unknown)
        {
            return;
        }
        let request = job.request.clone();
        let current = job.generation == self.generation && job.state == JobState::Running;
        if !job.external_write && outcome.external_effect != ExternalEffect::None {
            outcome.external_effect = ExternalEffect::None;
            if outcome.result.is_ok() {
                outcome.result = Err(JobError::new("a read-only job reported an external effect"));
            }
        }
        self.jobs.get_mut(&id).expect("registered job").state = JobState::Finished(outcome.clone());
        self.cancel_wakes(Some(id), effects);
        self.publish(
            Notice::JobFinished {
                id,
                outcome: outcome.clone(),
            },
            effects,
        );

        match request {
            JobRequest::Work { .. } => {
                if self.work.as_ref().is_some_and(|work| work.id == id) {
                    let work = self.work.take().expect("matching work request");
                    if current {
                        match outcome.result {
                            Ok(JobOutput::Work(result)) => {
                                self.candidate = Some(Candidate {
                                    request: work,
                                    result,
                                });
                                self.record_hold();
                            }
                            Ok(_) => self.model_failed(
                                work.messages,
                                "expected a Work result".into(),
                                effects,
                            ),
                            Err(error) => {
                                self.model_failed(work.messages, error.to_string(), effects)
                            }
                        }
                    } else {
                        self.discard(
                            id,
                            "work request was revoked; its result is retained as material",
                        );
                    }
                }
            }
            JobRequest::ReviewInput { .. } => {
                if self.review.as_ref().is_some_and(|review| review.id == id) {
                    let review = self.review.take().expect("matching input review");
                    if current {
                        match outcome.result {
                            Ok(JobOutput::InputReview(result)) => {
                                self.finish_review(review, result, effects)
                            }
                            Ok(_) => self.model_failed(
                                review.messages,
                                "expected an InputReview result".into(),
                                effects,
                            ),
                            Err(error) => {
                                self.model_failed(review.messages, error.to_string(), effects)
                            }
                        }
                    } else {
                        self.discard(
                            id,
                            "input review was revoked; its result cannot change the session",
                        );
                    }
                }
            }
            JobRequest::Tool(_) => {
                self.revision += 1;
                if self.autonomous || self.has_current_work() || self.candidate.is_some() {
                    self.need_work = true;
                }
            }
        }
    }

    fn finish_review(&mut self, review: Request, result: InputReview, effects: &mut Vec<Effect>) {
        let messages = message_ids(&review.messages);
        self.append(RecordKind::InputReviewed {
            job: review.id,
            messages: messages.clone(),
            disposition: result.disposition,
            note: result.note,
        });
        if let Some(text) = result.reply {
            self.publish(
                Notice::Reply {
                    text,
                    reply_to: messages.clone(),
                    as_of: review.as_of,
                },
                effects,
            );
        }
        match result.disposition {
            InputDisposition::Keep => {
                self.append(RecordKind::InputsHandled {
                    job: review.id,
                    messages,
                });
            }
            InputDisposition::Reconsider => {
                // Classification is not consumption: the solver must receive the original words.
                self.revision += 1;
                self.return_messages(review.messages);
                self.revoke_work(effects);
                self.need_work = true;
            }
            InputDisposition::Pause => {
                self.append(RecordKind::InputsHandled {
                    job: review.id,
                    messages,
                });
                let through = review
                    .messages
                    .iter()
                    .map(|message| self.seen_messages[&message.id])
                    .max()
                    .expect("review owns input");
                self.pause_at(through, effects);
                self.publish(Notice::Paused, effects);
            }
        }
    }

    fn has_current_work(&self) -> bool {
        self.work.as_ref().is_some_and(|work| {
            work.generation == self.generation && self.jobs[&work.id].state == JobState::Running
        })
    }

    /// The scheduling policy: solver when free, input review only while a solver proposal exists.
    fn advance(&mut self, effects: &mut Vec<Effect>) {
        if self.review.is_some() {
            return;
        }
        if self.work.is_some() {
            if self.has_current_work() && !self.pending.is_empty() {
                self.start_review(effects);
            }
            return;
        }
        if self.candidate.is_some() && !self.pending.is_empty() {
            self.start_review(effects);
            return;
        }
        if let Some(candidate) = self.candidate.take() {
            self.commit_work(candidate, effects);
        }
        if self.need_work && (self.autonomous || !self.pending.is_empty()) {
            self.start_work(effects);
        }
    }

    fn record_hold(&mut self) {
        let Some(candidate) = &self.candidate else {
            return;
        };
        let mut messages = self
            .review
            .as_ref()
            .map(|review| message_ids(&review.messages))
            .unwrap_or_default();
        messages.extend(message_ids(&self.pending));
        if !messages.is_empty() {
            self.append(RecordKind::WorkHeld {
                job: candidate.request.id,
                messages,
            });
        }
    }

    fn commit_work(&mut self, candidate: Candidate, effects: &mut Vec<Effect>) {
        let Candidate { request, result } = candidate;
        if request.generation != self.generation || request.revision != self.revision {
            self.discard(
                request.id,
                "new input or a substantive result changed the work basis",
            );
            self.return_messages(request.messages);
            self.need_work = true;
            return;
        }
        let running = match result.autonomy {
            Autonomy::Keep => self.autonomous,
            Autonomy::Run => true,
            Autonomy::Pause => false,
        };
        if let Err(reason) = self.validate(&request, &result, running) {
            self.discard(request.id, &reason);
            self.model_failed(request.messages, reason, effects);
            return;
        }
        if let Some(Operation::Tool(call)) = &result.operation
            && self.tools[&call.name].effect == ToolEffect::ExternalWrite
            && self.unresolved_write()
        {
            self.discard(
                request.id,
                "an external write is still unresolved; choose a different next step",
            );
            self.return_messages(request.messages);
            self.need_work = true;
            return;
        }

        // Only now may any part of the proposal change requirements or reach the user.
        if !request.messages.is_empty() {
            self.append(RecordKind::InputsHandled {
                job: request.id,
                messages: message_ids(&request.messages),
            });
        }
        if let Some(requirement) = result.requirement
            && self.requirement.as_ref() != Some(&requirement)
        {
            self.requirement = Some(requirement.clone());
            self.revision += 1;
            self.append(RecordKind::RequirementUpdated {
                job: request.id,
                text: requirement,
            });
        }
        self.append(RecordKind::PlanAccepted { job: request.id });
        self.autonomous = running;
        if result.autonomy == Autonomy::Pause {
            self.need_work = false;
            self.cancel_wakes(None, effects);
        }
        if let Some(text) = result.reply {
            self.publish(
                Notice::Reply {
                    text,
                    reply_to: message_ids(&request.messages),
                    as_of: request.as_of,
                },
                effects,
            );
        }
        match result.operation {
            Some(Operation::Tool(call)) => {
                self.start_job(Call::Tool(call), effects);
            }
            Some(Operation::Cancel(id)) => self.cancel_job(id, effects),
            None => {}
        }
        // Terminal consumers may stop reading at Paused. Deliver the accepted
        // reply first, and let Finish produce the only terminal notice.
        if result.autonomy == Autonomy::Pause && result.next != Next::Finish {
            self.publish(Notice::Paused, effects);
        }
        match result.next {
            Next::Continue => self.need_work = self.autonomous,
            Next::Wait { reconsider_after } => {
                if self.autonomous
                    && let Some(delay) = reconsider_after
                {
                    self.schedule_wake(None, delay, effects);
                }
            }
            Next::Finish => {
                self.autonomous = false;
                self.need_work = false;
                self.cancel_all_wakes(effects);
                let cleanup = self
                    .jobs
                    .values()
                    .filter(|job| {
                        matches!(job.request, JobRequest::Tool(_))
                            && !job.external_write
                            && job.is_running()
                    })
                    .map(|job| job.id)
                    .collect::<Vec<_>>();
                for id in &cleanup {
                    self.cancel_job(*id, effects);
                }
                self.publish(Notice::Finished { cleanup }, effects);
            }
        }
    }

    fn validate(
        &self,
        request: &Request,
        result: &WorkResult,
        running: bool,
    ) -> Result<(), String> {
        if result.requirement.is_some() && request.messages.is_empty() {
            return Err("only a Work input batch can update the requirement".into());
        }
        if result.autonomy == Autonomy::Run && !self.autonomous && request.messages.is_empty() {
            return Err("resuming autonomous work requires new user input".into());
        }
        if result.next == Next::Finish {
            if matches!(result.operation, Some(Operation::Tool(_))) {
                return Err("Finish cannot start a new tool".into());
            }
            if self.unresolved_write() {
                return Err("Finish cannot declare an unresolved write complete".into());
            }
        }
        if result.next == Next::Continue && !running {
            return Err("Continue requires autonomous work to be enabled".into());
        }
        if let Next::Wait {
            reconsider_after: Some(delay),
        } = result.next
            && delay.is_zero()
        {
            return Err("reconsider_after must be greater than zero".into());
        }
        match &result.operation {
            Some(Operation::Tool(call)) => {
                if !self.tools.contains_key(&call.name) {
                    return Err(format!("tool is not registered: {}", call.name));
                }
                if !running {
                    return Err("starting a tool requires autonomous work to be enabled".into());
                }
            }
            Some(Operation::Cancel(id)) => {
                let Some(job) = self.jobs.get(id) else {
                    return Err(format!("job {} is not registered", id.0));
                };
                if !job.is_running() {
                    return Err(if job.is_unresolved() {
                        "the local invocation has ended; query and confirm its external outcome"
                            .into()
                    } else {
                        format!("job {} has already finished", id.0)
                    });
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn unresolved_write(&self) -> bool {
        self.jobs
            .values()
            .any(|job| job.external_write && job.is_unresolved())
    }

    /// Preserve unconsumed input, but require another user event before retrying a failure.
    fn model_failed(&mut self, messages: Vec<Message>, error: String, effects: &mut Vec<Effect>) {
        self.return_messages(messages);
        self.generation += 1;
        self.revoke_work(effects);
        if let Some(review) = &mut self.review {
            let id = review.id;
            let messages = std::mem::take(&mut review.messages);
            self.return_messages(messages);
            self.cancel_job(id, effects);
        }
        self.autonomous = false;
        self.need_work = false;
        self.cancel_wakes(None, effects);
        self.publish(Notice::Error { message: error }, effects);
        self.publish(Notice::Paused, effects);
    }

    fn revoke_work(&mut self, effects: &mut Vec<Effect>) {
        if let Some(work) = &mut self.work {
            let id = work.id;
            let messages = std::mem::take(&mut work.messages);
            self.return_messages(messages);
            self.cancel_job(id, effects);
        }
        if let Some(candidate) = self.candidate.take() {
            self.discard(
                candidate.request.id,
                "work proposal was revoked; its result is retained as material",
            );
            self.return_messages(candidate.request.messages);
        }
    }

    fn return_messages(&mut self, messages: Vec<Message>) {
        self.pending.extend(messages);
        self.pending
            .retain(|message| self.seen_messages[&message.id] > self.input_boundary);
        self.pending
            .sort_by_key(|message| self.seen_messages[&message.id]);
        self.pending.dedup_by_key(|message| message.id);
    }

    fn pause_at(&mut self, through: u64, effects: &mut Vec<Effect>) {
        self.input_boundary = self.input_boundary.max(through);
        self.generation += 1;
        self.revision += 1;
        self.autonomous = false;
        self.revoke_work(effects);
        if let Some(review) = &mut self.review {
            let messages = std::mem::take(&mut review.messages);
            self.return_messages(messages);
        }
        self.return_messages(Vec::new());
        self.need_work = !self.pending.is_empty();
        self.cancel_all_wakes(effects);
        for id in self.jobs.keys().copied().collect::<Vec<_>>() {
            self.cancel_job(id, effects);
        }
    }

    fn start_work(&mut self, effects: &mut Vec<Effect>) {
        self.need_work = false;
        self.cancel_wakes(None, effects);
        let messages = std::mem::take(&mut self.pending);
        self.work = Some(self.start_model(ModelTask::Work { messages }, effects));
    }

    fn start_review(&mut self, effects: &mut Vec<Effect>) {
        let messages = std::mem::take(&mut self.pending);
        self.review = Some(self.start_model(ModelTask::ReviewInput { messages }, effects));
    }

    fn start_model(&mut self, task: ModelTask, effects: &mut Vec<Effect>) -> Request {
        let messages = match &task {
            ModelTask::Work { messages } | ModelTask::ReviewInput { messages } => messages.clone(),
        };
        let snapshot = self.snapshot();
        let as_of = snapshot.record_cursor;
        let revision = snapshot.revision;
        let generation = snapshot.generation;
        let id = self.start_job(Call::Model(ModelInput { task, snapshot }), effects);
        Request {
            id,
            messages,
            generation,
            revision,
            as_of,
        }
    }

    fn start_job(&mut self, call: Call, effects: &mut Vec<Effect>) -> JobId {
        let id = JobId(self.next_job);
        self.next_job = self
            .next_job
            .checked_add(1)
            .expect("job identifier space exhausted");
        let (request, external_write, timeout, as_of) = match &call {
            Call::Model(input) => {
                let (request, timeout) = match &input.task {
                    ModelTask::Work { messages } => (
                        JobRequest::Work {
                            messages: message_ids(messages),
                        },
                        self.config.work_timeout,
                    ),
                    ModelTask::ReviewInput { messages } => (
                        JobRequest::ReviewInput {
                            messages: message_ids(messages),
                        },
                        self.config.review_timeout,
                    ),
                };
                (request, false, Some(timeout), input.snapshot.record_cursor)
            }
            Call::Tool(call) => (
                JobRequest::Tool(call.clone()),
                self.tools[&call.name].effect == ToolEffect::ExternalWrite,
                None,
                self.record_cursor(),
            ),
        };
        let tool = matches!(request, JobRequest::Tool(_));
        self.jobs.insert(
            id,
            JobSnapshot {
                id,
                request: request.clone(),
                record_cursor: as_of,
                revision: self.revision,
                generation: self.generation,
                external_write,
                state: JobState::Running,
                progress: None,
            },
        );
        self.publish(Notice::JobStarted { id, request }, effects);
        effects.push(Effect::Start { id, call, timeout });
        if tool {
            self.schedule_wake(Some(id), self.config.soft_deadline, effects);
        }
        id
    }

    fn schedule_wake(&mut self, job: Option<JobId>, delay: Duration, effects: &mut Vec<Effect>) {
        let id = WakeId(self.next_wake);
        self.next_wake = self
            .next_wake
            .checked_add(1)
            .expect("wake identifier space exhausted");
        self.wakes.insert(id, job);
        effects.push(Effect::WakeAfter { id, delay });
    }

    fn wake(&mut self, id: WakeId) {
        if let Some(job) = self.wakes.remove(&id)
            && (self.autonomous || self.has_current_work() || self.candidate.is_some())
        {
            self.append(RecordKind::Reminder { id, job });
            self.need_work = true;
        }
    }

    fn cancel_wakes(&mut self, job: Option<JobId>, effects: &mut Vec<Effect>) {
        let ids = self
            .wakes
            .iter()
            .filter_map(|(id, owner)| (*owner == job).then_some(*id))
            .collect::<Vec<_>>();
        for id in ids {
            self.wakes.remove(&id);
            effects.push(Effect::CancelWake { id });
        }
    }

    fn cancel_all_wakes(&mut self, effects: &mut Vec<Effect>) {
        for (id, _) in std::mem::take(&mut self.wakes) {
            effects.push(Effect::CancelWake { id });
        }
    }

    fn cancel_job(&mut self, id: JobId, effects: &mut Vec<Effect>) {
        if let Some(job) = self.jobs.get_mut(&id)
            && job.state == JobState::Running
        {
            job.state = JobState::CancelRequested;
            self.append(RecordKind::CancellationRequested { job: id });
            effects.push(Effect::RequestCancel { id });
        }
    }

    fn discard(&mut self, job: JobId, reason: &str) {
        self.append(RecordKind::PlanDiscarded {
            job,
            reason: reason.into(),
        });
    }

    fn publish(&mut self, notice: Notice, effects: &mut Vec<Effect>) {
        self.append(RecordKind::Notice(notice.clone()));
        effects.push(Effect::Publish(notice));
    }

    fn append(&mut self, kind: RecordKind) {
        self.record.push(RecordEntry {
            cursor: self.record_cursor() + 1,
            kind,
        });
    }
}

fn message_ids(messages: &[Message]) -> Vec<MessageId> {
    messages.iter().map(|message| message.id).collect()
}
