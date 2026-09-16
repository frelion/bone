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
                    InputStatus::ConversationFailed { .. }
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
        Event::ConverseFinished {
            call: converse_call(&effects),
            result: Ok(start_jobs("parser request", &[first.id])),
        },
    );
    let (_, root) = work_calls(&effects).pop().unwrap();
    let correction = Input::new(InputId(9911), "include lexer");
    let (_, effects) = kernel.accept(NOW, correction.clone()).unwrap();
    send_existing(&mut kernel, &effects, root.job, &[correction.id]);
    let card = context::card(&kernel, root.job);
    assert_eq!(card.inputs, vec![first.id, correction.id]);
    let (_, effects) = kernel
        .accept(NOW, Input::new(InputId(9912), "adjust parser again"))
        .unwrap();
    let (_, Call::Converse(directory)) = starts(&effects).next().unwrap() else {
        panic!("conversation")
    };
    assert_eq!(directory.jobs[0].inputs, card.inputs);
    assert!(serde_json::to_vec(directory).unwrap().len() <= kernel.limits.context_bytes);
    let mut call = converse_call(&effects);
    for expected_input in [first.id, correction.id] {
        let source = kernel.inputs[&expected_input].accepted_at;
        let effects = kernel.step(
            NOW,
            Event::ConverseFinished {
                call,
                result: Ok(ConversationStep::Read(ReadQuery::Record {
                    id: source,
                    offset: 0,
                })),
            },
        );
        let (_, Call::Converse(read)) = starts(&effects).next().unwrap() else {
            panic!("read")
        };
        assert!(read.records.iter().any(|record| record.source == source));
        call = converse_call(&effects);
    }
}

#[test]
fn job_message_read_permission_does_not_expose_child_records() {
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
    let source = kernel
        .records
        .values()
        .find_map(|record| {
            matches!(record.body, RecordBody::JobCreated { job, .. } if job == child)
                .then_some(record.seq)
        })
        .unwrap();
    let mut record = kernel.records[&source].as_ref().clone();
    record.body = RecordBody::JobMessage {
        job: child,
        inputs: vec![],
        text: "private".into(),
    };
    kernel.records.insert(source, std::sync::Arc::new(record));
    let next = Input::new(InputId(9916), "unrelated work");
    let (_, effects) = kernel.accept(NOW, next.clone()).unwrap();
    kernel.step(
        NOW,
        Event::ConverseFinished {
            call: converse_call(&effects),
            result: Ok(ConversationStep::Read(ReadQuery::Record {
                id: source,
                offset: 0,
            })),
        },
    );
    assert!(
        kernel
            .records
            .values()
            .any(|record| matches!(record.body, RecordBody::ConversationRejected { .. }))
    );
}

#[test]
fn user_questions_survive_session_projection_and_can_be_read_by_later_conversation() {
    for worker_question in [false, true] {
        let mut kernel = kernel();
        let original = Input::new(InputId(9920), "choose a destination");
        let (_, effects) = kernel.accept(NOW, original.clone()).unwrap();
        let question = "First: Paris. Second: London. Which destination?";
        if worker_question {
            let effects = kernel.step(
                NOW,
                Event::ConverseFinished {
                    call: converse_call(&effects),
                    result: Ok(start_jobs("choose destination", &[original.id])),
                },
            );
            work(
                &mut kernel,
                work_calls(&effects)[0].0,
                WorkStep::NeedInput(question.into()),
            );
            converse_step(
                &mut kernel,
                ConversationStep::Ask {
                    inputs: vec![original.id],
                    question: question.into(),
                },
            );
        } else {
            kernel.step(
                NOW,
                Event::ConverseFinished {
                    call: converse_call(&effects),
                    result: Ok(ConversationStep::Ask {
                        inputs: vec![original.id],
                        question: question.into(),
                    }),
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
        let effects = if starts(&effects).any(|(_, call)| matches!(call, Call::Converse(_))) {
            effects
        } else {
            let call = converse_call(&reply_effects);
            kernel.step(
                NOW,
                Event::ConverseFinished {
                    call,
                    result: Ok(ConversationStep::Ask {
                        inputs: vec![original.id],
                        question: "stale".into(),
                    }),
                },
            )
        };
        let (_, Call::Converse(conversation)) = starts(&effects).next().unwrap() else {
            panic!("conversation")
        };
        let inputs = conversation
            .inputs
            .iter()
            .map(|input| input.id)
            .collect::<Vec<_>>();
        kernel.step(
            NOW,
            Event::ConverseFinished {
                call: converse_call(&effects),
                result: Ok(start_jobs("use the choice", &inputs)),
            },
        );
        let (_, effects) = kernel
            .accept(NOW, Input::new(InputId(9923), "remember the destination"))
            .unwrap();
        let (_, Call::Converse(conversation)) = starts(&effects).next().unwrap() else {
            panic!("new router")
        };
        assert!(
            serde_json::to_string(&conversation)
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
            Event::ConverseFinished {
                call: converse_call(&effects),
                result: Ok(ConversationStep::Read(ReadQuery::Record {
                    id: question_seq,
                    offset: 0,
                })),
            },
        );
        let (_, Call::Converse(read)) = starts(&effects).next().unwrap() else {
            panic!("conversation read")
        };
        assert!(
            read.records
                .iter()
                .any(|record| record.source == question_seq && record.content.contains(question))
        );
    }
}

#[test]
fn conversation_inputs_follow_acceptance_order_not_numeric_ids() {
    let mut kernel = kernel();
    kernel
        .accept(NOW, Input::new(InputId(99), "first request"))
        .unwrap();
    kernel
        .accept(NOW, Input::new(InputId(1), "new correction"))
        .unwrap();
    let input = context::prepare_conversation(&kernel).unwrap();
    assert_eq!(
        input
            .inputs
            .iter()
            .map(|input| input.id)
            .collect::<Vec<_>>(),
        vec![InputId(99), InputId(1)]
    );
}

#[test]
fn public_session_checkpoint_does_not_cover_unconsumed_conversation_feedback() {
    let mut kernel = kernel();
    let (_, effects) = kernel
        .accept(NOW, Input::new(InputId(1), "do work"))
        .unwrap();
    kernel.step(
        NOW,
        Event::ConverseFinished {
            call: converse_call(&effects),
            result: Ok(ConversationStep::Wait),
        },
    );
    let feedback = kernel
        .records
        .values()
        .find_map(|record| {
            matches!(record.body, RecordBody::ConversationRejected { .. }).then_some(record.seq)
        })
        .unwrap();
    let (receipt, _) = kernel
        .accept(NOW, Input::new(InputId(2), "more context"))
        .unwrap();
    let seq = Seq(kernel.records.keys().next_back().unwrap().0 + 1);
    kernel.records.insert(
        seq,
        std::sync::Arc::new(crate::Record {
            seq,
            origin: crate::Origin::Kernel,
            body: RecordBody::SessionCheckpoint {
                checkpoint: std::sync::Arc::new(crate::SessionCheckpoint {
                    through: receipt.accepted_at,
                    summary: "public messages only".into(),
                    evidence: Vec::new(),
                }),
            },
        }),
    );
    let input = context::prepare_conversation(&kernel).unwrap();
    assert!(
        input.records.iter().any(|record| record.source == feedback),
        "public compaction must not acknowledge private conversation feedback"
    );
}
