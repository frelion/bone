use super::*;
use crate::{DurableError, DurableSnapshot};

impl Kernel {
    pub(crate) fn durable_snapshot(&self) -> Result<DurableSnapshot, DurableError> {
        Ok(DurableSnapshot {
            version: 1,
            through: Seq(self.next_seq - 1),
            epoch: self.epoch,
            payload: serde_json::to_value(self)
                .map_err(|error| DurableError::Invalid(error.to_string()))?,
        })
    }

    /// Reconstitute facts, then interrupt old execution without scheduling any calls.
    /// Recovery effects and the resulting snapshot must be committed before use.
    pub(crate) fn restore(
        snapshot: DurableSnapshot,
        records: Vec<Arc<Record>>,
        limits: AgentLimits,
        tools: Vec<ToolSpec>,
    ) -> Result<(Self, Vec<Effect>), DurableError> {
        if snapshot.version != 1 {
            return Err(DurableError::UnsupportedVersion(snapshot.version));
        }
        let configured =
            Self::new(limits, tools).map_err(|error| DurableError::Invalid(error.to_string()))?;
        let mut kernel: Self = serde_json::from_value(snapshot.payload)
            .map_err(|error| DurableError::Invalid(error.to_string()))?;
        if kernel.next_seq.checked_sub(1) != Some(snapshot.through.0)
            || kernel.epoch != snapshot.epoch
            || records.len() as u64 != snapshot.through.0
            || records
                .iter()
                .enumerate()
                .any(|(index, record)| record.seq.0 != index as u64 + 1)
        {
            return Err(DurableError::Invalid(
                "record history does not match snapshot".into(),
            ));
        }
        kernel.records = records
            .into_iter()
            .map(|record| (record.seq, record))
            .collect();
        kernel.limits = configured.limits;
        kernel.tools = configured.tools;
        kernel.epoch = kernel
            .epoch
            .checked_add(1)
            .ok_or_else(|| DurableError::Invalid("epoch exhausted".into()))?;
        if kernel.jobs.keys().any(|id| id.0 >= kernel.next_job)
            || kernel.calls.keys().any(|id| id.0 >= kernel.next_call)
            || kernel.inputs.iter().any(|(id, input)| {
                !matches!(kernel.records.get(&input.accepted_at).map(|record| &record.body), Some(RecordBody::Input(value)) if value.id == *id)
                    || !kernel.routings.contains_key(&input.routing)
                    || input.required_jobs.iter().any(|job| !kernel.jobs.contains_key(job))
                    || input.pending_review_by.is_some_and(|job| !kernel.jobs.contains_key(&job))
            })
            || kernel.jobs.values().any(|job| {
                job.inputs.iter().any(|input| !kernel.inputs.contains_key(input))
                    || job.context.records.iter().any(|seq| !kernel.records.contains_key(seq))
                    || matches!(job.owner, Owner::Job(parent) if !kernel.jobs.contains_key(&parent))
            })
        {
            return Err(DurableError::Invalid("dangling state references or counters".into()));
        }
        let mut effects = Vec::new();
        let active = kernel
            .calls
            .iter()
            .filter(|(_, call)| call.running())
            .map(|(id, call)| (*id, call.task.clone()))
            .collect::<Vec<_>>();
        for (id, task) in active {
            let state = match task {
                CallTask::Tool {
                    job,
                    effect,
                    request,
                    ..
                } => {
                    let outcome = Arc::new(ToolOutcome {
                        result: Err(CallError::failed("execution interrupted by restore")),
                        external_effect: if effect == ToolEffect::ExternalWrite {
                            ExternalEffect::Unknown
                        } else {
                            ExternalEffect::None
                        },
                    });
                    kernel.record(
                        Origin::Kernel,
                        RecordBody::ToolFinished {
                            job,
                            call: id,
                            request,
                            outcome: Arc::clone(&outcome),
                        },
                        &mut effects,
                    );
                    CallState::ToolFinished(outcome)
                }
                _ => CallState::ModelFinished(Some(CallError::failed(
                    "execution interrupted by restore",
                ))),
            };
            kernel.calls.get_mut(&id).expect("call exists").state = state;
        }
        for job in kernel.jobs.values_mut() {
            job.active_call = None;
        }
        for route in kernel.routings.values_mut() {
            route.active_call = None;
        }
        let jobs = kernel
            .jobs
            .iter()
            .filter(|(_, job)| !matches!(job.state, JobState::Finished(_)))
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        for job in jobs {
            kernel.finish_job(
                job,
                OutcomeKind::Failed,
                Completion::new("work interrupted by restore; start new work to continue"),
                &mut effects,
            );
        }
        let routes = kernel
            .routings
            .iter()
            .filter(|(_, route)| {
                !matches!(route.state, RoutingState::Closed | RoutingState::Failed(_))
            })
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        for route in routes {
            kernel.fail_routing(route, "routing interrupted by restore".into(), &mut effects);
        }
        kernel.finish_inputs(&mut effects);
        kernel.interactive_ready.clear();
        kernel.background_ready.clear();
        kernel.routing_ready.clear();
        Ok((kernel, effects))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn records(kernel: &Kernel) -> Vec<Arc<Record>> {
        kernel.records.values().cloned().collect()
    }

    #[test]
    fn restore_rejects_removed_host_background_field() {
        let kernel = Kernel::new(AgentLimits::default(), vec![]).unwrap();
        let mut snapshot = kernel.durable_snapshot().unwrap();
        snapshot.payload["background"] = serde_json::json!({"entries": [], "omitted": false});
        assert!(matches!(
            Kernel::restore(snapshot, records(&kernel), AgentLimits::default(), vec![]),
            Err(DurableError::Invalid(_))
        ));
    }

    #[test]
    fn snapshot_excludes_records_and_restore_preserves_ids_without_running_old_work() {
        let mut kernel = Kernel::new(AgentLimits::default(), vec![]).unwrap();
        kernel
            .accept(
                MonoTime::default(),
                Input::new(InputId(1), "remember the original request"),
            )
            .unwrap();
        let snapshot = kernel.durable_snapshot().unwrap();
        assert!(snapshot.payload.get("records").is_none());
        assert!(snapshot.payload.get("background").is_none());
        let snapshot: DurableSnapshot =
            serde_json::from_str(&serde_json::to_string(&snapshot).unwrap()).unwrap();
        let old_call = CallId(kernel.next_call - 1);
        let old_seq = kernel.next_seq;
        let (mut restored, effects) =
            Kernel::restore(snapshot, records(&kernel), AgentLimits::default(), vec![]).unwrap();
        assert_eq!(restored.epoch, 1);
        assert!(restored.next_seq >= old_seq);
        assert_eq!(restored.next_call, kernel.next_call);
        assert!(
            effects
                .iter()
                .all(|effect| matches!(effect, Effect::Notify(_)))
        );
        assert!(restored.calls.values().all(|call| !call.running()));
        assert_eq!(
            restored.input(kernel.inputs[&InputId(1)].accepted_at).text,
            "remember the original request"
        );
        restored.step(
            MonoTime::default(),
            Event::CoordinateFinished {
                call: old_call,
                result: Ok(KernelDecision::Assign(vec![crate::RouteDelivery {
                    inputs: vec![InputId(1)],
                    target: crate::RouteTarget::New,
                    handoff: "stale".into(),
                }])),
            },
        );
        assert!(restored.jobs.is_empty());
    }

    #[test]
    fn restore_marks_running_external_write_unknown_and_keeps_existing_unknown() {
        let mut kernel = Kernel::new(AgentLimits::default(), vec![]).unwrap();
        kernel
            .accept(MonoTime::default(), Input::new(InputId(1), "write"))
            .unwrap();
        let routing = kernel.inputs[&InputId(1)].routing;
        kernel
            .apply_decision(
                MonoTime::default(),
                routing,
                KernelDecision::Assign(vec![crate::RouteDelivery {
                    inputs: vec![InputId(1)],
                    target: crate::RouteTarget::New,
                    handoff: "write".into(),
                }]),
                &mut vec![],
            )
            .unwrap();
        let job = *kernel.jobs.keys().next().unwrap();
        let call = CallId(kernel.next_call);
        kernel.next_call += 1;
        kernel.calls.insert(
            call,
            CallEntry {
                task: CallTask::Tool {
                    job,
                    revision: 1,
                    effect: ToolEffect::ExternalWrite,
                    request: Arc::new(crate::ToolCall::new("write", serde_json::json!({}))),
                    output_bytes: 100,
                },
                state: CallState::Running,
                progress: None,
            },
        );
        let (restored, effects) = Kernel::restore(
            kernel.durable_snapshot().unwrap(),
            records(&kernel),
            AgentLimits::default(),
            vec![],
        )
        .unwrap();
        assert_eq!(
            restored.calls[&call].external_effect(),
            ExternalEffect::Unknown
        );
        assert!(matches!(restored.jobs[&job].state, JobState::Finished(_)));
        assert!(restored.inputs[&InputId(1)].pending_review_by.is_none());
        assert!(
            effects
                .iter()
                .all(|effect| matches!(effect, Effect::Notify(_)))
        );
        let (again, _) = Kernel::restore(
            restored.durable_snapshot().unwrap(),
            records(&restored),
            AgentLimits::default(),
            vec![],
        )
        .unwrap();
        assert_eq!(
            again.calls[&call].external_effect(),
            ExternalEffect::Unknown
        );
        assert_eq!(again.next_call, restored.next_call);
    }

    #[test]
    fn restore_rejects_wrong_version_or_missing_records() {
        let mut kernel = Kernel::new(AgentLimits::default(), vec![]).unwrap();
        kernel
            .accept(MonoTime::default(), Input::new(InputId(1), "input"))
            .unwrap();
        assert!(
            Kernel::restore(
                kernel.durable_snapshot().unwrap(),
                vec![],
                AgentLimits::default(),
                vec![],
            )
            .is_err()
        );
        let mut snapshot = kernel.durable_snapshot().unwrap();
        snapshot.version = 999;
        assert!(matches!(
            Kernel::restore(snapshot, records(&kernel), AgentLimits::default(), vec![],),
            Err(DurableError::UnsupportedVersion(999))
        ));
    }
}
