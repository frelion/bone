use super::*;

impl Kernel {
    pub(super) fn has_pending_input(&self) -> bool {
        self.inputs
            .values()
            .any(|input| input.pending && input.finished.is_none())
    }

    fn has_inputs_to_conclude(&self) -> bool {
        self.inputs.values().any(|input| {
            input.finished.is_none()
                && input
                    .required_jobs
                    .iter()
                    .all(|job| matches!(self.jobs[job].state, JobState::Finished(_)))
        })
    }

    fn has_future_work(&self) -> bool {
        !self.inquiries.is_empty()
            || self
                .calls
                .values()
                .any(|call| call.running() || call.external_effect() == ExternalEffect::Unknown)
            || self.jobs.iter().any(|(id, job)| {
                !self.is_paused(*id)
                    && (matches!(
                        job.state,
                        JobState::Ready | JobState::Waiting(WaitState::Until(_))
                    ) || (!matches!(job.state, JobState::Finished(_))
                        && self.has_unread_required(*id)))
            })
    }

    pub(super) fn invalidate_conversation_call(&mut self, effects: &mut Vec<Effect>) {
        if let Some(call) = self.conversation.active_call.take() {
            self.cancel_call(call, effects);
        }
    }

    fn validate_inputs(&self, inputs: &[InputId]) -> Result<(), String> {
        if inputs.is_empty() {
            return Err("provide at least one current input".into());
        }
        if inputs.iter().collect::<BTreeSet<_>>().len() != inputs.len() {
            return Err("duplicate input IDs".into());
        }
        for input in inputs {
            if self
                .inputs
                .get(input)
                .is_none_or(|entry| entry.finished.is_some())
            {
                return Err(format!("input {input} is not open"));
            }
        }
        Ok(())
    }

    fn handled_inputs(&mut self, inputs: &[InputId]) {
        for input in inputs {
            self.inputs.get_mut(input).expect("validated input").pending = false;
        }
    }

    pub(super) fn apply_conversation(
        &mut self,
        step: ConversationStep,
        effects: &mut Vec<Effect>,
    ) -> Result<(), String> {
        match step {
            ConversationStep::Start(assignments) => {
                let inputs = self
                    .inputs
                    .iter()
                    .filter_map(|(id, entry)| entry.finished.is_none().then_some(*id))
                    .collect::<Vec<_>>();
                self.validate_assignments(None, &inputs, &assignments)?;
                if self.active_jobs() + assignments.len() > self.limits.active_jobs {
                    return Err("job capacity is full".into());
                }
                for assignment in &assignments {
                    self.validate_inputs(&assignment.inputs)?;
                }
                for mut assignment in assignments {
                    assignment
                        .inputs
                        .sort_by_key(|id| self.inputs[id].accepted_at);
                    let inputs = assignment.inputs.clone();
                    let job = self.create_job(Owner::User, assignment, effects);
                    for input in &inputs {
                        self.inputs
                            .get_mut(input)
                            .unwrap()
                            .required_jobs
                            .insert(job);
                    }
                    self.handled_inputs(&inputs);
                }
                self.conversation.ready = self.has_pending_input() || self.has_inputs_to_conclude();
            }
            ConversationStep::Send {
                job,
                mut inputs,
                message,
                question,
                tools,
            } => {
                self.validate_inputs(&inputs)?;
                self.validate_model_item("job message", &message)?;
                let requested = self.resolve_tools(None, tools.as_ref())?;
                let target = self.jobs.get(&job).ok_or("job does not exist")?;
                if tools.is_some() && target.allowed_tools != requested {
                    return Err("job has different tool authority; start new work".into());
                }
                if target.owner != Owner::User
                    || target.local_paused
                    || matches!(target.state, JobState::Finished(_))
                {
                    return Err("send requires an active root job; create new work to replace a finished job".into());
                }
                match target.state {
                    JobState::Waiting(WaitState::User { question: expected })
                        if question != Some(expected) =>
                    {
                        return Err("reply must identify the exact pending job question".into());
                    }
                    JobState::Waiting(WaitState::User { .. }) => {}
                    _ if question.is_some() => {
                        return Err("job has no matching pending question".into());
                    }
                    _ => {}
                }
                inputs.sort_by_key(|id| self.inputs[id].accepted_at);
                self.invalidate_job_call(job, effects);
                for input in &inputs {
                    self.inputs
                        .get_mut(input)
                        .unwrap()
                        .required_jobs
                        .insert(job);
                    if !self.jobs[&job].inputs.contains(input) {
                        self.jobs.get_mut(&job).unwrap().inputs.push(*input);
                        self.deliver(
                            DeliveryTarget::Job(job),
                            self.inputs[input].accepted_at,
                            DeliveryKind::Input,
                            effects,
                        );
                    }
                }
                let record = self.record(
                    Origin::Kernel,
                    RecordBody::JobMessage {
                        job,
                        inputs: inputs.clone(),
                        text: message,
                    },
                    effects,
                );
                self.attach(job, record.seq);
                self.make_ready(job);
                self.handled_inputs(&inputs);
                self.conversation.ready = self.has_pending_input() || self.has_inputs_to_conclude();
            }
            ConversationStep::Reply {
                inputs,
                text,
                outcome,
            } => {
                self.validate_inputs(&inputs)?;
                self.validate_model_item("reply", &text)?;
                if text.trim().is_empty() {
                    return Err("reply must not be empty".into());
                }
                if inputs.iter().any(|id| {
                    self.inputs[id]
                        .required_jobs
                        .iter()
                        .any(|job| !matches!(self.jobs[job].state, JobState::Finished(_)))
                }) {
                    return Err("cannot conclude input while its jobs are unfinished".into());
                }
                if outcome == InputOutcome::Completed
                    && inputs.iter().any(|id| {
                        self.inputs[id]
                            .required_jobs
                            .iter()
                            .any(|job| self.subtree_has_unresolved_write(*job))
                    })
                {
                    return Err(
                        "cannot complete input while its writes are still running or unresolved"
                            .into(),
                    );
                }
                let record = self.record(
                    Origin::Kernel,
                    RecordBody::Reply {
                        inputs: inputs.clone(),
                        text,
                    },
                    effects,
                );
                self.conversation.records.push(record.seq);
                self.handled_inputs(&inputs);
                for input in inputs {
                    self.inputs.get_mut(&input).unwrap().finished = Some(outcome.clone());
                    self.record(
                        Origin::Kernel,
                        RecordBody::InputFinished {
                            input,
                            outcome: outcome.clone(),
                        },
                        effects,
                    );
                }
                self.conversation.ready = self.has_pending_input() || self.has_inputs_to_conclude();
            }
            ConversationStep::Ask { inputs, question } => {
                self.validate_inputs(&inputs)?;
                if question.trim().is_empty() {
                    return Err("question must not be empty".into());
                }
                self.validate_model_item("question", &question)?;
                let record = self.record(
                    Origin::Kernel,
                    RecordBody::Clarification { inputs, question },
                    effects,
                );
                self.conversation.records.push(record.seq);
                self.conversation.question = Some(record.seq);
                self.conversation.ready = false;
            }
            ConversationStep::Control { job, action } => {
                if self.jobs.get(&job).is_none_or(|entry| {
                    entry.owner != Owner::User || matches!(entry.state, JobState::Finished(_))
                }) {
                    return Err("control requires an active root job".into());
                }
                match action {
                    OwnedAction::Pause => {
                        self.pause(job, effects);
                    }
                    OwnedAction::Resume => {
                        self.resume(job, effects);
                    }
                    OwnedAction::Cancel => {
                        self.finish_job(
                            job,
                            OutcomeKind::Cancelled,
                            Completion::new("cancelled by conversation"),
                            effects,
                        );
                    }
                }
                self.conversation.ready = true;
            }
            ConversationStep::Read(query) => {
                self.validate_read(DeliveryTarget::Conversation, &query)?;
                self.read(DeliveryTarget::Conversation, query, effects)?;
                self.conversation.ready = true;
            }
            ConversationStep::UpdateConstraints {
                source,
                expected_revision,
                constraints,
            } => {
                self.validate_inputs(&[source])?;
                if expected_revision != self.constraints_revision {
                    return Err("session constraints changed".into());
                }
                self.validate_model_item("constraints", &constraints)?;
                self.revoke_model_calls(effects);
                let active = self.jobs.keys().copied().collect::<Vec<_>>();
                for job in active {
                    self.invalidate_job_calls(job, effects);
                    self.enqueue_job(job);
                }
                self.constraints = constraints.clone();
                self.constraints_revision += 1;
                self.record(
                    Origin::Kernel,
                    RecordBody::ConstraintsChanged {
                        source,
                        revision: self.constraints_revision,
                        constraints,
                    },
                    effects,
                );
                self.conversation.ready = true;
            }
            ConversationStep::Wait => {
                if self.has_pending_input() || self.has_inputs_to_conclude() {
                    return Err("handle or conclude open inputs before waiting".into());
                }
                if self.inputs.values().any(|input| input.finished.is_none())
                    && !self.has_future_work()
                {
                    return Err(
                        "nothing can wake this conversation; reply or ask for missing information"
                            .into(),
                    );
                }
                self.conversation.ready = false;
            }
        }
        Ok(())
    }

    pub(super) fn reject_conversation(
        &mut self,
        call: CallId,
        message: String,
        effects: &mut Vec<Effect>,
    ) {
        let budget = &mut self.conversation.rejections;
        budget.consecutive += 1;
        budget.total += 1;
        let budget = budget.clone();
        let record = self.record(
            Origin::Call(call),
            RecordBody::ConversationRejected {
                call,
                message: message.clone(),
                budget: budget.clone(),
            },
            effects,
        );
        self.conversation.records.push(record.seq);
        if budget.exhausted() {
            self.fail_conversation(
                format!("conversation correction budget exhausted: {message}"),
                effects,
            );
        } else {
            self.conversation.ready = true;
        }
    }

    pub(super) fn fail_conversation(&mut self, message: String, effects: &mut Vec<Effect>) {
        let inputs = self
            .inputs
            .iter()
            .filter_map(|(id, entry)| entry.finished.is_none().then_some(*id))
            .collect();
        let record = self.record(
            Origin::Kernel,
            RecordBody::ConversationFailed { inputs, message },
            effects,
        );
        self.conversation.failure = Some(record.seq);
        self.conversation.records.push(record.seq);
        self.conversation.ready = false;
    }

    pub(super) fn retry(&mut self, input: InputId) -> bool {
        if self
            .inputs
            .get(&input)
            .is_none_or(|entry| entry.finished.is_some())
            || self.conversation.failure.take().is_none()
        {
            return false;
        }
        self.conversation.rejections = WorkRejections::default();
        self.conversation.ready = true;
        true
    }

    pub(super) fn need_input(&mut self, job: JobId, question: String, effects: &mut Vec<Effect>) {
        let record = self.record(
            Origin::Job {
                job,
                revision: self.jobs[&job].revision,
            },
            RecordBody::JobNeedsInput { job, question },
            effects,
        );
        self.attach(job, record.seq);
        self.jobs.get_mut(&job).unwrap().state = JobState::Waiting(WaitState::User {
            question: record.seq,
        });
        match self.jobs[&job].owner {
            Owner::User => {
                self.conversation.records.push(record.seq);
                self.conversation.ready = true;
            }
            Owner::Job(parent) => self.deliver(
                DeliveryTarget::Job(parent),
                record.seq,
                DeliveryKind::Inquiry,
                effects,
            ),
        }
    }

    pub(crate) fn delegation_capacity(&self, job: JobId) -> DelegationLimits {
        let mut remaining = usize::MAX;
        let mut depth = self.limits.job_depth.saturating_sub(self.job_depth(job));
        let mut distance = 0;
        let mut current = job;
        loop {
            let entry = &self.jobs[&current];
            // Finished descendants still count: completing jobs cannot refill a budget.
            let used = self
                .jobs
                .keys()
                .filter(|id| **id != current && self.owns(current, **id))
                .count();
            remaining = remaining.min(entry.delegation.max_descendants.saturating_sub(used));
            depth = depth.min(entry.delegation.max_depth.saturating_sub(distance));
            if let Owner::Job(parent) = entry.owner {
                current = parent;
                distance += 1;
            } else {
                remaining = remaining.min(self.limits.job_budget.saturating_sub(used));
                break;
            }
        }
        if remaining == 0 || depth == 0 {
            DelegationLimits::default()
        } else {
            DelegationLimits {
                max_descendants: remaining,
                max_depth: depth,
            }
        }
    }

    pub(super) fn validate_assignments(
        &self,
        source: Option<JobId>,
        allowed_inputs: &[InputId],
        assignments: &[Assignment],
    ) -> Result<(), String> {
        if assignments.is_empty() {
            return Err("delegate requires at least one assignment".into());
        }
        if let Some(job) = source {
            let capacity = self.delegation_capacity(job);
            if capacity.max_depth == 0 || assignments.len() > capacity.max_descendants {
                return Err(
                    "delegation exceeds this job's remaining subtree budget or depth".into(),
                );
            }
            for assignment in assignments {
                let requested = assignment.delegation;
                if (requested.max_depth == 0) != (requested.max_descendants == 0)
                    || requested.max_depth > capacity.max_depth - 1
                    || requested.max_descendants > capacity.max_descendants - assignments.len()
                {
                    return Err(
                        "child delegation limits must fit the parent's remaining authority".into(),
                    );
                }
            }
        }
        let depth = source.map_or(1, |job| self.job_depth(job) + 1);
        if depth > self.limits.job_depth {
            return Err("job tree is too deep".into());
        }
        for assignment in assignments {
            self.validate_spec(&assignment.spec)?;
            self.resolve_tools(source, assignment.tools.as_ref())?;
            if !assignment
                .inputs
                .iter()
                .all(|id| allowed_inputs.contains(id))
            {
                return Err("assignment cites input outside its owner's context".into());
            }
            if let Some(seq) = assignment.evidence.iter().find(|seq| !match source {
                Some(job) => self.can_read_record(job, **seq),
                None => self.can_read_conversation_record(**seq),
            }) {
                return Err(format!(
                    "assignment cites inaccessible evidence {seq}; cite a readable record source or use an empty evidence list"
                ));
            }
            if let Some(seed) = assignment.seed
                && !self.can_seed(source, seed)
            {
                return Err("assignment cannot reuse that completed job".into());
            }
        }
        Ok(())
    }

    fn validate_spec(&self, spec: &JobSpec) -> Result<(), String> {
        if spec.goal.trim().is_empty()
            || spec.scope.trim().is_empty()
            || spec.done_when.trim().is_empty()
        {
            Err("job goal, scope and done_when must be non-empty".into())
        } else {
            self.validate_model_item("job spec", spec)
        }
    }

    pub(super) fn resolve_tools(
        &self,
        parent: Option<JobId>,
        selection: Option<&crate::ToolSelection>,
    ) -> Result<BTreeSet<String>, String> {
        let ceiling = parent.map_or_else(
            || self.tools.keys().cloned().collect(),
            |id| self.jobs[&id].allowed_tools.clone(),
        );
        match selection {
            None => Ok(ceiling),
            Some(crate::ToolSelection::ReadOnly) => Ok(ceiling
                .into_iter()
                .filter(|name| {
                    self.tools
                        .get(name)
                        .is_some_and(|tool| tool.effect == ToolEffect::ReadOnly)
                })
                .collect()),
            Some(crate::ToolSelection::Only(names)) => {
                for name in names {
                    if !self.tools.contains_key(name) {
                        return Err(format!("unknown tool: {name}"));
                    }
                    if !ceiling.contains(name) {
                        return Err(format!("tool outside parent's authority: {name}"));
                    }
                }
                Ok(names.iter().cloned().collect())
            }
        }
    }

    pub(super) fn create_job(
        &mut self,
        owner: Owner,
        assignment: Assignment,
        effects: &mut Vec<Effect>,
    ) -> JobId {
        let parent = match owner {
            Owner::Job(parent) => Some(parent),
            _ => None,
        };
        let allowed_tools = self
            .resolve_tools(parent, assignment.tools.as_ref())
            .expect("validated tool selection");
        let id = JobId(self.next_job);
        self.next_job += 1;
        let Assignment {
            tools: _,
            spec,
            inputs,
            evidence,
            seed,
            delegation,
        } = assignment;
        let delegation = if matches!(owner, Owner::Job(_)) {
            delegation
        } else {
            DelegationLimits {
                max_descendants: self.limits.job_budget,
                max_depth: self.limits.job_depth.saturating_sub(1),
            }
        };
        self.jobs.insert(
            id,
            Job {
                allowed_tools: allowed_tools.clone(),
                spec: spec.clone(),
                inputs: inputs.clone(),
                owner,
                revision: 1,
                local_paused: false,
                state: JobState::Ready,
                active_call: None,
                context: JobContext::default(),
                report: None,
                work_rejections: WorkRejections::default(),
                delegation,
            },
        );
        let created = self.record(
            Origin::Kernel,
            RecordBody::JobCreated {
                allowed_tools,
                job: id,
                spec,
                owner,
            },
            effects,
        );
        for input in inputs {
            let source = self.inputs[&input].accepted_at;
            self.deliver(
                DeliveryTarget::Job(id),
                source,
                DeliveryKind::Input,
                effects,
            );
        }
        for source in evidence {
            self.deliver(
                DeliveryTarget::Job(id),
                source,
                DeliveryKind::Memory,
                effects,
            );
        }
        if let Some(source_job) = seed {
            let source = &self.jobs[&source_job];
            let outcome = match &source.state {
                JobState::Finished(outcome) => outcome,
                _ => unreachable!("seed validation requires a terminal job"),
            };
            let summary = outcome.completion.summary.clone();
            let mut record_refs = vec![outcome.as_of];
            record_refs.extend(outcome.completion.evidence.iter().copied());
            if let Some(checkpoint) = &source.context.checkpoint {
                record_refs.extend(checkpoint.evidence.iter().copied());
            }
            record_refs.sort_unstable();
            record_refs.dedup();
            let memory = self.record(
                Origin::Kernel,
                RecordBody::ImportedMemory {
                    source_job,
                    source_revision: source.revision,
                    summary,
                    record_refs,
                },
                effects,
            );
            self.attach(id, memory.seq);
        }
        match owner {
            Owner::Job(parent) => self.deliver(
                DeliveryTarget::Job(parent),
                created.seq,
                DeliveryKind::ChildCreated,
                effects,
            ),
            Owner::User => {}
        }
        self.enqueue_job(id);
        id
    }
}
