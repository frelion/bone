use super::*;

impl Kernel {
    pub(super) fn enqueue_routing(&mut self, routing: Seq) {
        if !self.routing_ready.contains(&routing) {
            self.routing_ready.push_back(routing);
        }
    }

    pub(super) fn enqueue_job(&mut self, job: JobId) {
        let Some(entry) = self.jobs.get(&job) else {
            return;
        };
        if matches!(entry.state, JobState::Finished(_)) {
            return;
        }
        let queue = if matches!(entry.owner, Owner::User) {
            &mut self.interactive_ready
        } else {
            &mut self.background_ready
        };
        if !queue.contains(&job) {
            queue.push_back(job);
        }
    }

    pub(super) fn make_ready(&mut self, job: JobId) {
        let Some(entry) = self.jobs.get_mut(&job) else {
            return;
        };
        if matches!(entry.state, JobState::Finished(_)) {
            return;
        }
        entry.state = JobState::Ready;
        self.enqueue_job(job);
    }

    pub(super) fn make_routing_ready(&mut self, routing: Seq) {
        self.routings
            .get_mut(&routing)
            .expect("routing exists")
            .state = RoutingState::Ready;
        self.enqueue_routing(routing);
    }

    pub(super) fn advance(&mut self, now: MonoTime, effects: &mut Vec<Effect>) {
        if !self.suspended {
            self.process_candidates(now, effects);
            self.schedule_routing(effects);
            self.schedule_workers(effects);
        }
        self.finish_inputs(effects);
    }

    pub(super) fn expire(&mut self, now: MonoTime, effects: &mut Vec<Effect>) {
        let timers = self
            .jobs
            .iter()
            .filter_map(|(id, job)| match job.state {
                JobState::Waiting(WaitState::Until(deadline)) if deadline <= now => Some(*id),
                _ => None,
            })
            .collect::<Vec<_>>();
        for job in timers {
            self.make_ready(job);
        }

        let expired = self
            .inquiries
            .iter()
            .filter_map(|(id, inquiry)| (inquiry.deadline <= now).then_some(*id))
            .collect::<Vec<_>>();
        for inquiry in expired {
            self.settle_inquiry(inquiry, InquiryResult::TimedOut, effects);
        }
    }

    fn process_candidates(&mut self, now: MonoTime, effects: &mut Vec<Effect>) {
        let candidates = self
            .jobs
            .iter()
            .filter_map(|(id, job)| match &job.state {
                JobState::Waiting(WaitState::Commit(pending)) => Some((*id, pending.clone())),
                _ => None,
            })
            .collect::<Vec<_>>();
        for (job, pending) in candidates {
            if let Some(entry) = self.jobs.get_mut(&job) {
                entry.state = JobState::Ready;
            }
            self.try_step(now, job, pending, effects);
        }
    }

    fn schedule_routing(&mut self, effects: &mut Vec<Effect>) {
        if self
            .calls
            .values()
            .any(|call| call.kind() == CallKind::Coordinate && call.running())
        {
            return;
        }
        let scans = self.routing_ready.len();
        for _ in 0..scans {
            let Some(routing) = self.routing_ready.pop_front() else {
                return;
            };
            let ready = self.routings.get(&routing).is_some_and(|entry| {
                entry.state == RoutingState::Ready && entry.active_call.is_none()
            });
            if !ready {
                continue;
            }
            match context::prepare_coordinate(self, routing) {
                Ok(input) => {
                    let call = self.start_coordination(input, effects);
                    self.routings
                        .get_mut(&routing)
                        .expect("routing exists")
                        .active_call = Some(call);
                    return;
                }
                Err(error) => {
                    if context::session_can_help_coordinate(self, routing)
                        && self.session_compacting()
                    {
                        self.enqueue_routing(routing);
                        return;
                    }
                    if context::session_can_help_coordinate(self, routing)
                        && let Ok(input) = context::prepare_session_compact(self)
                    {
                        self.start_session_compaction(
                            input,
                            DeliveryTarget::Routing(routing),
                            effects,
                        );
                        self.enqueue_routing(routing);
                        return;
                    }
                    self.fail_routing(routing, error.to_string(), effects);
                }
            }
        }
    }

    fn schedule_workers(&mut self, effects: &mut Vec<Effect>) {
        let mut deferred = Vec::new();
        while self.running_workers() < self.limits.worker_slots() {
            let job = self.take_runnable(true).or_else(|| {
                (self.running_background_workers() < self.limits.background_workers)
                    .then(|| self.take_runnable(false))
                    .flatten()
            });
            let Some(job) = job else {
                break;
            };
            let prepared = match context::prepare_work(self, job) {
                Ok(prepared) => prepared,
                Err(error) => {
                    self.finish_job(
                        job,
                        OutcomeKind::Failed,
                        Completion::new(format!("cannot prepare context: {error}")),
                        effects,
                    );
                    continue;
                }
            };
            match prepared {
                PreparedWork::Work {
                    input,
                    seen_through,
                } => {
                    let call = self.start_work(*input, seen_through, effects);
                    self.jobs.get_mut(&job).expect("job exists").active_call = Some(call);
                }
                PreparedWork::Compact(input) => {
                    if input.scope == crate::CompactScope::Session {
                        if !self.session_compacting() {
                            self.start_session_compaction(input, DeliveryTarget::Job(job), effects);
                        }
                        deferred.push(job);
                        continue;
                    }
                    let call = self.start_compaction(input, effects);
                    self.jobs.get_mut(&job).expect("job exists").active_call = Some(call);
                }
            }
        }
        for job in deferred {
            self.enqueue_job(job);
        }
    }

    fn start_coordination(
        &mut self,
        input: crate::CoordinateInput,
        effects: &mut Vec<Effect>,
    ) -> CallId {
        let routing = input.routing;
        self.start_call(
            CallTask::Coordinate { routing },
            Call::Coordinate(input),
            self.limits.coordination_timeout,
            effects,
        )
    }

    fn start_work(
        &mut self,
        input: crate::WorkInput,
        seen_through: Seq,
        effects: &mut Vec<Effect>,
    ) -> CallId {
        let task = CallTask::Work {
            job: input.job,
            revision: input.revision,
            seen_through,
            inquiries: input.inquiries.iter().map(|item| item.id).collect(),
        };
        self.start_call(task, Call::Work(input), self.limits.work_timeout, effects)
    }

    fn start_compaction(
        &mut self,
        input: crate::CompactInput,
        effects: &mut Vec<Effect>,
    ) -> CallId {
        let crate::CompactScope::Job { job, revision } = input.scope else {
            unreachable!()
        };
        let source_bytes = serde_json::to_vec(&input.records)
            .expect("records serialize")
            .len()
            + input
                .previous
                .as_ref()
                .map_or(0, |previous| previous.summary.len());
        let task = CallTask::Compact {
            job,
            revision,
            through: input.through,
            source_bytes,
        };
        self.start_call(
            task,
            Call::Compact(input),
            self.limits.work_timeout,
            effects,
        )
    }

    fn session_compacting(&self) -> bool {
        self.calls
            .values()
            .any(|call| call.running() && matches!(call.task, CallTask::SessionCompact { .. }))
    }

    fn start_session_compaction(
        &mut self,
        input: crate::CompactInput,
        requester: DeliveryTarget,
        effects: &mut Vec<Effect>,
    ) {
        let source_bytes = serde_json::to_vec(&input.records)
            .expect("records serialize")
            .len()
            + input
                .previous
                .as_ref()
                .map_or(0, |previous| previous.summary.len());
        let task = CallTask::SessionCompact {
            requester,
            through: input.through,
            previous_through: input
                .previous
                .as_ref()
                .map_or(Seq::ZERO, |previous| previous.through),
            source_bytes,
        };
        self.start_call(
            task,
            Call::Compact(input),
            self.limits.work_timeout,
            effects,
        );
    }

    pub(super) fn start_call(
        &mut self,
        task: CallTask,
        call: Call,
        timeout: std::time::Duration,
        effects: &mut Vec<Effect>,
    ) -> CallId {
        let id = CallId(self.next_call);
        self.next_call += 1;
        let entry = CallEntry {
            task,
            state: CallState::Running,
            progress: None,
        };
        let kind = entry.kind();
        let job = entry.job();
        let tool = entry.tool_request().cloned();
        self.calls.insert(id, entry);
        self.record(
            Origin::Call(id),
            RecordBody::CallStarted {
                call: id,
                kind,
                job,
                tool,
            },
            effects,
        );
        effects.push(Effect::Start {
            id,
            call: Box::new(call),
            timeout,
        });
        id
    }

    fn take_runnable(&mut self, interactive: bool) -> Option<JobId> {
        let scans = if interactive {
            self.interactive_ready.len()
        } else {
            self.background_ready.len()
        };
        for _ in 0..scans {
            let job = if interactive {
                self.interactive_ready.pop_front()
            } else {
                self.background_ready.pop_front()
            }?;
            if self.runnable(job) {
                return Some(job);
            }
        }
        None
    }

    fn runnable(&self, id: JobId) -> bool {
        let Some(job) = self.jobs.get(&id) else {
            return false;
        };
        if matches!(
            job.state,
            JobState::Finished(_) | JobState::Waiting(WaitState::Commit(_))
        ) || job.active_call.is_some()
            || self.is_paused(id)
        {
            return false;
        }
        if let JobState::Waiting(WaitState::Jobs(jobs)) = &job.state
            && !self.job_group_ready(jobs)
        {
            // Keep partial success unread for eventual review, but do not call
            // the model merely to acknowledge it. Other required facts interrupt.
            return job.context.records.iter().any(|seq| {
                if *seq <= job.context.read_through {
                    return false;
                }
                match self.records.get(seq).map(|record| &record.body) {
                    Some(RecordBody::Delivery {
                        source,
                        kind: DeliveryKind::ChildCreated,
                        ..
                    }) => !matches!(self.records.get(source).map(|record| &record.body),
                            Some(RecordBody::JobCreated { job, .. }) if jobs.contains(job)),
                    Some(RecordBody::Delivery {
                        source,
                        kind: DeliveryKind::Outcome,
                        ..
                    }) => !matches!(self.records.get(source).map(|record| &record.body),
                            Some(RecordBody::Outcome { job, outcome })
                            if jobs.contains(job) && outcome.kind == OutcomeKind::Completed),
                    Some(
                        RecordBody::Delivery { .. }
                        | RecordBody::ReadResult { .. }
                        | RecordBody::ToolFinished { .. }
                        | RecordBody::ImportedMemory { .. }
                        | RecordBody::WorkRejected { .. },
                    ) => true,
                    _ => false,
                }
            });
        }
        matches!(job.state, JobState::Ready) || self.has_unread_required(id)
    }

    fn running_workers(&self) -> usize {
        self.calls
            .values()
            .filter(|call| {
                matches!(call.kind(), CallKind::Work | CallKind::Compact) && call.running()
            })
            .count()
    }

    fn running_background_workers(&self) -> usize {
        self.calls
            .values()
            .filter(|call| {
                matches!(call.kind(), CallKind::Work | CallKind::Compact)
                    && call.running()
                    && call
                        .job()
                        .is_some_and(|job| !matches!(self.jobs[&job].owner, Owner::User))
            })
            .count()
    }

    pub(super) fn has_unread_required(&self, id: JobId) -> bool {
        let job = &self.jobs[&id];
        job.context.records.iter().any(|seq| {
            *seq > job.context.read_through
                && self.records.get(seq).is_some_and(|record| {
                    matches!(
                        record.body,
                        RecordBody::Delivery { .. }
                            | RecordBody::WorkRejected { .. }
                            | RecordBody::ReadResult { .. }
                            | RecordBody::ToolFinished { .. }
                            | RecordBody::ImportedMemory { .. }
                    )
                })
        })
    }

    pub(super) fn is_paused(&self, id: JobId) -> bool {
        let mut current = Some(id);
        while let Some(job) = current {
            let entry = &self.jobs[&job];
            if entry.local_paused {
                return true;
            }
            current = match entry.owner {
                Owner::Job(parent) => Some(parent),
                Owner::User | Owner::Routing(_) => None,
            };
        }
        false
    }

    pub(super) fn coordinate_finished(
        &mut self,
        now: MonoTime,
        call: CallId,
        result: Result<KernelDecision, CallError>,
        effects: &mut Vec<Effect>,
    ) {
        let Some(routing) = self.finish_routing_call(call, result.as_ref().err().cloned(), effects)
        else {
            return;
        };
        match result {
            Ok(decision) => {
                if let Err(message) = self.apply_decision(now, routing, decision, effects) {
                    self.fail_routing(routing, message, effects);
                }
            }
            Err(error) => self.fail_routing(routing, error.message, effects),
        }
    }

    pub(super) fn work_finished(
        &mut self,
        now: MonoTime,
        call: CallId,
        result: Result<WorkProposal, CallError>,
        effects: &mut Vec<Effect>,
    ) {
        let Some((job, entry)) = self.finish_job_call(
            call,
            CallKind::Work,
            result.as_ref().err().cloned(),
            effects,
        ) else {
            return;
        };
        match result {
            Ok(proposal) => self.accept_work(now, job, call, &entry, proposal, effects),
            Err(error) => self.finish_job(
                job,
                OutcomeKind::Failed,
                Completion::new(error.message),
                effects,
            ),
        }
    }

    pub(super) fn compact_finished(
        &mut self,
        call: CallId,
        result: Result<crate::CheckpointDraft, CallError>,
        effects: &mut Vec<Effect>,
    ) {
        if let Some(entry) = self.calls.get(&call).cloned()
            && let CallTask::SessionCompact {
                requester,
                through,
                previous_through,
                source_bytes,
            } = entry.task
        {
            if !entry.running() {
                return;
            }
            let eligible = matches!(entry.state, CallState::Running);
            self.finish_model_call(call, result.as_ref().err().cloned(), effects);
            if !eligible {
                self.discard_model_result::<()>(call, effects);
                return;
            }
            if context::session_checkpoint(self)
                .as_ref()
                .map_or(Seq::ZERO, |cp| cp.through)
                != previous_through
            {
                return;
            }
            let result = result.and_then(|draft| {
                let bytes = serde_json::to_vec(&draft).expect("draft serializes").len();
                if draft.summary.trim().is_empty()
                    || bytes > context::compact_output_bytes(self)
                    || bytes >= source_bytes
                    || !draft.evidence.iter().all(|seq| {
                        *seq <= through
                            && self
                                .records
                                .get(seq)
                                .is_some_and(|record| context::session_public(self, record))
                    })
                {
                    Err(CallError::failed(
                        "session checkpoint invalid or made no compression progress",
                    ))
                } else {
                    Ok(draft)
                }
            });
            match result {
                Ok(draft) => {
                    self.record(
                        Origin::Call(call),
                        RecordBody::SessionCheckpoint {
                            checkpoint: Arc::new(crate::SessionCheckpoint {
                                through,
                                summary: draft.summary,
                                evidence: draft.evidence,
                            }),
                        },
                        effects,
                    );
                }
                Err(error) => match requester {
                    DeliveryTarget::Routing(routing) => {
                        if self
                            .routings
                            .get(&routing)
                            .is_some_and(|route| route.state != RoutingState::Closed)
                        {
                            self.fail_routing(routing, error.message, effects);
                        }
                    }
                    DeliveryTarget::Job(job) => self.finish_job(
                        job,
                        OutcomeKind::Failed,
                        Completion::new(error.message),
                        effects,
                    ),
                },
            }
            return;
        }
        let Some((job, entry)) = self.finish_job_call(
            call,
            CallKind::Compact,
            result.as_ref().err().cloned(),
            effects,
        ) else {
            return;
        };
        match result {
            Ok(draft) => self.accept_checkpoint(job, call, &entry, draft, effects),
            Err(error) => self.finish_job(
                job,
                OutcomeKind::Failed,
                Completion::new(error.message),
                effects,
            ),
        }
    }

    fn finish_routing_call(
        &mut self,
        call: CallId,
        error: Option<CallError>,
        effects: &mut Vec<Effect>,
    ) -> Option<Seq> {
        let entry = self.calls.get(&call)?.clone();
        let CallTask::Coordinate { routing } = &entry.task else {
            return None;
        };
        let routing = *routing;
        if !entry.running() {
            return None;
        }
        let eligible = self
            .routings
            .get(&routing)
            .is_some_and(|route| route.active_call == Some(call));
        self.finish_model_call(call, error, effects);
        if let Some(route) = self.routings.get_mut(&routing)
            && route.active_call == Some(call)
        {
            route.active_call = None;
        }
        eligible
            .then_some(routing)
            .or_else(|| self.discard_model_result(call, effects))
    }

    fn finish_job_call(
        &mut self,
        call: CallId,
        kind: CallKind,
        error: Option<CallError>,
        effects: &mut Vec<Effect>,
    ) -> Option<(JobId, CallEntry)> {
        let entry = self.calls.get(&call)?.clone();
        let job = entry.job()?;
        if !entry.running() || entry.kind() != kind {
            return None;
        }
        let eligible = self.jobs.get(&job).is_some_and(|current| {
            current.active_call == Some(call)
                && current.revision == entry.revision()
                && !self.is_paused(job)
        });
        self.finish_model_call(call, error, effects);
        if let Some(current) = self.jobs.get_mut(&job)
            && current.active_call == Some(call)
        {
            current.active_call = None;
        }
        eligible
            .then_some((job, entry))
            .or_else(|| self.discard_model_result(call, effects))
    }

    fn discard_model_result<T>(&mut self, call: CallId, effects: &mut Vec<Effect>) -> Option<T> {
        self.record(
            Origin::Call(call),
            RecordBody::Audit {
                message: "discarded a model result after its execution authority changed".into(),
            },
            effects,
        );
        None
    }

    fn finish_model_call(
        &mut self,
        call: CallId,
        error: Option<CallError>,
        effects: &mut Vec<Effect>,
    ) {
        let Some(entry) = self.calls.get_mut(&call) else {
            return;
        };
        entry.state = CallState::ModelFinished(error.clone());
        entry.progress = None;
        self.record(
            Origin::Call(call),
            RecordBody::CallFinished {
                call,
                error,
                external_effect: ExternalEffect::None,
            },
            effects,
        );
    }

    fn accept_checkpoint(
        &mut self,
        job: JobId,
        call: CallId,
        call_entry: &CallEntry,
        draft: crate::CheckpointDraft,
        effects: &mut Vec<Effect>,
    ) {
        let (revision, through) = call_entry.compact_context();
        let previous = self.jobs[&job].context.checkpoint.as_ref();
        let after = previous.map_or(Seq::ZERO, |checkpoint| checkpoint.through);
        let source_bytes = match call_entry.task {
            CallTask::Compact { source_bytes, .. } => source_bytes,
            _ => unreachable!(),
        };
        let draft_bytes = serde_json::to_vec(&draft).expect("draft serializes").len();
        if through <= after
            || draft_bytes >= source_bytes
            || draft_bytes > context::compact_output_bytes(self)
            || draft.summary.trim().is_empty()
            || !draft
                .evidence
                .iter()
                .all(|seq| *seq <= through && self.can_read_record(job, *seq))
            || self.validate_model_item("checkpoint", &draft).is_err()
        {
            self.finish_job(
                job,
                OutcomeKind::Failed,
                Completion::new("invalid checkpoint"),
                effects,
            );
            return;
        }
        let checkpoint = Arc::new(crate::Checkpoint {
            job,
            revision,
            through,
            summary: draft.summary,
            evidence: draft.evidence,
        });
        self.record(
            Origin::Call(call),
            RecordBody::Checkpoint {
                job,
                checkpoint: checkpoint.clone(),
            },
            effects,
        );
        if let Some(entry) = self.jobs.get_mut(&job) {
            entry.context.checkpoint = Some(checkpoint);
        }
        self.make_ready(job);
    }

    pub(super) fn tool_finished(
        &mut self,
        call: CallId,
        result: ToolOutcome,
        effects: &mut Vec<Effect>,
    ) {
        let Some(entry) = self.calls.get(&call).cloned() else {
            return;
        };
        if !entry.running() {
            return;
        }
        let CallTask::Tool {
            job,
            request,
            output_bytes,
            ..
        } = entry.task
        else {
            return;
        };
        let result = Arc::new(Self::limit_tool_outcome(result, output_bytes));
        let call_entry = self.calls.get_mut(&call).expect("call exists");
        call_entry.state = CallState::ToolFinished(result.clone());
        call_entry.progress = None;
        let record = self.record(
            Origin::Call(call),
            RecordBody::ToolFinished {
                job,
                call,
                request,
                outcome: result,
            },
            effects,
        );
        self.attach(job, record.seq);
        if self.jobs.get(&job).is_some_and(|entry| {
            matches!(entry.state, JobState::Waiting(WaitState::Tool(waiting)) if waiting == call)
        }) {
            self.make_ready(job);
        } else {
            self.enqueue_job(job);
        }
    }

    fn limit_tool_outcome(outcome: ToolOutcome, output_bytes: usize) -> ToolOutcome {
        if serde_json::to_vec(&outcome).is_ok_and(|encoded| encoded.len() <= output_bytes) {
            return outcome;
        }
        ToolOutcome {
            result: Err(CallError::failed("tool outcome exceeds tool_output_bytes")),
            external_effect: outcome.external_effect,
        }
    }

    pub(super) fn write_resolved(
        &mut self,
        call: CallId,
        result: ToolOutcome,
        effects: &mut Vec<Effect>,
    ) -> bool {
        let Some(entry) = self.calls.get(&call).cloned() else {
            return false;
        };
        if entry.external_effect() != ExternalEffect::Unknown
            || result.external_effect == ExternalEffect::Unknown
        {
            return false;
        }
        let CallTask::Tool {
            job,
            request,
            output_bytes,
            revision: _,
            effect: _,
        } = entry.task
        else {
            return false;
        };
        let result = Arc::new(Self::limit_tool_outcome(result, output_bytes));
        let call_entry = self.calls.get_mut(&call).expect("call exists");
        call_entry.state = CallState::ToolFinished(result.clone());
        let record = self.record(
            Origin::Kernel,
            RecordBody::ToolFinished {
                job,
                call,
                request,
                outcome: result,
            },
            effects,
        );
        self.attach(job, record.seq);
        self.enqueue_job(job);
        true
    }

    pub(super) fn cancel_call(&mut self, call: CallId, effects: &mut Vec<Effect>) {
        if let Some(entry) = self.calls.get_mut(&call)
            && matches!(entry.state, CallState::Running)
        {
            entry.state = CallState::CancelRequested;
            effects.push(Effect::Cancel(call));
        }
    }
}
