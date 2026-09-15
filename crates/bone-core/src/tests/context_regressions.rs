use super::*;

#[test]
fn local_control_revokes_session_compaction_without_stranding_other_waiters() {
    for ancestor in [false, true] {
        for cancel in [false, true] {
            for success in [false, true] {
                let mut kernel = kernel_with_session_history();
                let first = Input::new(InputId(9940), "work needing context");
                let (call, root) = create_roots(
                    &mut kernel,
                    first.clone(),
                    vec![assignment("root", &[first.id])],
                )
                .pop()
                .unwrap();
                let (requester_call, requester) = if ancestor {
                    let effects = work(
                        &mut kernel,
                        call,
                        WorkStep::delegate(vec![assignment("child", &[])]),
                    );
                    let (call, child) = work_calls(&effects)
                        .into_iter()
                        .find(|(_, job)| job.job != root.job)
                        .unwrap();
                    (call, child.job)
                } else {
                    (call, root.job)
                };
                let second = Input::new(InputId(9941), "independent waiting work");
                let (other_call, other) = create_roots(
                    &mut kernel,
                    second.clone(),
                    vec![assignment("other", &[second.id])],
                )
                .pop()
                .unwrap();
                kernel.limits.context_bytes = 2800;
                kernel.limits.item_bytes = 512;
                let effects = work(&mut kernel, requester_call, WorkStep::Continue);
                let (compaction, compact) =
                    compact_call(&effects).expect("requester needs session compaction");
                assert_eq!(compact.scope, crate::CompactScope::Session);
                let effects = work(&mut kernel, other_call, WorkStep::Continue);
                assert!(
                    compact_call(&effects).is_none(),
                    "other job shares pending compaction"
                );
                let command = if cancel {
                    KernelControl::Cancel(root.job)
                } else {
                    KernelControl::Pause(root.job)
                };
                let (_, effects) = kernel.control(NOW, command);
                assert!(
                    effects
                        .iter()
                        .any(|effect| matches!(effect, Effect::Cancel(id) if *id == compaction))
                );
                let result = if success {
                    Ok(CheckpointDraft {
                        summary: "late old summary".into(),
                        evidence: vec![],
                    })
                } else {
                    Err(crate::CallError::failed("late compactor failure"))
                };
                let effects = kernel.step(
                    NOW,
                    Event::CompactFinished {
                        call: compaction,
                        result,
                    },
                );
                assert!(context::session_checkpoint(&kernel).is_none());
                if cancel {
                    assert!(finished_as(&kernel, requester, OutcomeKind::Cancelled));
                } else {
                    assert!(!matches!(
                        kernel.job_status(requester),
                        JobStatus::Finished(_)
                    ));
                }
                assert!(!matches!(
                    kernel.job_status(other.job),
                    JobStatus::Finished(_)
                ));
                assert!(
                    compact_call(&effects).is_some(),
                    "other waiter starts a fresh compaction"
                );
            }
        }
    }
}

#[test]
fn revoked_session_compaction_cannot_fail_its_requester_or_install_a_checkpoint() {
    for suspend in [false, true] {
        for success in [false, true] {
            for worker_requester in [false, true] {
                let mut kernel = kernel_with_session_history();
                let input = Input::new(InputId(9900), "new work");
                let mut worker = None;
                let effects = if worker_requester {
                    let (call, work_input) = create_roots(
                        &mut kernel,
                        input.clone(),
                        vec![assignment("new work", &[input.id])],
                    )
                    .pop()
                    .unwrap();
                    worker = Some(work_input.job);
                    kernel.limits.context_bytes = 2800;
                    kernel.limits.item_bytes = 512;
                    work(&mut kernel, call, WorkStep::Continue)
                } else {
                    kernel.limits.context_bytes = 2800;
                    kernel.limits.item_bytes = 512;
                    kernel.accept(NOW, input.clone()).unwrap().1
                };
                let (call, compact) = compact_call(&effects).expect("history needs compaction");
                assert_eq!(compact.scope, crate::CompactScope::Session);
                let effects = if suspend {
                    kernel.suspend(NOW)
                } else {
                    kernel
                        .reconfigure(NOW, kernel.limits.clone(), vec![])
                        .unwrap()
                };
                assert!(
                    effects
                        .iter()
                        .any(|effect| matches!(effect, Effect::Cancel(id) if *id == call))
                );
                let result = if success {
                    Ok(CheckpointDraft {
                        summary: "old history".into(),
                        evidence: vec![],
                    })
                } else {
                    Err(crate::CallError {
                        kind: crate::CallErrorKind::Cancelled,
                        message: "cancelled".into(),
                    })
                };
                let effects = kernel.step(NOW, Event::CompactFinished { call, result });
                assert!(context::session_checkpoint(&kernel).is_none());
                assert!(!matches!(
                    input_status(&kernel, input.id),
                    InputStatus::RoutingFailed { .. }
                ));
                if let Some(job) = worker {
                    assert!(!matches!(kernel.job_status(job), JobStatus::Finished(_)));
                }
                let effects = if suspend {
                    kernel.resume_scheduling(NOW)
                } else {
                    effects
                };
                assert!(
                    compact_call(&effects).is_some(),
                    "revoked compression is rescheduled"
                );
            }
        }
    }
}

#[test]
fn root_directory_exposes_readable_bounded_assignment_history() {
    let mut kernel = kernel();
    let first = Input::new(InputId(9910), "fix parser");
    let (_, effects) = kernel.accept(NOW, first.clone()).unwrap();
    let effects = kernel.step(
        NOW,
        Event::CoordinateFinished {
            call: coordinate_call(&effects),
            result: Ok(route_new("parser request", &[first.id])),
        },
    );
    let (_, root) = work_calls(&effects).pop().unwrap();
    let first_handoff = context::card(&kernel, root.job).latest_handoff.unwrap();
    let correction = Input::new(InputId(9911), "include lexer");
    let (_, effects) = kernel.accept(NOW, correction.clone()).unwrap();
    route_existing(&mut kernel, &effects, root.job, &[correction.id]);
    let latest = context::card(&kernel, root.job).latest_handoff.unwrap();
    assert!(
        matches!(&kernel.records[&latest].body, RecordBody::RoutingHandoff { inputs, previous, .. }
        if inputs == &[correction.id] && *previous == Some(first_handoff))
    );
    let (_, effects) = kernel
        .accept(NOW, Input::new(InputId(9912), "adjust parser again"))
        .unwrap();
    let (_, Call::Coordinate(directory)) = starts(&effects).next().unwrap() else {
        panic!("router")
    };
    assert_eq!(directory.jobs[0].latest_handoff, Some(latest));
    assert!(serde_json::to_vec(directory).unwrap().len() <= kernel.limits.context_bytes);
    let mut call = coordinate_call(&effects);
    for (source, expected_input) in [(latest, correction.id), (first_handoff, first.id)] {
        let effects = kernel.step(
            NOW,
            Event::CoordinateFinished {
                call,
                result: Ok(KernelDecision::Read(ReadQuery::Record {
                    id: source,
                    offset: 0,
                })),
            },
        );
        let (_, Call::Coordinate(read)) = starts(&effects).next().unwrap() else {
            panic!("router read")
        };
        assert!(read.records.iter().any(|record| {
            record.source == source
                && matches!(serde_json::from_str::<RecordBody>(&record.content),
                Ok(RecordBody::RoutingHandoff { inputs, .. }) if inputs.contains(&expected_input))
        }));
        call = coordinate_call(&effects);
    }
}

#[test]
fn routing_handoff_read_permission_does_not_expose_child_records() {
    let mut kernel = kernel();
    let input = Input::new(InputId(9915), "delegate work");
    let (call, root) = create_roots(
        &mut kernel,
        input.clone(),
        vec![assignment("root", &[input.id])],
    )
    .pop()
    .unwrap();
    let effects = work(
        &mut kernel,
        call,
        WorkStep::delegate(vec![assignment("child", &[])]),
    );
    let child = work_calls(&effects)
        .into_iter()
        .find(|(_, work)| work.job != root.job)
        .unwrap()
        .1
        .job;
    let source = context::card(&kernel, root.job).latest_handoff.unwrap();
    // Simulate a historical handoff owned by a child; the record kind alone
    // must not grant the new router access to a private job's intent.
    let mut record = kernel.records[&source].as_ref().clone();
    if let RecordBody::RoutingHandoff { job, .. } = &mut record.body {
        *job = child;
    }
    kernel.records.insert(source, std::sync::Arc::new(record));
    let next = Input::new(InputId(9916), "unrelated work");
    let (_, effects) = kernel.accept(NOW, next.clone()).unwrap();
    kernel.step(
        NOW,
        Event::CoordinateFinished {
            call: coordinate_call(&effects),
            result: Ok(KernelDecision::Read(ReadQuery::Record {
                id: source,
                offset: 0,
            })),
        },
    );
    assert!(matches!(
        input_status(&kernel, next.id),
        InputStatus::RoutingFailed { .. }
    ));
}

#[test]
fn user_questions_survive_session_projection_and_can_be_read_by_later_routing() {
    for worker_question in [false, true] {
        let mut kernel = kernel();
        let original = Input::new(InputId(9920), "choose a destination");
        let (_, effects) = kernel.accept(NOW, original.clone()).unwrap();
        let question = "First: Paris. Second: London. Which destination?";
        if worker_question {
            let effects = kernel.step(
                NOW,
                Event::CoordinateFinished {
                    call: coordinate_call(&effects),
                    result: Ok(route_new("choose destination", &[original.id])),
                },
            );
            work(
                &mut kernel,
                work_calls(&effects)[0].0,
                WorkStep::AskUser(question.into()),
            );
        } else {
            kernel.step(
                NOW,
                Event::CoordinateFinished {
                    call: coordinate_call(&effects),
                    result: Ok(KernelDecision::Clarify(question.into())),
                },
            );
        }
        let question_seq = kernel
            .records
            .values()
            .find_map(|record| {
                matches!(record.body, RecordBody::Clarification { .. }).then_some(record.seq)
            })
            .unwrap();
        let (_, reply_effects) = kernel
            .accept(
                NOW,
                Input::new(InputId(9921), "second").answering(original.id, question_seq),
            )
            .unwrap();
        let (_, effects) = kernel
            .accept(NOW, Input::new(InputId(9922), "use that choice again"))
            .unwrap();
        // A prior router call may need to retire before the newest routing starts.
        let effects = if starts(&effects).any(|(_, call)| matches!(call, Call::Coordinate(_))) {
            effects
        } else {
            let call = coordinate_call(&reply_effects);
            kernel.step(
                NOW,
                Event::CoordinateFinished {
                    call,
                    result: Ok(KernelDecision::Clarify("stale".into())),
                },
            )
        };
        let (_, Call::Coordinate(coordinate)) = starts(&effects).next().unwrap() else {
            panic!("router")
        };
        let inputs = coordinate
            .inputs
            .iter()
            .map(|input| input.id)
            .collect::<Vec<_>>();
        kernel.step(
            NOW,
            Event::CoordinateFinished {
                call: coordinate_call(&effects),
                result: Ok(route_new("use the choice", &inputs)),
            },
        );
        let (_, effects) = kernel
            .accept(NOW, Input::new(InputId(9923), "remember the destination"))
            .unwrap();
        let (_, Call::Coordinate(coordinate)) = starts(&effects).next().unwrap() else {
            panic!("new router")
        };
        assert!(
            serde_json::to_string(&coordinate.background)
                .unwrap()
                .contains(question)
        );
        let compact = context::prepare_session_compact(&kernel).unwrap();
        assert!(
            compact
                .records
                .iter()
                .any(|record| record.source == question_seq)
        );
        let effects = kernel.step(
            NOW,
            Event::CoordinateFinished {
                call: coordinate_call(&effects),
                result: Ok(KernelDecision::Read(ReadQuery::Record {
                    id: question_seq,
                    offset: 0,
                })),
            },
        );
        let (_, Call::Coordinate(read)) = starts(&effects).next().unwrap() else {
            panic!("router read")
        };
        assert!(
            read.records
                .iter()
                .any(|record| record.source == question_seq && record.content.contains(question))
        );
    }
}
