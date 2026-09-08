use super::*;

impl Kernel {
    pub(super) fn reply_target(&self, input: InputId) -> Option<ReplyTarget> {
        let entry = self.inputs.get(&input)?;
        if matches!(
            self.routings[&entry.routing].state,
            RoutingState::WaitingForUser(_)
        ) {
            return Some(ReplyTarget::Routing(entry.routing));
        }
        let job = self.user_question?;
        (entry.finished.is_none()
            && self.jobs[&job].inputs.contains(&input)
            && matches!(
                self.jobs[&job].state,
                JobState::Waiting(WaitState::User { .. })
            ))
        .then_some(ReplyTarget::Job(job))
    }

    pub(super) fn supersede_input_routings(
        &mut self,
        effects: &mut Vec<Effect>,
    ) -> (Vec<InputId>, Vec<Seq>) {
        let routings = self
            .routings
            .iter()
            .filter_map(|(id, route)| {
                (matches!(route.requester, Requester::Inputs)
                    && !matches!(route.state, RoutingState::Closed))
                .then_some(*id)
            })
            .collect::<BTreeSet<_>>();
        let mut inputs = self
            .inputs
            .iter()
            .filter_map(|(id, entry)| {
                (entry.finished.is_none() && routings.contains(&entry.routing))
                    .then_some((entry.accepted_at, *id))
            })
            .collect::<Vec<_>>();
        inputs.sort_unstable();

        let mut records = routings
            .iter()
            .flat_map(|routing| {
                let route = &self.routings[routing];
                route.records.iter().copied().chain(match route.state {
                    RoutingState::WaitingForUser(record) | RoutingState::Failed(record) => {
                        Some(record)
                    }
                    _ => None,
                })
            })
            .collect::<Vec<_>>();
        records.sort_unstable();
        records.dedup();

        for routing in routings {
            self.invalidate_routing(routing, effects);
        }
        (
            inputs.into_iter().map(|(_, input)| input).collect(),
            records,
        )
    }

    pub(super) fn open_input_routing(
        &mut self,
        inputs: Vec<InputId>,
        records: Vec<Seq>,
        effects: &mut Vec<Effect>,
    ) -> Seq {
        let routing = self.peek_seq();
        let record = self.record(
            Origin::Kernel,
            RecordBody::RoutingStarted {
                inputs: inputs.clone(),
                source: None,
            },
            effects,
        );
        debug_assert_eq!(record.seq, routing);
        self.routings.insert(
            routing,
            Routing {
                requester: Requester::Inputs,
                inputs,
                request: None,
                records,
                active_call: None,
                state: RoutingState::Ready,
            },
        );
        self.enqueue_routing(routing);
        routing
    }

    pub(super) fn accept_user_reply(
        &mut self,
        job: JobId,
        input: InputId,
        source: Seq,
        effects: &mut Vec<Effect>,
    ) -> Seq {
        let routing = self.peek_seq();
        let record = self.record(
            Origin::Kernel,
            RecordBody::RoutingStarted {
                inputs: vec![input],
                source: None,
            },
            effects,
        );
        debug_assert_eq!(record.seq, routing);
        self.routings.insert(
            routing,
            Routing {
                requester: Requester::Inputs,
                inputs: vec![input],
                request: None,
                records: vec![source],
                active_call: None,
                state: RoutingState::Closed,
            },
        );
        self.record(
            Origin::Kernel,
            RecordBody::RoutingFinished { routing },
            effects,
        );
        let target = self.jobs.get_mut(&job).expect("question owner exists");
        if !target.inputs.contains(&input) {
            target.inputs.push(input);
        }
        self.user_question = None;
        routing
    }

    pub(super) fn apply_decision(
        &mut self,
        now: MonoTime,
        routing: Seq,
        decision: KernelDecision,
        effects: &mut Vec<Effect>,
    ) -> Result<(), String> {
        let route = self
            .routings
            .get(&routing)
            .cloned()
            .ok_or_else(|| format!("routing {routing} no longer exists"))?;
        match decision {
            KernelDecision::Apply {
                changes,
                constraints,
            } => {
                self.validate_changes(&route, &changes, constraints.as_deref())?;
                if let Some(constraints) = constraints
                    && self.constraints != constraints
                {
                    self.constraints = constraints;
                    let jobs = self
                        .jobs
                        .iter()
                        .filter_map(|(id, job)| {
                            (!matches!(job.state, JobState::Finished(_))).then_some(*id)
                        })
                        .collect::<Vec<_>>();
                    for job in jobs {
                        self.invalidate_job_calls(job, effects);
                        self.enqueue_job(job);
                    }
                }
                for change in changes {
                    self.apply_change(route.requester, change, effects);
                }
                self.close_routing(routing, effects);
            }
            KernelDecision::Read(query) => {
                self.validate_read(DeliveryTarget::Routing(routing), &query)?;
                self.read(DeliveryTarget::Routing(routing), query, effects)?;
            }
            KernelDecision::Inquire { job, question } => {
                if question.trim().is_empty()
                    || !self.routing_can_access(&route, job)
                    || self.inquiries.len() >= self.limits.inquiries
                {
                    return Err("invalid routing inquiry".into());
                }
                self.open_inquiry(
                    DeliveryTarget::Routing(routing),
                    job,
                    question,
                    now,
                    effects,
                );
            }
            KernelDecision::Investigate(assignment) => {
                let source = match route.requester {
                    Requester::Inputs => None,
                    Requester::Job { job, .. } => Some(job),
                };
                self.validate_assignments(
                    source,
                    &route.inputs,
                    std::slice::from_ref(&assignment),
                )?;
                if self.active_jobs() >= self.limits.active_jobs {
                    return Err("job capacity is full".into());
                }
                let job = self.create_job(Owner::Routing(routing), assignment, effects);
                self.routings
                    .get_mut(&routing)
                    .expect("routing exists")
                    .state = RoutingState::WaitingJob(job);
            }
            KernelDecision::Clarify(question) => {
                if !matches!(route.requester, Requester::Inputs) || question.trim().is_empty() {
                    return Err("only user input routing may ask for clarification".into());
                }
                self.clarify_routing(routing, question, effects);
            }
        }
        Ok(())
    }

    fn validate_changes(
        &self,
        route: &Routing,
        changes: &[JobChange],
        constraints: Option<&str>,
    ) -> Result<(), String> {
        if constraints.is_some() && !matches!(route.requester, Requester::Inputs) {
            return Err("worker coordination cannot change session constraints".into());
        }
        let creates = changes
            .iter()
            .filter(|change| matches!(change, JobChange::Create(_)))
            .count();
        if self.active_jobs() + creates > self.limits.active_jobs {
            return Err("job capacity is full".into());
        }
        let source = match route.requester {
            Requester::Inputs => None,
            Requester::Job { job, revision } => {
                if self.jobs.get(&job).is_none_or(|entry| {
                    entry.revision != revision || matches!(entry.state, JobState::Finished(_))
                }) {
                    return Err("coordination source changed".into());
                }
                Some(job)
            }
        };
        let mut updated = BTreeSet::new();
        for change in changes {
            match change {
                JobChange::Create(assignment) => self.validate_assignments(
                    source,
                    &route.inputs,
                    std::slice::from_ref(assignment),
                )?,
                JobChange::Update {
                    job,
                    spec,
                    action,
                    inputs,
                    required,
                } => {
                    if !updated.insert(*job) {
                        return Err(format!("job {job} is updated twice"));
                    }
                    let Some(target) = self.jobs.get(job) else {
                        return Err(format!("job {job} does not exist"));
                    };
                    if matches!(target.state, JobState::Finished(_)) {
                        return Err(format!("job {job} is already finished"));
                    }
                    if let Some(source) = source {
                        if !self.owns(source, *job)
                            || spec.is_some()
                            || !inputs.is_empty()
                            || *required
                            || matches!(action, JobAction::Resume)
                        {
                            return Err("worker coordination exceeds its owned tree".into());
                        }
                    } else {
                        if !inputs.iter().all(|id| route.inputs.contains(id)) {
                            return Err("job update cites input outside this routing".into());
                        }
                        if (spec.is_some() || matches!(action, JobAction::Resume))
                            && inputs.is_empty()
                        {
                            return Err("goal changes and resume require current user input".into());
                        }
                    }
                    if let Some(spec) = spec {
                        self.validate_spec(spec)?;
                    }
                }
            }
        }
        for ancestor in &updated {
            if updated
                .iter()
                .any(|descendant| ancestor != descendant && self.owns(*ancestor, *descendant))
            {
                return Err("one decision cannot update both an owner and its descendant".into());
            }
        }
        Ok(())
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
        let depth = source.map_or(1, |job| self.job_depth(job) + 1);
        if depth > self.limits.job_depth {
            return Err("job tree is too deep".into());
        }
        for assignment in assignments {
            self.validate_spec(&assignment.spec)?;
            if !assignment
                .inputs
                .iter()
                .all(|id| allowed_inputs.contains(id))
            {
                return Err("assignment cites input outside its owner's context".into());
            }
            if !assignment.evidence.iter().all(|seq| match source {
                Some(job) => self.can_read_record(job, *seq),
                None => self.public_record(*seq),
            }) {
                return Err("assignment cites inaccessible evidence".into());
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
            Ok(())
        }
    }

    fn apply_change(&mut self, requester: Requester, change: JobChange, effects: &mut Vec<Effect>) {
        match change {
            JobChange::Create(assignment) => {
                let owner = match requester {
                    Requester::Inputs => Owner::User,
                    Requester::Job { job, .. } => Owner::Job(job),
                };
                let inputs = assignment.inputs.clone();
                let job = self.create_job(owner, assignment, effects);
                if matches!(owner, Owner::User) {
                    for input in inputs {
                        self.inputs
                            .get_mut(&input)
                            .expect("validated input exists")
                            .required_jobs
                            .insert(job);
                    }
                }
            }
            JobChange::Update {
                job,
                spec,
                action,
                inputs,
                required,
            } => {
                if let Some(spec) = spec {
                    self.change_spec(job, spec, effects);
                } else if !inputs.is_empty() {
                    self.invalidate_job_call(job, effects);
                }
                for input in &inputs {
                    if !self.jobs[&job].inputs.contains(input) {
                        self.jobs
                            .get_mut(&job)
                            .expect("job exists")
                            .inputs
                            .push(*input);
                        let source = self.inputs[input].accepted_at;
                        self.deliver(
                            DeliveryTarget::Job(job),
                            source,
                            DeliveryKind::Input,
                            effects,
                        );
                    }
                    if required {
                        self.inputs
                            .get_mut(input)
                            .expect("validated input exists")
                            .required_jobs
                            .insert(job);
                    }
                }
                match action {
                    JobAction::Keep => {}
                    JobAction::Pause => self.pause(job, effects),
                    JobAction::Resume => self.resume(job, effects),
                    JobAction::Cancel => self.cancel(job, effects),
                }
                if !matches!(self.jobs[&job].state, JobState::Finished(_)) {
                    self.enqueue_job(job);
                }
            }
        }
    }

    pub(super) fn create_job(
        &mut self,
        owner: Owner,
        assignment: Assignment,
        effects: &mut Vec<Effect>,
    ) -> JobId {
        let id = JobId(self.next_job);
        self.next_job += 1;
        let Assignment {
            spec,
            inputs,
            evidence,
            seed,
        } = assignment;
        self.jobs.insert(
            id,
            Job {
                spec: spec.clone(),
                inputs: inputs.clone(),
                owner,
                revision: 1,
                local_paused: false,
                state: JobState::Ready,
                active_call: None,
                context: JobContext::default(),
                report: None,
            },
        );
        let created = self.record(
            Origin::Kernel,
            RecordBody::JobCreated {
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
            Owner::Routing(routing) => {
                self.routings
                    .get_mut(&routing)
                    .expect("routing owner exists")
                    .records
                    .push(created.seq);
            }
            Owner::User => {}
        }
        self.enqueue_job(id);
        id
    }

    fn close_routing(&mut self, routing: Seq, effects: &mut Vec<Effect>) {
        let Some(route) = self.routings.get(&routing).cloned() else {
            return;
        };
        if matches!(route.state, RoutingState::Closed | RoutingState::Failed(_)) {
            return;
        }
        self.routings
            .get_mut(&routing)
            .expect("routing exists")
            .state = RoutingState::Closed;
        let finished = self.record(
            Origin::Kernel,
            RecordBody::RoutingFinished { routing },
            effects,
        );
        match route.requester {
            Requester::Inputs => {}
            Requester::Job { job, revision } => {
                if self.jobs.get(&job).is_some_and(|entry| {
                    entry.revision == revision && !matches!(entry.state, JobState::Finished(_))
                }) {
                    self.deliver(
                        DeliveryTarget::Job(job),
                        finished.seq,
                        DeliveryKind::Coordination,
                        effects,
                    );
                    if matches!(self.jobs[&job].state, JobState::Waiting(WaitState::Coordination(id)) if id == routing)
                    {
                        self.make_ready(job);
                    } else {
                        self.enqueue_job(job);
                    }
                }
            }
        }
    }

    pub(super) fn invalidate_job_routings(&mut self, job: JobId, effects: &mut Vec<Effect>) {
        let routings = self
            .routings
            .iter()
            .filter_map(|(id, route)| {
                (matches!(route.requester, Requester::Job { job: owner, .. } if owner == job)
                    && !matches!(route.state, RoutingState::Closed | RoutingState::Failed(_)))
                .then_some(*id)
            })
            .collect::<Vec<_>>();
        for routing in routings {
            self.invalidate_routing(routing, effects);
        }
    }

    fn invalidate_routing(&mut self, routing: Seq, effects: &mut Vec<Effect>) {
        let Some(route) = self.routings.get(&routing).cloned() else {
            return;
        };
        if matches!(route.state, RoutingState::Closed) {
            return;
        }
        if let Some(call) = route.active_call {
            self.cancel_call(call, effects);
        }
        let requester = route.requester;
        let stored = self.routings.get_mut(&routing).expect("routing exists");
        stored.active_call = None;
        stored.state = RoutingState::Closed;
        self.routing_ready.retain(|ready| *ready != routing);
        if let Requester::Job { job, revision } = requester
            && self.jobs.get(&job).is_some_and(|entry| {
                entry.revision == revision
                    && matches!(entry.state, JobState::Waiting(WaitState::Coordination(id)) if id == routing)
            })
        {
            self.make_ready(job);
        }

        let inquiries = self
            .inquiries
            .iter()
            .filter_map(|(id, inquiry)| {
                (inquiry.requester == DeliveryTarget::Routing(routing)).then_some(*id)
            })
            .collect::<Vec<_>>();
        for inquiry in inquiries {
            self.settle_inquiry(inquiry, InquiryResult::Cancelled, effects);
        }
        let investigations = self
            .jobs
            .iter()
            .filter_map(|(id, job)| {
                (job.owner == Owner::Routing(routing)
                    && !matches!(job.state, JobState::Finished(_)))
                .then_some(*id)
            })
            .collect::<Vec<_>>();
        for investigation in investigations {
            self.finish_job(
                investigation,
                OutcomeKind::Cancelled,
                Completion::new("coordination request was invalidated"),
                effects,
            );
        }
        self.record(
            Origin::Kernel,
            RecordBody::RoutingFinished { routing },
            effects,
        );
    }

    pub(super) fn fail_routing(
        &mut self,
        routing: Seq,
        message: String,
        effects: &mut Vec<Effect>,
    ) {
        let Some(route) = self.routings.get(&routing).cloned() else {
            return;
        };
        let message = format!("routing {routing} failed: {message}");
        let body = match route.requester {
            Requester::Inputs => RecordBody::InputRoutingFailed {
                inputs: route.inputs.clone(),
                message,
            },
            Requester::Job { .. } => RecordBody::Audit { message },
        };
        let failure = self.record(Origin::Kernel, body, effects);
        self.routings
            .get_mut(&routing)
            .expect("routing exists")
            .state = RoutingState::Failed(failure.seq);
        match route.requester {
            Requester::Inputs => {}
            Requester::Job { job, revision } => {
                if self.jobs.get(&job).is_some_and(|entry| {
                    entry.revision == revision && !matches!(entry.state, JobState::Finished(_))
                }) {
                    self.deliver(
                        DeliveryTarget::Job(job),
                        failure.seq,
                        DeliveryKind::Coordination,
                        effects,
                    );
                    self.make_ready(job);
                }
            }
        }
    }

    fn clarify_routing(&mut self, routing: Seq, question: String, effects: &mut Vec<Effect>) {
        let inputs = self.routings[&routing].inputs.clone();
        let clarification = self.record(
            Origin::Kernel,
            RecordBody::Clarification { inputs, question },
            effects,
        );
        self.routings
            .get_mut(&routing)
            .expect("routing exists")
            .state = RoutingState::WaitingForUser(clarification.seq);
        self.routings
            .get_mut(&routing)
            .expect("routing exists")
            .records
            .push(clarification.seq);
    }

    pub(super) fn open_coordination(
        &mut self,
        job: JobId,
        request: String,
        effects: &mut Vec<Effect>,
    ) {
        let routing = self.peek_seq();
        let record = self.record(
            Origin::Job {
                job,
                revision: self.jobs[&job].revision,
            },
            RecordBody::RoutingStarted {
                inputs: Vec::new(),
                source: Some(job),
            },
            effects,
        );
        debug_assert_eq!(record.seq, routing);
        self.routings.insert(
            routing,
            Routing {
                requester: Requester::Job {
                    job,
                    revision: self.jobs[&job].revision,
                },
                inputs: Vec::new(),
                request: Some(request),
                records: Vec::new(),
                active_call: None,
                state: RoutingState::Ready,
            },
        );
        self.jobs.get_mut(&job).expect("job exists").state =
            JobState::Waiting(WaitState::Coordination(routing));
        self.enqueue_routing(routing);
    }

    pub(super) fn ask_user(&mut self, job: JobId, question: String, effects: &mut Vec<Effect>) {
        let inputs = self.jobs[&job]
            .inputs
            .iter()
            .copied()
            .filter(|input| self.inputs[input].finished.is_none())
            .collect::<Vec<_>>();
        let record = self.record(
            Origin::Job {
                job,
                revision: self.jobs[&job].revision,
            },
            RecordBody::Clarification {
                inputs: inputs.clone(),
                question: question.clone(),
            },
            effects,
        );
        self.attach(job, record.seq);
        self.user_question = Some(job);
        self.jobs.get_mut(&job).expect("job exists").state = JobState::Waiting(WaitState::User {
            question: record.seq,
        });
    }

    pub(super) fn retry(&mut self, input: InputId) {
        let Some(entry) = self.inputs.get(&input) else {
            return;
        };
        let routing = entry.routing;
        let RoutingState::Failed(failure) = self.routings[&routing].state else {
            return;
        };
        let route = self.routings.get_mut(&routing).expect("routing exists");
        route.active_call = None;
        route.records.push(failure);
        self.make_routing_ready(routing);
    }

    pub(super) fn finish_inputs(&mut self, effects: &mut Vec<Effect>) {
        let finished = self
            .inputs
            .iter()
            .filter_map(|(id, input)| {
                if input.finished.is_some()
                    || self.routings[&input.routing].state != RoutingState::Closed
                {
                    return None;
                }
                let outcome = input.required_jobs.iter().try_fold(
                    InputOutcome::Completed,
                    |current, job| match &self.jobs[job].state {
                        JobState::Finished(outcome) => Some(match outcome.kind {
                            OutcomeKind::Failed => InputOutcome::Failed,
                            OutcomeKind::Cancelled if current == InputOutcome::Completed => {
                                InputOutcome::Cancelled
                            }
                            _ => current,
                        }),
                        _ => None,
                    },
                )?;
                Some((*id, outcome))
            })
            .collect::<Vec<_>>();
        for (input, outcome) in finished {
            self.inputs.get_mut(&input).expect("input exists").finished = Some(outcome.clone());
            self.record(
                Origin::Kernel,
                RecordBody::InputFinished { input, outcome },
                effects,
            );
        }
    }

    pub(super) fn has_open_input_routing(&self) -> bool {
        self.routings.values().any(|routing| {
            matches!(routing.requester, Requester::Inputs)
                && !matches!(routing.state, RoutingState::Closed)
        })
    }
}
