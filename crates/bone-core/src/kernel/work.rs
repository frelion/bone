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
            self.reject_work(job, call, message, effects);
            return;
        }

        self.jobs
            .get_mut(&job)
            .expect("job exists")
            .work_rejections
            .consecutive = 0;

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

    /// Validation refusals are feedback, not proof that the requested work failed.
    /// Do not acknowledge input review or store any part of the rejected proposal.
    fn reject_work(
        &mut self,
        job: JobId,
        call: CallId,
        message: String,
        effects: &mut Vec<Effect>,
    ) {
        let entry = self.jobs.get_mut(&job).expect("job exists");
        entry.work_rejections.consecutive = entry.work_rejections.consecutive.saturating_add(1);
        entry.work_rejections.total = entry.work_rejections.total.saturating_add(1);
        let budget = entry.work_rejections.clone();
        let exhausted = budget.exhausted();
        let receipt = self.record(
            Origin::Call(call),
            RecordBody::WorkRejected {
                job,
                call,
                message: message.clone(),
                budget,
            },
            effects,
        );
        self.attach(job, receipt.seq);
        if exhausted {
            self.finish_job(
                job,
                OutcomeKind::Failed,
                Completion::new(format!("work correction budget exhausted: {message}")),
                effects,
            );
        } else {
            self.make_ready(job);
        }
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
            self.validate_model_item("work note", note)?;
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
                self.validate_model_item("inquiry answer", reason)?;
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
                let Some(_) = self.tools.get(&tool.name) else {
                    return Err(format!("unknown tool: {}", tool.name));
                };
                if !self.jobs[&job].allowed_tools.contains(&tool.name) {
                    return Err(format!(
                        "job {job} is not authorized to use tool: {}",
                        tool.name
                    ));
                }
                Ok(())
            }
            WorkStep::Delegate(delegation) => self.validate_assignments(
                Some(job),
                &self.jobs[&job].inputs,
                &delegation.assignments,
            ),
            WorkStep::Wait(wait) => self.validate_wait(job, wait),
            WorkStep::NeedInput(question) => {
                if question.trim().is_empty() {
                    return Err("question must not be empty".into());
                }
                self.validate_model_item("job question", question)
            }
            WorkStep::Respond {
                job: target,
                question,
                message,
            } => {
                if self.jobs.get(target).is_none_or(|entry| entry.owner != Owner::Job(job) || !matches!(entry.state, JobState::Waiting(WaitState::User { question: expected }) if expected == *question)) {
                    return Err("respond requires the exact pending question of an owned child".into());
                }
                self.validate_model_item("job response", message)
            }
            WorkStep::Inquire {
                job: target,
                question,
            } => {
                if question.trim().is_empty()
                    || *target == job
                    || !self.can_access_job(job, *target)
                {
                    return Err("invalid job inquiry".into());
                }
                self.validate_model_item("job inquiry", question)?;
                Ok(())
            }
            WorkStep::ControlOwned {
                job: target,
                action: _,
            } => {
                if *target == job || !self.owns(job, *target) {
                    Err("a worker may control only its active child tree".into())
                } else if self
                    .jobs
                    .get(target)
                    .is_none_or(|entry| matches!(entry.state, JobState::Finished(_)))
                {
                    Err("controlled job must be active".into())
                } else {
                    Ok(())
                }
            }
            WorkStep::Read(query) => self.validate_read(DeliveryTarget::Job(job), query),
            WorkStep::PublishResult(report) => self.validate_report(job, report),
            WorkStep::Finish(completion) | WorkStep::Fail(completion) => {
                self.validate_completion(job, completion)
            }
        }
    }

    fn validate_report(&self, job: JobId, report: &ReportDraft) -> Result<(), String> {
        self.validate_artifact(job, "report", &report.summary, &report.evidence, report)
    }

    fn validate_completion(&self, job: JobId, completion: &Completion) -> Result<(), String> {
        self.validate_artifact(
            job,
            "completion",
            &completion.summary,
            &completion.evidence,
            completion,
        )
    }

    fn validate_artifact<T: serde::Serialize + ?Sized>(
        &self,
        job: JobId,
        name: &str,
        summary: &str,
        evidence: &[Seq],
        artifact: &T,
    ) -> Result<(), String> {
        if summary.trim().is_empty() {
            return Err(format!("{name} summary is empty"));
        }
        self.validate_model_item(name, artifact)?;
        if evidence.iter().all(|seq| self.can_read_record(job, *seq)) {
            Ok(())
        } else {
            Err(format!("{name} cites inaccessible evidence"))
        }
    }

    fn validate_wait(&self, job: JobId, wait: &Await) -> Result<(), String> {
        match wait {
            Await::Jobs(jobs) => {
                let distinct = jobs.iter().copied().collect::<BTreeSet<_>>();
                if jobs.is_empty()
                    || distinct.len() != jobs.len()
                    || jobs
                        .iter()
                        .any(|target| *target == job || !self.can_access_job(job, *target))
                {
                    Err("job group must contain distinct owned descendants".into())
                } else {
                    Ok(())
                }
            }
            Await::Tool(call) => {
                if self
                    .calls
                    .get(call)
                    .is_some_and(|entry| entry.kind() == CallKind::Tool && entry.job() == Some(job))
                {
                    Ok(())
                } else {
                    Err("job cannot wait for that tool call".into())
                }
            }
            Await::After(_) => Ok(()),
            Await::Job(target) | Await::Result { job: target, .. } => {
                // Workers can depend only on strict descendants. Every such
                // edge increases ownership depth, so a dependency cycle cannot
                // be formed.
                if *target != job && self.can_access_job(job, *target) {
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
            | WorkStep::NeedInput(_)
            | WorkStep::Finish(_)
            | WorkStep::ControlOwned { .. }
            | WorkStep::Respond { .. } => true,
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
            WorkStep::Delegate(delegation) => {
                let assignments = delegation.assignments;
                if self.active_jobs() + assignments.len() > self.limits.active_jobs {
                    let rejected = self.record(
                        Origin::Call(pending.call),
                        RecordBody::Audit {
                            message: "delegation was not accepted: job capacity is full".into(),
                        },
                        effects,
                    );
                    self.attach(job, rejected.seq);
                    self.make_ready(job);
                } else {
                    let children = assignments
                        .into_iter()
                        .map(|assignment| self.create_job(Owner::Job(job), assignment, effects))
                        .collect::<Vec<_>>();
                    let record = self.record(
                        Origin::Call(pending.call),
                        RecordBody::Delegated {
                            job,
                            call: pending.call,
                            children: children.clone(),
                            continuation: delegation.continuation,
                        },
                        effects,
                    );
                    self.attach(job, record.seq);
                    match delegation.continuation {
                        AfterDelegation::Continue => self.make_ready(job),
                        AfterDelegation::WaitAll => {
                            self.install_wait(now, job, Await::Jobs(children), effects)
                        }
                    }
                }
            }
            WorkStep::Wait(wait) => self.install_wait(now, job, wait, effects),
            WorkStep::NeedInput(question) => self.need_input(job, question, effects),
            WorkStep::Respond {
                job: target,
                question,
                message,
            } => {
                if !matches!(self.jobs[&target].state, JobState::Waiting(WaitState::User { question: expected }) if expected == question)
                {
                    self.make_ready(job);
                    return;
                }
                let record = self.record(
                    Origin::Job {
                        job,
                        revision: self.jobs[&job].revision,
                    },
                    RecordBody::JobMessage {
                        job: target,
                        inputs: Vec::new(),
                        text: message,
                    },
                    effects,
                );
                self.attach(target, record.seq);
                self.make_ready(target);
                self.make_ready(job);
            }
            WorkStep::Inquire {
                job: target,
                question,
            } => self.open_inquiry(DeliveryTarget::Job(job), target, question, now, effects),
            WorkStep::ControlOwned {
                job: target,
                action,
            } => {
                match action {
                    OwnedAction::Pause => {
                        let _ = self.pause(target, effects);
                    }
                    OwnedAction::Resume => {
                        let _ = self.resume(target, effects);
                    }
                    OwnedAction::Cancel => {
                        let _ = self.cancel(target, effects);
                    }
                }
                self.make_ready(job);
            }
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
                WaitState::Jobs(jobs) => self.job_group_ready(jobs),
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
                && (self.has_pending_input() || self.has_unresolved_write()));
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
            Await::Jobs(jobs) => {
                if self.job_group_ready(&jobs) {
                    for target in jobs {
                        self.deliver_outcome(job, target, effects);
                    }
                    self.make_ready(job);
                    return;
                }
                WaitState::Jobs(jobs)
            }
            Await::Tool(call) => {
                if !self.calls[&call].running() {
                    self.make_ready(job);
                    return;
                }
                WaitState::Tool(call)
            }
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

    pub(super) fn job_group_ready(&self, jobs: &[JobId]) -> bool {
        jobs.iter().any(|id| self.jobs.get(id).is_none_or(|entry| {
            matches!(&entry.state, JobState::Finished(outcome) if outcome.kind != OutcomeKind::Completed)
        })) || jobs.iter().all(|id| matches!(self.jobs[id].state, JobState::Finished(_)))
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
        self.has_pending_input()
            || self.has_unread_required(job)
            || self.inquiries.values().any(|inquiry| inquiry.target == job)
            || self.job_has_running_tool(job)
            || self
                .children_of(job)
                .any(|child| !matches!(self.jobs[&child].state, JobState::Finished(_)))
            || self.subtree_has_unresolved_write(job)
    }

    pub(super) fn subtree_has_unresolved_write(&self, root: JobId) -> bool {
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
            Owner::User => {
                self.conversation.records.push(record.seq);
                self.conversation.ready = true;
            }
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
        }
        self.wake_job_waiters(job, effects);
    }

    fn wake_job_waiters(&mut self, producer: JobId, effects: &mut Vec<Effect>) {
        let waiting = self
            .jobs
            .iter()
            .filter_map(|(id, entry)| match &entry.state {
                JobState::Waiting(WaitState::Job { job, .. }) if *job == producer => Some(*id),
                JobState::Waiting(WaitState::Result { job, .. }) if *job == producer => Some(*id),
                JobState::Waiting(WaitState::Jobs(jobs))
                    if jobs.contains(&producer) && self.job_group_ready(jobs) =>
                {
                    Some(*id)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for job in waiting {
            let targets = match &self.jobs[&job].state {
                JobState::Waiting(WaitState::Jobs(jobs)) => jobs.clone(),
                _ => vec![producer],
            };
            for target in targets {
                self.deliver_outcome(job, target, effects);
            }
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
    }

    pub(super) fn invalidate_job_calls(&mut self, job: JobId, effects: &mut Vec<Effect>) {
        self.invalidate_job_call(job, effects);
        let running = self
            .calls
            .iter()
            .filter_map(|(id, call)| {
                let requested = matches!(call.task, CallTask::SessionCompact {
                    requester: DeliveryTarget::Job(requester), ..
                } if requester == job);
                ((call.job() == Some(job) || requested) && call.running()).then_some(*id)
            })
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
            || self.conversation.active_call.is_some()
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
        self.conversation.active_call = None;
        self.conversation.question = None;
        self.conversation.ready = false;
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
        self.conversation.ready = false;
        self.record(Origin::Kernel, RecordBody::Stopped, effects);
        true
    }
}
