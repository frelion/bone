use super::*;

fn assert_visible_start_evidence(input: &crate::ConversationInput, sources: &[Seq]) {
    let contract = crate::model_contract::converse(input.clone()).unwrap();
    let start = contract.submission().2["properties"]["step"]["anyOf"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|variant| {
            variant.pointer("/properties/Start/items/properties/evidence/items/enum")
        })
        .expect("visible evidence appears in the Start schema");
    for source in sources {
        assert!(start.as_array().unwrap().contains(&json!(source.0)));
    }
}

#[test]
fn input_and_reply_read_from_history_are_legal_assignment_evidence() {
    for use_reply in [false, true] {
        let mut kernel = kernel();
        let (receipt, _) = kernel.accept(NOW, Input::new(InputId(1), "hello")).unwrap();
        converse_step(
            &mut kernel,
            ConversationStep::Reply {
                inputs: vec![InputId(1)],
                text: "Hello.".into(),
                outcome: InputOutcome::Completed,
            },
        );
        let source = if use_reply {
            kernel
                .records
                .values()
                .find_map(|record| {
                    matches!(record.body, RecordBody::Reply { .. }).then_some(record.seq)
                })
                .unwrap()
        } else {
            receipt.accepted_at
        };
        let (_, effects) = kernel
            .accept(NOW, Input::new(InputId(2), "analyze the previous exchange"))
            .unwrap();
        let effects = kernel.step(
            NOW,
            Event::ConverseFinished {
                call: converse_call(&effects),
                result: Ok(ConversationStep::Read(ReadQuery::Record {
                    id: source,
                    offset: 0,
                })),
            },
        );
        let (call, input) = starts(&effects)
            .find_map(|(id, call)| match call {
                Call::Converse(input) => Some((id, input)),
                _ => None,
            })
            .unwrap();
        let read_result = input
            .records
            .iter()
            .find_map(|record| {
                matches!(
                    kernel.records[&record.source].body,
                    RecordBody::ReadResult { .. }
                )
                .then_some(record.source)
            })
            .unwrap();
        let evidence = vec![source, read_result];
        assert_visible_start_evidence(input, &evidence);
        let mut job = assignment("analyze exchange", &[InputId(2)]);
        job.evidence = evidence.clone();
        let step = ConversationStep::Start(vec![job]);
        let decoded = crate::model_contract::converse(input.clone())
            .unwrap()
            .decode(&json!({"step":step}))
            .unwrap();
        let effects = kernel.step(
            NOW,
            Event::ConverseFinished {
                call,
                result: Ok(decoded),
            },
        );
        let (_, worker) = work_calls(&effects)
            .pop()
            .expect("schema-valid readable evidence must start work");
        for source in evidence {
            assert!(worker.records.iter().any(|record| record.source == source));
        }
    }
}

#[test]
fn visible_private_conversation_feedback_is_legal_assignment_evidence() {
    let mut kernel = kernel();
    let (_, effects) = kernel
        .accept(NOW, Input::new(InputId(1), "inspect the code"))
        .unwrap();
    let effects = kernel.step(
        NOW,
        Event::ConverseFinished {
            call: converse_call(&effects),
            result: Ok(ConversationStep::Wait),
        },
    );
    let (call, input) = starts(&effects)
        .find_map(|(id, call)| match call {
            Call::Converse(input) => Some((id, input)),
            _ => None,
        })
        .unwrap();
    let feedback = input
        .records
        .iter()
        .find_map(|record| {
            matches!(
                kernel.records[&record.source].body,
                RecordBody::ConversationRejected { .. }
            )
            .then_some(record.source)
        })
        .unwrap();
    assert_visible_start_evidence(input, &[feedback]);
    let mut job = assignment("inspect code", &[InputId(1)]);
    job.evidence = vec![feedback];
    let step = ConversationStep::Start(vec![job]);
    let decoded = crate::model_contract::converse(input.clone())
        .unwrap()
        .decode(&json!({"step":step}))
        .unwrap();
    let effects = kernel.step(
        NOW,
        Event::ConverseFinished {
            call,
            result: Ok(decoded),
        },
    );
    assert_eq!(work_calls(&effects).len(), 1);
    assert_eq!(kernel.jobs.len(), 1);
}

#[test]
fn unshared_worker_notes_are_rejected_for_both_read_and_assignment() {
    let mut kernel = kernel();
    let (call, _) = create_roots(
        &mut kernel,
        Input::new(InputId(1), "inspect"),
        vec![assignment("inspect", &[InputId(1)])],
    )
    .pop()
    .unwrap();
    kernel.step(
        NOW,
        Event::WorkFinished {
            call,
            result: Ok(WorkProposal {
                note: Some("unshared implementation note".into()),
                report: None,
                answers: vec![],
                step: WorkStep::Finish(Completion::new("inspection done")),
            }),
        },
    );
    let private = kernel
        .records
        .values()
        .find_map(|record| matches!(record.body, RecordBody::Note { .. }).then_some(record.seq))
        .unwrap();
    converse_step(
        &mut kernel,
        ConversationStep::Read(ReadQuery::Record {
            id: private,
            offset: 0,
        }),
    );
    let mut job = assignment("follow up", &[InputId(1)]);
    job.evidence = vec![private];
    converse_step(&mut kernel, ConversationStep::Start(vec![job]));
    assert_eq!(kernel.jobs.len(), 1);
    let errors = kernel
        .records
        .values()
        .filter_map(|record| match &record.body {
            RecordBody::ConversationRejected { message, .. } => Some(message.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        errors
            .iter()
            .any(|message| message.contains("cannot read that record"))
    );
    assert!(errors.iter().any(|message| {
        message.contains(&format!("inaccessible evidence {private}"))
            && message.contains("empty evidence")
    }));
}
