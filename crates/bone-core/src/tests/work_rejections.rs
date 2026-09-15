use super::*;
use crate::WorkRejections;

fn root(kernel: &mut Kernel) -> (CallId, WorkInput) {
    create_roots(
        kernel,
        Input::new(InputId(1), "implement and verify"),
        vec![assignment("root", &[InputId(1)])],
    )
    .pop()
    .unwrap()
}

#[test]
fn empty_delegation_returns_feedback_and_can_finish_in_the_same_job() {
    let mut kernel = kernel();
    let (call, initial) = root(&mut kernel);
    let effects = work(&mut kernel, call, WorkStep::delegate(vec![]));
    let (next, retry) = work_calls(&effects).pop().expect("correction call");
    assert_eq!(retry.job, initial.job);
    assert_eq!(kernel.jobs.len(), 1);
    assert_eq!(kernel.jobs[&initial.job].context.read_through, Seq::ZERO);
    assert_eq!(
        kernel.inputs[&InputId(1)].pending_review_by,
        Some(initial.job)
    );
    assert!(retry.records.iter().any(|record| matches!(
        serde_json::from_str::<RecordBody>(&record.content),
        Ok(RecordBody::WorkRejected { job, call: rejected, message, budget })
        if job == initial.job && rejected == call && message == "delegate requires at least one assignment"
            && budget == (WorkRejections { consecutive: 1, total: 1 })
    )));
    // Redelivery of the same model completion must not spend another attempt.
    assert!(work(&mut kernel, call, WorkStep::delegate(vec![])).is_empty());
    assert_eq!(kernel.jobs[&initial.job].work_rejections.total, 1);
    work(
        &mut kernel,
        next,
        WorkStep::Finish(Completion::new("verified")),
    );
    assert!(finished_as(&kernel, initial.job, OutcomeKind::Completed));
    assert_eq!(
        kernel.jobs[&initial.job].work_rejections,
        WorkRejections {
            consecutive: 0,
            total: 1
        }
    );
    assert!(kernel.inputs[&InputId(1)].pending_review_by.is_none());
}

#[test]
fn three_consecutive_rejections_terminate_without_a_fourth_call() {
    let mut kernel = kernel();
    let (mut call, initial) = root(&mut kernel);
    for attempt in 1..=WorkRejections::CONSECUTIVE_LIMIT {
        let effects = work(
            &mut kernel,
            call,
            WorkStep::Read(ReadQuery::Job(crate::JobId(999))),
        );
        assert!(
            !kernel
                .records
                .values()
                .any(|record| matches!(record.body, RecordBody::ReadResult { .. }))
        );
        assert_eq!(kernel.jobs[&initial.job].work_rejections.total, attempt);
        if attempt < WorkRejections::CONSECUTIVE_LIMIT {
            call = work_calls(&effects).pop().unwrap().0;
        } else {
            assert!(work_calls(&effects).is_empty());
            assert!(finished_as(&kernel, initial.job, OutcomeKind::Failed));
            let JobStatus::Finished(outcome) = kernel.job_status(initial.job) else {
                unreachable!()
            };
            assert!(
                outcome
                    .completion
                    .summary
                    .contains("correction budget exhausted")
            );
        }
    }
}

#[test]
fn valid_interleaved_actions_do_not_reset_the_total_budget() {
    let mut kernel = kernel();
    let (mut call, initial) = root(&mut kernel);
    for attempt in 1..=WorkRejections::TOTAL_LIMIT {
        let effects = work(&mut kernel, call, WorkStep::delegate(vec![]));
        assert_eq!(
            kernel.jobs[&initial.job].work_rejections,
            WorkRejections {
                consecutive: 1,
                total: attempt
            }
        );
        if attempt == WorkRejections::TOTAL_LIMIT {
            assert!(work_calls(&effects).is_empty());
            assert!(finished_as(&kernel, initial.job, OutcomeKind::Failed));
        } else {
            call = work_calls(&effects).pop().unwrap().0;
            let effects = work(
                &mut kernel,
                call,
                WorkStep::Read(ReadQuery::Job(initial.job)),
            );
            call = work_calls(&effects).pop().unwrap().0;
        }
    }
}

#[test]
fn rejected_action_does_not_publish_notes_reports_or_settle_valid_inquiry_answers() {
    let mut kernel = kernel();
    let (call, parent) = root(&mut kernel);
    let effects = work(
        &mut kernel,
        call,
        WorkStep::delegate(vec![assignment("child", &[])]),
    );
    let calls = work_calls(&effects);
    let parent_call = calls
        .iter()
        .find(|(_, input)| input.job == parent.job)
        .unwrap()
        .0;
    let (child_call, child) = calls
        .iter()
        .find(|(_, input)| input.job != parent.job)
        .unwrap();
    work(
        &mut kernel,
        parent_call,
        WorkStep::Inquire {
            job: child.job,
            question: "what did you verify?".into(),
        },
    );
    let effects = work(&mut kernel, *child_call, WorkStep::Continue);
    let (child_call, child) = work_calls(&effects).pop().unwrap();
    let inquiry = child.inquiries[0].id;
    let answer = InquiryAnswer {
        inquiry,
        response: InquiryResponse::Answer(ReportDraft {
            summary: "verified".into(),
            evidence: vec![],
        }),
    };
    let read_through = kernel.jobs[&child.job].context.read_through;
    let effects = kernel.step(
        NOW,
        Event::WorkFinished {
            call: child_call,
            result: Ok(WorkProposal {
                note: Some("uncommitted note".into()),
                report: Some(ReportDraft {
                    summary: "uncommitted report".into(),
                    evidence: vec![],
                }),
                answers: vec![answer.clone()],
                step: WorkStep::delegate(vec![]),
            }),
        },
    );
    assert!(kernel.inquiries.contains_key(&inquiry));
    assert_eq!(kernel.jobs[&child.job].context.read_through, read_through);
    assert!(kernel.jobs[&child.job].report.is_none());
    assert!(!kernel.records.values().any(|record| matches!(
        record.body,
        RecordBody::Note { .. } | RecordBody::Report { .. } | RecordBody::InquirySettled { .. }
    )));
    let (call, retry) = work_calls(&effects).pop().unwrap();
    assert_eq!(retry.job, child.job);
    assert_eq!(retry.inquiries[0].id, inquiry);
    kernel.step(
        NOW,
        Event::WorkFinished {
            call,
            result: Ok(WorkProposal {
                note: None,
                report: None,
                answers: vec![answer],
                step: WorkStep::Continue,
            }),
        },
    );
    assert!(!kernel.inquiries.contains_key(&inquiry));
}

#[test]
fn late_cancelled_proposals_neither_retry_nor_consume_budget() {
    let mut kernel = kernel();
    let (call, initial) = root(&mut kernel);
    kernel.control(NOW, KernelControl::Cancel(initial.job));
    let effects = work(&mut kernel, call, WorkStep::delegate(vec![]));
    assert!(work_calls(&effects).is_empty());
    assert_eq!(kernel.jobs[&initial.job].work_rejections.total, 0);
    assert!(
        !kernel
            .records
            .values()
            .any(|record| matches!(record.body, RecordBody::WorkRejected { .. }))
    );
    assert!(finished_as(&kernel, initial.job, OutcomeKind::Cancelled));
}

#[test]
fn correction_budget_survives_snapshot_round_trip_and_existing_restore_policy() {
    let mut kernel = kernel();
    let (call, initial) = root(&mut kernel);
    work(&mut kernel, call, WorkStep::delegate(vec![]));
    let snapshot = kernel.durable_snapshot().unwrap();
    assert_eq!(snapshot.version(), 3);
    let snapshot = serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
    let (restored, effects) = Kernel::restore(
        snapshot,
        kernel.records.values().cloned().collect(),
        AgentLimits::default(),
        vec![],
    )
    .unwrap();
    assert_eq!(
        restored.jobs[&initial.job].work_rejections,
        WorkRejections {
            consecutive: 1,
            total: 1
        }
    );
    // Recovery still interrupts work; this change does not introduce automatic replay.
    assert!(finished_as(&restored, initial.job, OutcomeKind::Failed));
    assert!(starts(&effects).next().is_none());
}

#[test]
fn only_legacy_snapshots_may_omit_correction_counters() {
    let mut kernel = kernel();
    let (_, initial) = root(&mut kernel);
    let mut snapshot = kernel.durable_snapshot().unwrap();
    snapshot.payload["jobs"][initial.job.to_string()]
        .as_object_mut()
        .unwrap()
        .remove("work_rejections");
    assert!(
        Kernel::restore(
            snapshot.clone(),
            kernel.records.values().cloned().collect(),
            AgentLimits::default(),
            vec![]
        )
        .is_err()
    );
    snapshot.version = 1;
    let (restored, _) = Kernel::restore(
        snapshot,
        kernel.records.values().cloned().collect(),
        AgentLimits::default(),
        vec![],
    )
    .unwrap();
    assert_eq!(
        restored.jobs[&initial.job].work_rejections,
        WorkRejections::default()
    );
}
