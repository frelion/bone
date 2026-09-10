use super::*;

impl Kernel {
    #[cfg(test)]
    pub(crate) fn test_add_user_roots(
        &mut self,
        now: MonoTime,
        assignments: Vec<Assignment>,
        effects: &mut Vec<Effect>,
    ) {
        for assignment in assignments {
            let inputs = assignment.inputs.clone();
            let job = self.create_job(Owner::User, assignment, effects);
            for input in inputs {
                self.inputs
                    .get_mut(&input)
                    .expect("test input exists")
                    .required_jobs
                    .insert(job);
            }
        }
        self.advance(now, effects);
    }

    pub(super) fn reply_target(&self, input: InputId) -> Option<(ReplyTarget, Seq)> {
        let entry = self.inputs.get(&input)?;
        if let RoutingState::WaitingForUser(question) = self.routings[&entry.routing].state {
            return Some((ReplyTarget::Routing(entry.routing), question));
        }
        let job = self.user_question?;
        if entry.finished.is_none()
            && self.jobs[&job].inputs.contains(&input)
            && let JobState::Waiting(WaitState::User { question }) = self.jobs[&job].state
        {
            Some((ReplyTarget::Job(job), question))
        } else {
            None
        }
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
        _now: MonoTime,
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
            KernelDecision::Assign(mut deliveries) => {
                self.validate_routes(&route, &deliveries)?;
                for delivery in &mut deliveries {
                    delivery
                        .inputs
                        .sort_by_key(|input| self.inputs[input].accepted_at);
                }
                deliveries.sort_by_key(|delivery| {
                    delivery
                        .inputs
                        .first()
                        .map(|input| self.inputs[input].accepted_at)
                });
                for delivery in deliveries {
                    self.apply_route(delivery, effects);
                }
                self.close_routing(routing, effects);
            }
            KernelDecision::Read(query) => {
                self.validate_read(DeliveryTarget::Routing(routing), &query)?;
                self.read(DeliveryTarget::Routing(routing), query, effects)?;
            }
            KernelDecision::Clarify(question) => {
                if !matches!(route.requester, Requester::Inputs) || question.trim().is_empty() {
                    return Err("only user input routing may ask for clarification".into());
                }
                self.validate_model_item("clarification question", &question)?;
                self.clarify_routing(routing, question, effects);
            }
        }
        Ok(())
    }

    fn validate_routes(
        &self,
        route: &Routing,
        deliveries: &[crate::RouteDelivery],
    ) -> Result<(), String> {
        if !matches!(route.requester, Requester::Inputs) {
            return Err("only user input routing may assign inputs".into());
        }
        if deliveries.is_empty() {
            return Err("routing must assign every input".into());
        }
        let creates = deliveries
            .iter()
            .filter(|delivery| matches!(delivery.target, crate::RouteTarget::New))
            .count();
        if self.active_jobs() + creates > self.limits.active_jobs {
            return Err("job capacity is full".into());
        }

        let allowed = route.inputs.iter().copied().collect::<BTreeSet<_>>();
        let mut assigned = BTreeSet::new();
        for delivery in deliveries {
            if delivery.inputs.is_empty() || delivery.handoff.trim().is_empty() {
                return Err("each route needs inputs and a non-empty handoff".into());
            }
            self.validate_model_item("route handoff", &delivery.handoff)?;
            for input in &delivery.inputs {
                if !allowed.contains(input) {
                    return Err("route cites input outside this routing".into());
                }
                if !assigned.insert(*input) {
                    return Err(format!("input {input} is assigned twice"));
                }
            }
            if let crate::RouteTarget::Existing(job) = delivery.target {
                let Some(target) = self.jobs.get(&job) else {
                    return Err(format!("job {job} does not exist"));
                };
                if target.owner != Owner::User
                    || target.local_paused
                    || matches!(target.state, JobState::Finished(_))
                {
                    return Err("routing target must be a schedulable user-owned root".into());
                }
            }
        }
        if assigned != allowed {
            return Err("routing must assign every input exactly once".into());
        }
        Ok(())
    }

    fn apply_route(&mut self, delivery: crate::RouteDelivery, effects: &mut Vec<Effect>) {
        let job = match delivery.target {
            crate::RouteTarget::New => {
                let mut assignment = Assignment::new(JobSpec::new(
                    "Handle the assigned user request.",
                    "Handle the assigned user input within its stated boundaries.",
                    "The assigned user input is answered or completed.",
                ));
                assignment.inputs = delivery.inputs.clone();
                self.create_job(Owner::User, assignment, effects)
            }
            crate::RouteTarget::Existing(job) => {
                self.invalidate_job_call(job, effects);
                for input in &delivery.inputs {
                    if !self.jobs[&job].inputs.contains(input) {
                        self.jobs
                            .get_mut(&job)
                            .expect("validated job exists")
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
                }
                job
            }
        };
        for input in &delivery.inputs {
            self.inputs
                .get_mut(input)
                .expect("validated input exists")
                .required_jobs
                .insert(job);
            self.inputs
                .get_mut(input)
                .expect("validated input exists")
                .pending_review_by = Some(job);
        }
        let handoff = self.record(
            Origin::Kernel,
            RecordBody::RoutingHandoff {
                job,
                inputs: delivery.inputs,
                text: delivery.handoff,
                previous: context::latest_handoff(self, job),
            },
            effects,
        );
        self.attach(job, handoff.seq);
        self.enqueue_job(job);
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
            self.validate_model_item("job spec", spec)
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

    pub(super) fn fail_pending_reviews(
        &mut self,
        job: JobId,
        message: String,
        effects: &mut Vec<Effect>,
    ) {
        let mut inputs = self
            .inputs
            .iter()
            .filter_map(|(id, input)| (input.pending_review_by == Some(job)).then_some(*id))
            .collect::<Vec<_>>();
        if inputs.is_empty() {
            return;
        }
        inputs.sort_by_key(|input| self.inputs[input].accepted_at);

        let routing = self.open_input_routing(inputs.clone(), Vec::new(), effects);
        for input in inputs {
            let entry = self.inputs.get_mut(&input).expect("pending input exists");
            entry.routing = routing;
            entry.pending_review_by = None;
            entry.required_jobs.remove(&job);
        }
        self.fail_routing(
            routing,
            format!("assigned worker {job} could not review its new input: {message}"),
            effects,
        );
    }

    pub(super) fn clear_pending_reviews(&mut self, job: JobId) {
        for input in self.inputs.values_mut() {
            if input.pending_review_by == Some(job) {
                input.pending_review_by = None;
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

    pub(super) fn retry(&mut self, input: InputId) -> bool {
        let Some(entry) = self.inputs.get(&input) else {
            return false;
        };
        let routing = entry.routing;
        let RoutingState::Failed(failure) = self.routings[&routing].state else {
            return false;
        };
        let route = self.routings.get_mut(&routing).expect("routing exists");
        route.active_call = None;
        route.records.push(failure);
        self.make_routing_ready(routing);
        true
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
        }) || self
            .inputs
            .values()
            .any(|input| input.pending_review_by.is_some())
    }
}
