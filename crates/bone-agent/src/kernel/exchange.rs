use super::*;

impl Kernel {
    pub(super) fn open_inquiry(
        &mut self,
        requester: DeliveryTarget,
        target: JobId,
        question: String,
        now: MonoTime,
        effects: &mut Vec<Effect>,
    ) {
        let immediate = match &self.jobs[&target].state {
            JobState::Finished(outcome) => Some(InquiryResult::Finished(outcome.as_of)),
            JobState::Ready | JobState::Waiting(_) if self.is_paused(target) => {
                Some(InquiryResult::Unavailable("target is paused".into()))
            }
            JobState::Ready | JobState::Waiting(_) => None,
        };
        if immediate.is_none()
            && (self.inquiries.len() >= self.limits.inquiries
                || self
                    .inquiries
                    .values()
                    .any(|inquiry| inquiry.requester == requester && inquiry.target == target))
        {
            let record = self.record(
                Origin::Kernel,
                RecordBody::Audit {
                    message: "inquiry was not accepted".into(),
                },
                effects,
            );
            self.deliver(requester, record.seq, DeliveryKind::InquiryResult, effects);
            return;
        }
        let id = self.peek_seq();
        let record = self.record(
            Origin::Kernel,
            RecordBody::Inquiry {
                requester,
                target,
                question,
            },
            effects,
        );
        debug_assert_eq!(record.seq, id);
        if let Some(result) = immediate {
            self.record_inquiry_result(requester, id, result, effects);
            return;
        }
        self.inquiries.insert(
            id,
            Inquiry {
                requester,
                target,
                target_revision: self.jobs[&target].revision,
                deadline: now.after(self.limits.coordination_timeout),
            },
        );
        match requester {
            DeliveryTarget::Job(job) => {
                self.jobs.get_mut(&job).expect("requester exists").state =
                    JobState::Waiting(WaitState::Inquiry(id));
            }
            DeliveryTarget::Routing(routing) => {
                self.routings
                    .get_mut(&routing)
                    .expect("requester exists")
                    .state = RoutingState::WaitingInquiry(id);
            }
        }
        self.deliver(
            DeliveryTarget::Job(target),
            record.seq,
            DeliveryKind::Inquiry,
            effects,
        );
    }

    pub(super) fn settle_inquiry(
        &mut self,
        id: Seq,
        result: InquiryResult,
        effects: &mut Vec<Effect>,
    ) {
        let Some(inquiry) = self.inquiries.remove(&id) else {
            return;
        };
        self.record_inquiry_result(inquiry.requester, id, result, effects);
        match inquiry.requester {
            DeliveryTarget::Job(job) => {
                if matches!(self.jobs.get(&job).map(|job| &job.state), Some(JobState::Waiting(WaitState::Inquiry(waiting))) if *waiting == id)
                {
                    self.make_ready(job);
                }
            }
            DeliveryTarget::Routing(routing) => {
                if self.routings.get(&routing).is_some_and(|route| {
                    matches!(route.state, RoutingState::WaitingInquiry(waiting) if waiting == id)
                }) {
                    self.make_routing_ready(routing);
                }
            }
        }
    }

    fn record_inquiry_result(
        &mut self,
        requester: DeliveryTarget,
        inquiry: Seq,
        result: InquiryResult,
        effects: &mut Vec<Effect>,
    ) {
        let record = self.record(
            Origin::Kernel,
            RecordBody::InquirySettled { inquiry, result },
            effects,
        );
        self.deliver(requester, record.seq, DeliveryKind::InquiryResult, effects);
    }

    pub(super) fn validate_read(
        &self,
        requester: DeliveryTarget,
        query: &ReadQuery,
    ) -> Result<(), String> {
        match (requester, query) {
            (DeliveryTarget::Job(job), ReadQuery::Jobs { parent, .. }) => {
                if parent.is_some_and(|parent| parent != job) {
                    Err("worker may list only its direct children".into())
                } else {
                    Ok(())
                }
            }
            (DeliveryTarget::Job(job), ReadQuery::Job(target)) => self
                .can_access_job(job, *target)
                .then_some(())
                .ok_or_else(|| "worker cannot read that job".into()),
            (DeliveryTarget::Job(job), ReadQuery::Record { id, offset }) => {
                if !self.can_read_record(job, *id) {
                    return Err("worker cannot read that record".into());
                }
                context::view(&self.records[id], *offset, self.limits.item_bytes)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }
            (DeliveryTarget::Routing(routing), ReadQuery::Jobs { parent, .. }) => {
                let route = &self.routings[&routing];
                if let (Requester::Job { job, .. }, Some(parent)) = (route.requester, parent)
                    && !self.owns(job, *parent)
                {
                    return Err("worker routing cannot list an unrelated tree".into());
                }
                Ok(())
            }
            (DeliveryTarget::Routing(routing), ReadQuery::Job(target)) => self
                .routing_can_access(&self.routings[&routing], *target)
                .then_some(())
                .ok_or_else(|| "routing cannot read that job".into()),
            (DeliveryTarget::Routing(routing), ReadQuery::Record { id, offset }) => {
                let route = &self.routings[&routing];
                let allowed = route.records.contains(id)
                    || self.public_record(*id)
                    || matches!(route.requester, Requester::Job { job, .. } if self.can_read_record(job, *id));
                if !allowed {
                    return Err("routing cannot read that record".into());
                }
                context::view(&self.records[id], *offset, self.limits.item_bytes)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }
        }
    }

    pub(super) fn read(
        &mut self,
        requester: DeliveryTarget,
        query: ReadQuery,
        effects: &mut Vec<Effect>,
    ) {
        let mut jobs = Vec::new();
        let mut record_range = None;
        let mut next_job = None;
        match &query {
            ReadQuery::Jobs { parent, after } => {
                let mut ids = self
                    .jobs
                    .iter()
                    .filter_map(|(id, job)| {
                        let belongs = match parent {
                            Some(parent) => {
                                matches!(job.owner, Owner::Job(owner) if owner == *parent)
                            }
                            None => {
                                matches!(job.owner, Owner::User)
                                    && !matches!(job.state, JobState::Finished(_))
                            }
                        };
                        (belongs && after.is_none_or(|after| *id > after)).then_some(*id)
                    })
                    .filter(|id| match requester {
                        DeliveryTarget::Job(job) => self.can_access_job(job, *id),
                        DeliveryTarget::Routing(routing) => {
                            self.routing_can_access(&self.routings[&routing], *id)
                        }
                    })
                    .take(context::DIRECTORY_PAGE + 1)
                    .collect::<Vec<_>>();
                if ids.len() > context::DIRECTORY_PAGE {
                    ids.pop();
                    next_job = ids.last().copied();
                }
                jobs = ids.into_iter().map(|id| context::card(self, id)).collect();
            }
            ReadQuery::Job(job) => jobs.push(context::card(self, *job)),
            ReadQuery::Record { id, offset } => {
                record_range = Some(RecordRange {
                    source: *id,
                    offset: *offset,
                });
            }
        }
        let record = self.record(
            Origin::Kernel,
            RecordBody::ReadResult {
                requester,
                query,
                next_job,
                jobs,
                record: record_range,
            },
            effects,
        );
        match requester {
            DeliveryTarget::Job(job) => {
                self.attach(job, record.seq);
                self.enqueue_job(job);
            }
            DeliveryTarget::Routing(routing) => {
                self.routings
                    .get_mut(&routing)
                    .expect("routing exists")
                    .records
                    .push(record.seq);
                self.make_routing_ready(routing);
            }
        }
    }

    pub(super) fn attach(&mut self, job: JobId, record: Seq) {
        if let Some(entry) = self.jobs.get_mut(&job)
            && !matches!(entry.state, JobState::Finished(_))
        {
            entry.context.records.push_back(record);
        }
    }

    pub(super) fn deliver(
        &mut self,
        target: DeliveryTarget,
        source: Seq,
        kind: DeliveryKind,
        effects: &mut Vec<Effect>,
    ) {
        if matches!(target, DeliveryTarget::Job(job) if self.jobs.get(&job).is_none_or(|entry| matches!(entry.state, JobState::Finished(_))))
            || matches!(target, DeliveryTarget::Routing(routing) if self.routings.get(&routing).is_none_or(|entry| matches!(entry.state, RoutingState::Closed | RoutingState::Failed(_))))
        {
            return;
        }
        let record = self.record(
            Origin::Kernel,
            RecordBody::Delivery {
                to: target,
                source,
                kind,
            },
            effects,
        );
        match target {
            DeliveryTarget::Job(job) => {
                let entry = self.jobs.get_mut(&job).expect("delivery target exists");
                entry.context.records.push_back(record.seq);
                if matches!(entry.state, JobState::Waiting(WaitState::Commit(_))) {
                    entry.state = JobState::Ready;
                }
                self.enqueue_job(job);
            }
            DeliveryTarget::Routing(routing) => {
                let route = self
                    .routings
                    .get_mut(&routing)
                    .expect("delivery target exists");
                route.records.push(record.seq);
                self.make_routing_ready(routing);
            }
        }
    }

    pub(super) fn latest_result(&self, job: JobId, after: Seq) -> Option<Seq> {
        self.records.iter().rev().find_map(|(seq, record)| {
            (*seq > after
                && matches!(record.body, RecordBody::Published { job: producer, .. } if producer == job))
            .then_some(*seq)
        })
    }

    pub(super) fn wake_result_waiters(
        &mut self,
        producer: JobId,
        result: Seq,
        effects: &mut Vec<Effect>,
    ) {
        let waiting = self
            .jobs
            .iter()
            .filter_map(|(id, job)| match job.state {
                JobState::Waiting(WaitState::Result {
                    job: target, after, ..
                }) if target == producer && result > after => Some(*id),
                _ => None,
            })
            .collect::<Vec<_>>();
        for job in waiting {
            self.deliver(
                DeliveryTarget::Job(job),
                result,
                DeliveryKind::Result,
                effects,
            );
            self.make_ready(job);
        }
    }

    pub(super) fn deliver_outcome(
        &mut self,
        target: JobId,
        source_job: JobId,
        effects: &mut Vec<Effect>,
    ) {
        let outcome = self.records.iter().rev().find_map(|(seq, record)| {
            matches!(record.body, RecordBody::Outcome { job, .. } if job == source_job)
                .then_some(*seq)
        });
        if let Some(outcome) = outcome {
            self.deliver(
                DeliveryTarget::Job(target),
                outcome,
                DeliveryKind::Outcome,
                effects,
            );
        }
    }

    pub(super) fn can_access_job(&self, source: JobId, target: JobId) -> bool {
        source == target || self.owns(source, target)
    }

    pub(super) fn routing_can_access(&self, route: &Routing, target: JobId) -> bool {
        match route.requester {
            Requester::Inputs => self.jobs.contains_key(&target),
            Requester::Job { job, .. } => self.can_access_job(job, target),
        }
    }

    pub(super) fn owns(&self, root: JobId, target: JobId) -> bool {
        let mut current = Some(target);
        while let Some(job) = current {
            if job == root {
                return true;
            }
            current = match self.jobs.get(&job).map(|entry| entry.owner) {
                Some(Owner::Job(parent)) => Some(parent),
                _ => None,
            };
        }
        false
    }

    pub(super) fn is_investigation(&self, job: JobId) -> bool {
        let mut current = job;
        loop {
            match self.jobs[&current].owner {
                Owner::Routing(_) => return true,
                Owner::Job(parent) => current = parent,
                Owner::User => return false,
            }
        }
    }

    pub(super) fn can_read_record(&self, job: JobId, seq: Seq) -> bool {
        let Some(record) = self.records.get(&seq) else {
            return false;
        };
        if self.jobs[&job].context.records.contains(&seq)
            || matches!(record.origin, Origin::Job { job: owner, .. } if owner == job)
            || matches!(record.origin, Origin::Call(call) if self.calls.get(&call).is_some_and(|entry| entry.job() == Some(job)))
        {
            return true;
        }
        if self.jobs[&job].context.records.iter().any(|delivery| {
            self.records.get(delivery).is_some_and(|record| {
                matches!(record.body, RecordBody::Delivery { source, .. } if source == seq)
                    || matches!(record.body, RecordBody::ReadResult { record: Some(RecordRange { source, .. }), .. } if source == seq)
            })
        }) {
            return true;
        }
        match &record.body {
            RecordBody::Report { job: owner, .. }
            | RecordBody::Published { job: owner, .. }
            | RecordBody::Outcome { job: owner, .. } => self.owns(job, *owner),
            _ => false,
        }
    }

    pub(super) fn public_record(&self, seq: Seq) -> bool {
        self.records.get(&seq).is_some_and(|record| {
            matches!(
                record.body,
                RecordBody::Report { .. }
                    | RecordBody::Published { .. }
                    | RecordBody::Outcome { .. }
            )
        })
    }

    pub(super) fn can_seed(&self, source: Option<JobId>, seed: JobId) -> bool {
        let Some(job) = self.jobs.get(&seed) else {
            return false;
        };
        if !matches!(job.state, JobState::Finished(ref outcome) if outcome.kind == OutcomeKind::Completed)
        {
            return false;
        }
        match source {
            Some(source) => self.owns(source, seed),
            None => matches!(job.owner, Owner::User),
        }
    }

    pub(super) fn job_depth(&self, job: JobId) -> usize {
        let mut depth = 1;
        let mut current = job;
        while let Owner::Job(parent) = self.jobs[&current].owner {
            depth += 1;
            current = parent;
        }
        depth
    }

    pub(super) fn active_jobs(&self) -> usize {
        self.jobs
            .values()
            .filter(|job| !matches!(job.state, JobState::Finished(_)))
            .count()
    }

    pub(super) fn would_cycle(&self, source: JobId, target: JobId) -> bool {
        let mut current = Some(target);
        for _ in 0..self.limits.active_jobs {
            let Some(job) = current else {
                return false;
            };
            if job == source {
                return true;
            }
            current = match &self.jobs[&job].state {
                JobState::Waiting(WaitState::Job { job, .. })
                | JobState::Waiting(WaitState::Result { job, .. }) => Some(*job),
                JobState::Waiting(WaitState::Inquiry(inquiry)) => {
                    self.inquiries.get(inquiry).map(|item| item.target)
                }
                _ => None,
            };
        }
        true
    }
}
