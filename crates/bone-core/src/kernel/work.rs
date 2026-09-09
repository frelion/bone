use super::*;

impl Kernel {
    pub(super) fn accept_work(
        &mut self,
        now: MonoTime,
        job: JobId,
        call: CallId,
        call_entry: &CallEntry,
        proposal: WorkProposal,
        effects: &mut Vec<Effect>,
    ) {
        if let Err(message) = self.validate_proposal(job, call_entry, &proposal) {
            self.record(
                Origin::Call(call),
                RecordBody::Audit {
                    message: message.clone(),
                },
                effects,
            );
            self.finish_job(job, OutcomeKind::Failed, Completion::new(message), effects);
            return;
        }

        let (seen_through, _) = call_entry.work_context();
        if let Some(entry) = self.jobs.get_mut(&job) {
            entry.context.read_through = entry.context.read_through.max(seen_through);
        }
        if let Some(note) = proposal.note {
            let record = self.record(
                Origin::Call(call),
                RecordBody::Note { job, text: note },
                effects,
            );
            self.attach(job, record.seq);
        }
        if let Some(report) = proposal.report {
            let record = self.record(
                Origin::Call(call),
                RecordBody::Report { job, report },
                effects,
            );
            self.attach(job, record.seq);
            self.jobs.get_mut(&job).expect("job exists").report = Some(record.seq);
        }
        for answer in proposal.answers {
            if self.inquiries.contains_key(&answer.inquiry) {
                let result = match answer.response {
                    InquiryResponse::Answer(report) => InquiryResult::Answer(report),
                    InquiryResponse::NeedsWork(reason) => InquiryResult::NeedsWork(reason),
                    InquiryResponse::Unavailable(reason) => InquiryResult::Unavailable(reason),
                };
                self.settle_inquiry(answer.inquiry, result, effects);
            } else {
                self.record(
                    Origin::Call(call),
                    RecordBody::Audit {
                        message: format!("ignored a settled inquiry answer: {}", answer.inquiry),
                    },
                    effects,
                );
            }
        }
        self.try_step(
            now,
            job,
            PendingStep {
                call,
                step: proposal.step,
            },
            effects,
        );
    }

    fn validate_proposal(
        &self,
        job: JobId,
        call: &CallEntry,
        proposal: &WorkProposal,
    ) -> Result<(), String> {
        let (_, delivered_inquiries) = call.work_context();
        if proposal
            .note
            .as_ref()
            .is_some_and(|note| note.trim().is_empty())
        {
            return Err("work note is empty".into());
        }
        if let Some(note) = &proposal.note {
            self.validate_model_text("work note", note)?;
        }
        if let Some(report) = &proposal.report {
            self.validate_report(job, report)?;
        }
        for answer in &proposal.answers {
            self.validate_model_item("inquiry answer", answer)?;
            if !delivered_inquiries.contains(&answer.inquiry) {
                return Err(format!(
                    "inquiry {} was not delivered to this call",
                    answer.inquiry
                ));
            }
            if let InquiryResponse::Answer(report) = &answer.response {
                self.validate_report(job, report)?;
            }
            if let InquiryResponse::NeedsWork(reason) | InquiryResponse::Unavailable(reason) =
                &answer.response
            {
                self.validate_model_text("inquiry answer", reason)?;
            }
            if let Some(inquiry) = self.inquiries.get(&answer.inquiry)
                && inquiry.target != job
            {
                return Err(format!("inquiry {} belongs to another job", answer.inquiry));
            }
        }
        match &proposal.step {
            WorkStep::Continue => Ok(()),
            WorkStep::Tool(tool) => {
                if !tool.arguments.is_object() {
                    return Err("tool arguments must be an object".into());
                }
                self.validate_model_item("tool call", tool)?;
                let Some(spec) = self.tools.get(&tool.name) else {
                    return Err(format!("unknown tool: {}", tool.name));
                };
                if spec.effect == ToolEffect::ExternalWrite && self.is_investigation(job) {
                    return Err("routing investigations may use only read-only tools".into());
                }
                Ok(())
            }
            WorkStep::Delegate(assignments) => {
                self.validate_assignments(Some(job), &self.jobs[&job].inputs, assignments)
            }
            WorkStep::Wait(wait) => self.validate_wait(job, wait),
            WorkStep::AskUser(question) => {
                let has_reply_key = self.jobs[&job]
                    .inputs
                    .iter()
                    .any(|input| self.inputs[input].finished.is_none());
                if question.trim().is_empty()
                    || !matches!(self.jobs[&job].owner, Owner::User)
                    || !has_reply_key
                {
                    return Err("only a user-owned job may ask a non-empty user question".into());
                }
                self.validate_model_text("user question", question)?;
                Ok(())
            }
            WorkStep::Inquire {
                job: target,
                question,
            } => {
                if question.trim().is_empty()
                    || *target == job
                    || !self.can_access_job(job, *target)
                    || self.would_cycle(job, *target)
                {
                    return Err("invalid job inquiry".into());
                }
                self.validate_model_text("job inquiry", question)?;
                Ok(())
            }
            WorkStep::Coordinate(request) => {
                if request.trim().is_empty() {
                    Err("coordination request cannot be empty".into())
                } else {
                    self.validate_model_text("coordination request", request)
                }
            }
            WorkStep::Read(query) => self.validate_read(DeliveryTarget::Job(job), query),
            WorkStep::PublishResult(report) => self.validate_report(job, report),
            WorkStep::Reply(text) => {
                if text.trim().is_empty() || !matches!(self.jobs[&job].owner, Owner::User) {
                    Err("only a user-owned job may send a non-empty reply".into())
                } else {
                    self.validate_model_text("reply", text)
                }
            }
            WorkStep::Finish(completion) | WorkStep::Fail(completion) => {
                self.validate_completion(job, completion)
            }
        }
    }

    fn validate_report(&self, job: JobId, report: &ReportDraft) -> Result<(), String> {
        if report.summary.trim().is_empty() {
            return Err("report summary is empty".into());
        }
        self.validate_model_item("report", report)?;
        if report
            .evidence
            .iter()
            .all(|seq| self.can_read_record(job, *seq))
        {
            Ok(())
        } else {
            Err("report cites inaccessible evidence".into())
        }
    }

    fn validate_completion(&self, job: JobId, completion: &Completion) -> Result<(), String> {
        if completion.summary.trim().is_empty() {
            return Err("completion summary is empty".into());
        }
        self.validate_model_item("completion", completion)?;
        if completion
            .evidence
            .iter()
            .all(|seq| self.can_read_record(job, *seq))
        {
            Ok(())
        } else {
            Err("completion cites inaccessible evidence".into())
        }
    }

    fn validate_wait(&self, job: JobId, wait: &Await) -> Result<(), String> {
        match wait {
            Await::Tool(call) => {
                if self.calls.get(call).is_some_and(|entry| {
                    entry.kind() == CallKind::Tool && entry.job() == Some(job) && entry.running()
                }) {
                    Ok(())
                } else {
                    Err("job cannot wait for that tool call".into())
                }
            }
            Await::After(_) => Ok(()),
            Await::Job(target) | Await::Result { job: target, .. } => {
                if *target != job
                    && self.can_access_job(job, *target)
                    && !self.would_cycle(job, *target)
                {
                    Ok(())
                } else {
                    Err("invalid job dependency".into())
                }
            }
        }
    }

    pub(super) fn try_step(
        &mut self,
        now: MonoTime,
        job: JobId,
        pending: PendingStep,
        effects: &mut Vec<Effect>,
    ) {
        let Some(entry) = self.jobs.get(&job) else {
            return;
        };
        if matches!(entry.state, JobState::Finished(_)) {
            return;
        }
        if self.calls[&pending.call].revision() != entry.revision {
            self.make_ready(job);
            return;
        }
        if self.is_paused(job) {
            self.jobs.get_mut(&job).expect("job exists").state =
                JobState::Waiting(WaitState::Commit(pending));
            return;
        }

        let unread = self.has_unread_required(job);
        let stale_sensitive = match &pending.step {
            WorkStep::Delegate(_)
            | WorkStep::AskUser(_)
            | WorkStep::Reply(_)
            | WorkStep::Finish(_) => true,
            WorkStep::Tool(tool) => self.tools[&tool.name].effect == ToolEffect::ExternalWrite,
            _ => false,
        };
        if unread && stale_sensitive {
            self.make_ready(job);
            return;
        }

        match pending.step.clone() {
            WorkStep::Continue => {
                if self.wait_satisfied(job, now) {
                    self.make_ready(job);
                }
            }
            WorkStep::Tool(tool) => self.start_tool(job, pending, tool, effects),
            WorkStep::Delegate(assignments) => {
                if self.active_jobs() + assignments.len() > self.limits.active_jobs {
                    let rejected = self.record(
                        Origin::Call(pending.call),
                        RecordBody::Audit {
                            message: "delegation was not accepted: job capacity is full".into(),
                        },
                        effects,
                    );
                    self.attach(job, rejected.seq);
                } else {
                    for assignment in assignments {
                        self.create_job(Owner::Job(job), assignment, effects);
                    }
                }
                self.make_ready(job);
            }
            WorkStep::Wait(wait) => self.install_wait(now, job, wait, effects),
            WorkStep::AskUser(question) => {
                let another_question = self.user_question.is_some_and(|owner| owner != job);
                if self.has_open_input_routing() && self.user_question == Some(job) {
                    debug_assert!(matches!(
                        self.jobs[&job].state,
                        JobState::Waiting(WaitState::User { .. })
                    ));
                } else if another_question || self.has_open_input_routing() {
                    self.jobs.get_mut(&job).expect("job exists").state =
                        JobState::Waiting(WaitState::Commit(pending));
                } else {
                    self.ask_user(job, question, effects);
                }
            }
            WorkStep::Inquire {
                job: target,
                question,
            } => self.open_inquiry(DeliveryTarget::Job(job), target, question, now, effects),
            WorkStep::Coordinate(request) => self.open_coordination(job, request, effects),
            WorkStep::Read(query) => match self.read(DeliveryTarget::Job(job), query, effects) {
                Ok(()) => {
                    if matches!(self.jobs[&job].state, JobState::Ready) {
                        self.enqueue_job(job);
                    }
                }
                Err(message) => {
                    self.finish_job(job, OutcomeKind::Failed, Completion::new(message), effects)
                }
            },
            WorkStep::PublishResult(result) => {
                let record = self.record(
                    Origin::Call(pending.call),
                    RecordBody::Published { job, result },
                    effects,
                );
                self.attach(job, record.seq);
                self.wake_result_waiters(job, record.seq, effects);
                if matches!(self.jobs[&job].state, JobState::Ready) {
                    self.enqueue_job(job);
                }
            }
            WorkStep::Reply(text) => {
                if self.has_open_input_routing() {
                    self.jobs.get_mut(&job).expect("job exists").state =
                        JobState::Waiting(WaitState::Commit(pending));
                } else {
                    let inputs = self.jobs[&job].inputs.clone();
                    self.record(
                        Origin::Job {
                            job,
                            revision: self.jobs[&job].revision,
                        },
                        RecordBody::Reply { job, inputs, text },
                        effects,
                    );
                    self.make_ready(job);
                }
            }
            WorkStep::Finish(completion) => {
                if self.finish_blocked(job) {
                    self.jobs.get_mut(&job).expect("job exists").state =
                        JobState::Waiting(WaitState::Commit(pending));
                } else {
                    self.finish_job(job, OutcomeKind::Completed, completion, effects);
                }
            }
            WorkStep::Fail(completion) => {
                self.finish_job(job, OutcomeKind::Failed, completion, effects);
            }
        }
    }

    fn wait_satisfied(&self, job: JobId, now: MonoTime) -> bool {
        match &self.jobs[&job].state {
            JobState::Ready => true,
            JobState::Finished(_) => false,
            JobState::Waiting(wait) => match wait {
                WaitState::Tool(call) => !self.calls.get(call).is_some_and(CallEntry::running),
                WaitState::Until(deadline) => *deadline <= now,
                WaitState::Job {
                    job: target,
                    revision,
                } => self.jobs.get(target).is_some_and(|entry| {
                    entry.revision != *revision || matches!(entry.state, JobState::Finished(_))
                }),
                WaitState::Result {
                    job: target,
                    revision,
                    after,
                } => {
                    self.jobs
                        .get(target)
                        .is_some_and(|entry| entry.revision != *revision)
                        || self.latest_result(*target, *after).is_some()
                        || self
                            .jobs
                            .get(target)
                            .is_some_and(|entry| matches!(entry.state, JobState::Finished(_)))
                }
                WaitState::Inquiry(inquiry) => !self.inquiries.contains_key(inquiry),
                WaitState::Coordination(routing) => {
                    self.routings.get(routing).is_none_or(|route| {
                        matches!(route.state, RoutingState::Closed | RoutingState::Failed(_))
                    })
                }
                WaitState::User { .. } | WaitState::Commit(_) => false,
            },
        }
    }

    fn start_tool(
        &mut self,
        job: JobId,
        pending: PendingStep,
        tool_call: crate::ToolCall,
        effects: &mut Vec<Effect>,
    ) {
        let spec = self.tools[&tool_call.name].clone();
        let blocked = self.running_tools() >= self.limits.tool_slots
            || self.job_has_running_tool(job)
            || (spec.effect == ToolEffect::ExternalWrite
                && (self.has_open_input_routing() || self.has_unresolved_write()));
        if blocked {
            self.jobs.get_mut(&job).expect("job exists").state =
                JobState::Waiting(WaitState::Commit(pending));
            return;
        }
        let revision = self.jobs[&job].revision;
        let request = Arc::new(tool_call);
        let call = self.start_call(
            CallTask::Tool {
                job,
                revision,
                effect: spec.effect,
                request: request.clone(),
                output_bytes: self.limits.tool_output_bytes,
            },
            Call::Tool(request),
            self.limits.tool_timeout,
            effects,
        );
        self.jobs.get_mut(&job).expect("job exists").state =
            JobState::Waiting(WaitState::Tool(call));
    }

    fn install_wait(&mut self, now: MonoTime, job: JobId, wait: Await, effects: &mut Vec<Effect>) {
        let state = match wait {
            Await::Tool(call) => WaitState::Tool(call),
            Await::After(duration) => {
                if duration.is_zero() {
                    self.make_ready(job);
                    return;
                }
                WaitState::Until(now.after(duration))
            }
            Await::Job(target) => {
                let revision = self.jobs[&target].revision;
                if matches!(self.jobs[&target].state, JobState::Finished(_)) {
                    self.deliver_outcome(job, target, effects);
                    self.make_ready(job);
                    return;
                }
                WaitState::Job {
                    job: target,
                    revision,
                }
            }
            Await::Result { job: target, after } => {
                let revision = self.jobs[&target].revision;
                if let Some(result) = self.latest_result(target, after) {
                    self.deliver(
                        DeliveryTarget::Job(job),
                        result,
                        DeliveryKind::Result,
                        effects,
                    );
                    self.make_ready(job);
                    return;
                }
                if matches!(self.jobs[&target].state, JobState::Finished(_)) {
                    self.deliver_outcome(job, target, effects);
                    self.make_ready(job);
                    return;
                }
                WaitState::Result {
                    job: target,
                    revision,
                    after,
                }
            }
        };
        self.jobs.get_mut(&job).expect("job exists").state = JobState::Waiting(state);
    }

    fn running_tools(&self) -> usize {
        self.calls
            .values()
            .filter(|call| call.kind() == CallKind::Tool && call.running())
            .count()
    }

    fn job_has_running_tool(&self, job: JobId) -> bool {
        self.calls
            .values()
            .any(|call| call.kind() == CallKind::Tool && call.job() == Some(job) && call.running())
    }

    fn has_unresolved_write(&self) -> bool {
        self.calls.values().any(|call| {
            call.tool_effect() == Some(ToolEffect::ExternalWrite)
                && (call.running() || call.external_effect() == ExternalEffect::Unknown)
        })
    }

    fn finish_blocked(&self, job: JobId) -> bool {
        (!self.is_investigation(job) && self.has_open_input_routing())
            || self.has_unread_required(job)
            || self.inquiries.values().any(|inquiry| inquiry.target == job)
            || self.job_has_running_tool(job)
            || self
                .children_of(job)
                .any(|child| !matches!(self.jobs[&child].state, JobState::Finished(_)))
            || self.subtree_has_unresolved_write(job)
    }

    fn subtree_has_unresolved_write(&self, root: JobId) -> bool {
        self.calls.values().any(|call| {
            call.job().is_some_and(|job| self.owns(root, job))
                && call.tool_effect() == Some(ToolEffect::ExternalWrite)
                && (call.running() || call.external_effect() == ExternalEffect::Unknown)
        })
    }

    pub(super) fn finish_job(
        &mut self,
        job: JobId,
        kind: OutcomeKind,
        completion: Completion,
        effects: &mut Vec<Effect>,
    ) {
        if self
            .jobs
            .get(&job)
            .is_none_or(|entry| matches!(entry.state, JobState::Finished(_)))
        {
            return;
        }
        if self.user_question == Some(job) {
            self.user_question = None;
        }
        if kind != OutcomeKind::Completed {
            let children = self.children_of(job).collect::<Vec<_>>();
            for child in children {
                if !matches!(self.jobs[&child].state, JobState::Finished(_)) {
                    self.finish_job(
                        child,
                        OutcomeKind::Cancelled,
                        Completion::new("owner finished before this job"),
                        effects,
                    );
                }
            }
        }
        self.invalidate_job_calls(job, effects);
        let related = self
            .inquiries
            .iter()
            .filter_map(|(id, inquiry)| {
                (inquiry.target == job
                    || matches!(inquiry.requester, DeliveryTarget::Job(owner) if owner == job))
                .then_some(*id)
            })
            .collect::<Vec<_>>();
        for inquiry in related {
            self.settle_inquiry(
                inquiry,
                InquiryResult::Unavailable("job finished".into()),
                effects,
            );
        }

        let outcome = Arc::new(JobOutcome {
            kind,
            completion,
            revision: self.jobs[&job].revision,
            as_of: self.peek_seq(),
        });
        let record = self.record(
            Origin::Job {
                job,
                revision: outcome.revision,
            },
            RecordBody::Outcome {
                job,
                outcome: outcome.clone(),
            },
            effects,
        );
        debug_assert_eq!(record.seq, outcome.as_of);
        let owner = self.jobs[&job].owner;
        self.jobs.get_mut(&job).expect("job exists").state = JobState::Finished(outcome.clone());
        self.jobs.get_mut(&job).expect("job exists").active_call = None;

        match owner {
            Owner::User => {}
            Owner::Job(parent) => {
                self.deliver(
                    DeliveryTarget::Job(parent),
                    record.seq,
                    DeliveryKind::Outcome,
                    effects,
                );
                if matches!(
                    self.jobs[&parent].state,
                    JobState::Waiting(WaitState::Job { job: target, .. })
                        | JobState::Waiting(WaitState::Result { job: target, .. })
                        if target == job
                ) {
                    self.make_ready(parent);
                }
            }
            Owner::Routing(routing) => self.deliver(
                DeliveryTarget::Routing(routing),
                record.seq,
                DeliveryKind::Outcome,
                effects,
            ),
        }
        self.wake_job_waiters(job, record.seq, effects);
    }

    fn wake_job_waiters(&mut self, producer: JobId, outcome: Seq, effects: &mut Vec<Effect>) {
        let waiting = self
            .jobs
            .iter()
            .filter_map(|(id, entry)| match entry.state {
                JobState::Waiting(WaitState::Job { job, .. }) if job == producer => Some(*id),
                JobState::Waiting(WaitState::Result { job, .. }) if job == producer => Some(*id),
                _ => None,
            })
            .collect::<Vec<_>>();
        for job in waiting {
            self.deliver(
                DeliveryTarget::Job(job),
                outcome,
                DeliveryKind::Outcome,
                effects,
            );
            self.make_ready(job);
        }
    }

    pub(super) fn pause(&mut self, job: JobId, effects: &mut Vec<Effect>) -> bool {
        if self
            .jobs
            .get(&job)
            .is_none_or(|entry| matches!(entry.state, JobState::Finished(_)))
        {
            return false;
        }
        if self.jobs[&job].local_paused {
            return false;
        }
        self.jobs.get_mut(&job).expect("job exists").local_paused = true;
        self.record(
            Origin::Kernel,
            RecordBody::JobControlChanged { job, paused: true },
            effects,
        );
        let subtree = self.subtree(job);
        for member in &subtree {
            self.invalidate_job_calls(*member, effects);
            if matches!(
                self.jobs[member].state,
                JobState::Waiting(WaitState::Commit(_))
            ) {
                self.jobs.get_mut(member).expect("job exists").state = JobState::Ready;
            }
        }
        let inquiries = self
            .inquiries
            .iter()
            .filter_map(|(id, inquiry)| subtree.contains(&inquiry.target).then_some(*id))
            .collect::<Vec<_>>();
        for inquiry in inquiries {
            self.settle_inquiry(
                inquiry,
                InquiryResult::Unavailable("target is paused".into()),
                effects,
            );
        }
        true
    }

    pub(super) fn resume(&mut self, job: JobId, effects: &mut Vec<Effect>) -> bool {
        let Some(entry) = self.jobs.get_mut(&job) else {
            return false;
        };
        if matches!(entry.state, JobState::Finished(_)) || !entry.local_paused {
            return false;
        }
        entry.local_paused = false;
        self.record(
            Origin::Kernel,
            RecordBody::JobControlChanged { job, paused: false },
            effects,
        );
        for member in self.subtree(job) {
            self.enqueue_job(member);
        }
        true
    }

    pub(super) fn cancel(&mut self, job: JobId, effects: &mut Vec<Effect>) -> bool {
        if self
            .jobs
            .get(&job)
            .is_none_or(|entry| matches!(entry.state, JobState::Finished(_)))
        {
            return false;
        }
        self.finish_job(
            job,
            OutcomeKind::Cancelled,
            Completion::new("cancelled by the host"),
            effects,
        );
        true
    }

    pub(super) fn change_spec(&mut self, job: JobId, spec: JobSpec, effects: &mut Vec<Effect>) {
        if self.user_question == Some(job) {
            self.user_question = None;
        }
        let children = self.children_of(job).collect::<Vec<_>>();
        for child in children {
            if !matches!(self.jobs[&child].state, JobState::Finished(_)) {
                self.finish_job(
                    child,
                    OutcomeKind::Cancelled,
                    Completion::new("owner contract changed"),
                    effects,
                );
            }
        }
        self.invalidate_job_calls(job, effects);
        let revision = {
            let entry = self.jobs.get_mut(&job).expect("job exists");
            entry.spec = spec.clone();
            entry.revision += 1;
            entry.report = None;
            entry.context.checkpoint = None;
            entry.revision
        };
        let changed_record = self.record(
            Origin::Kernel,
            RecordBody::JobChanged {
                job,
                spec,
                revision,
            },
            effects,
        );
        let waiters = self
            .jobs
            .iter()
            .filter_map(|(id, entry)| match entry.state {
                JobState::Waiting(WaitState::Job {
                    job: target,
                    revision: seen,
                })
                | JobState::Waiting(WaitState::Result {
                    job: target,
                    revision: seen,
                    ..
                }) if target == job && seen != revision => Some(*id),
                _ => None,
            })
            .collect::<Vec<_>>();
        for waiter in waiters {
            self.deliver(
                DeliveryTarget::Job(waiter),
                changed_record.seq,
                DeliveryKind::DependencyChanged,
                effects,
            );
            self.make_ready(waiter);
        }
        let changed = self
            .inquiries
            .iter()
            .filter_map(|(id, inquiry)| {
                (inquiry.target == job && inquiry.target_revision != self.jobs[&job].revision)
                    .then_some(*id)
            })
            .collect::<Vec<_>>();
        for inquiry in changed {
            self.settle_inquiry(inquiry, InquiryResult::Changed, effects);
        }
        self.make_ready(job);
    }

    pub(super) fn invalidate_job_call(&mut self, job: JobId, effects: &mut Vec<Effect>) {
        let call = self.jobs.get(&job).and_then(|entry| entry.active_call);
        if let Some(call) = call {
            self.cancel_call(call, effects);
            self.jobs.get_mut(&job).expect("job exists").active_call = None;
        }
        if matches!(
            self.jobs[&job].state,
            JobState::Waiting(WaitState::Commit(_))
        ) {
            self.jobs.get_mut(&job).expect("job exists").state = JobState::Ready;
        }
        self.invalidate_job_routings(job, effects);
    }

    pub(super) fn invalidate_job_calls(&mut self, job: JobId, effects: &mut Vec<Effect>) {
        self.invalidate_job_call(job, effects);
        let running = self
            .calls
            .iter()
            .filter_map(|(id, call)| (call.job() == Some(job) && call.running()).then_some(*id))
            .collect::<Vec<_>>();
        for call in running {
            self.cancel_call(call, effects);
        }
    }

    fn subtree(&self, root: JobId) -> Vec<JobId> {
        self.jobs
            .keys()
            .copied()
            .filter(|job| self.owns(root, *job))
            .collect()
    }

    pub(super) fn stop(&mut self, effects: &mut Vec<Effect>) -> bool {
        let active = self
            .calls
            .values()
            .any(|call| matches!(call.state, CallState::Running))
            || !self.inquiries.is_empty()
            || self
                .routings
                .values()
                .any(|routing| !matches!(routing.state, RoutingState::Closed))
            || self
                .jobs
                .values()
                .any(|job| !matches!(job.state, JobState::Finished(_)))
            || self.inputs.values().any(|input| input.finished.is_none());
        if !active {
            return false;
        }
        let calls = self
            .calls
            .iter()
            .filter_map(|(id, call)| call.running().then_some(*id))
            .collect::<Vec<_>>();
        for call in calls {
            self.cancel_call(call, effects);
        }
        let inquiries = self.inquiries.keys().copied().collect::<Vec<_>>();
        for inquiry in inquiries {
            self.settle_inquiry(
                inquiry,
                InquiryResult::Unavailable("agent stopped".into()),
                effects,
            );
        }
        for routing in self.routings.values_mut() {
            if !matches!(routing.state, RoutingState::Closed) {
                routing.state = RoutingState::Closed;
                routing.active_call = None;
            }
        }
        self.user_question = None;
        self.routing_ready.clear();
        self.interactive_ready.clear();
        self.background_ready.clear();
        let roots = self
            .jobs
            .iter()
            .filter_map(|(id, job)| {
                (!matches!(job.owner, Owner::Job(_)) && !matches!(job.state, JobState::Finished(_)))
                    .then_some(*id)
            })
            .collect::<Vec<_>>();
        for job in roots {
            self.finish_job(
                job,
                OutcomeKind::Cancelled,
                Completion::new("stopped by the host"),
                effects,
            );
        }
        self.interactive_ready.clear();
        self.background_ready.clear();
        let unfinished = self
            .inputs
            .iter()
            .filter_map(|(id, input)| input.finished.is_none().then_some(*id))
            .collect::<Vec<_>>();
        for input in unfinished {
            let outcome = InputOutcome::Cancelled;
            self.inputs.get_mut(&input).expect("input exists").finished = Some(outcome.clone());
            self.record(
                Origin::Kernel,
                RecordBody::InputFinished { input, outcome },
                effects,
            );
        }
        self.record(Origin::Kernel, RecordBody::Stopped, effects);
        true
    }
}
