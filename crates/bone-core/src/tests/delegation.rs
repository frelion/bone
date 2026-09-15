use super::*;
use crate::{JobId, WaitView};

fn parent(kernel: &mut Kernel) -> (CallId, WorkInput) {
    create_roots(
        kernel,
        Input::new(InputId(1), "implement two independent functions"),
        vec![assignment("parent", &[InputId(1)])],
    )
    .pop()
    .unwrap()
}

fn outcomes(kernel: &Kernel, target: JobId) -> usize {
    kernel.records.values().filter(|record| matches!(record.body,
        RecordBody::Delivery { to: DeliveryTarget::Job(job), kind: crate::DeliveryKind::Outcome, .. } if job == target
    )).count()
}

#[test]
fn atomic_batch_wait_uses_no_parent_round_until_both_children_succeed() {
    let mut kernel = kernel();
    let (call, parent) = parent(&mut kernel);
    let effects = work(
        &mut kernel,
        call,
        WorkStep::delegate_and_wait(vec![
            assignment("normalize", &[]),
            assignment("dedupe", &[]),
        ]),
    );
    let children = work_calls(&effects);
    assert_eq!(children.len(), 2);
    assert!(children.iter().all(|(_, child)| child.job != parent.job));
    assert!(
        matches!(kernel.job_status(parent.job), JobStatus::Waiting(WaitView::Jobs(ref ids)) if ids.len() == 2)
    );
    // Creation and waiting are already present together in the same snapshot.
    let snapshot = kernel.durable_snapshot().unwrap();
    assert!(
        snapshot.payload["jobs"][parent.job.to_string()]["state"]["Waiting"]
            .get("Jobs")
            .is_some()
    );
    let effects = work(
        &mut kernel,
        children[0].0,
        WorkStep::Finish(Completion::new("normalize verified")),
    );
    assert!(work_calls(&effects).is_empty());
    assert_eq!(outcomes(&kernel, parent.job), 1);
    assert!(
        kernel.jobs[&parent.job].context.records.back().unwrap()
            > &kernel.jobs[&parent.job].context.read_through
    );
    for seconds in [1, 5, 60] {
        let effects = kernel.step(MonoTime(Duration::from_secs(seconds)), Event::Tick);
        assert!(work_calls(&effects).is_empty());
    }
    let effects = work(
        &mut kernel,
        children[1].0,
        WorkStep::Finish(Completion::new("dedupe verified")),
    );
    let calls = work_calls(&effects);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1.job, parent.job);
    assert_eq!(outcomes(&kernel, parent.job), 2);
    assert_eq!(
        calls[0]
            .1
            .records
            .iter()
            .filter(|view| matches!(
                kernel.records[&view.source].body,
                RecordBody::Delivery {
                    kind: crate::DeliveryKind::Outcome,
                    ..
                }
            ))
            .count(),
        2
    );
    work(
        &mut kernel,
        calls[0].0,
        WorkStep::Finish(Completion::new("integration verified")),
    );
    assert!(finished_as(&kernel, parent.job, OutcomeKind::Completed));
}

#[test]
fn parent_with_independent_work_is_not_forced_to_wait() {
    let mut kernel = kernel();
    let (call, parent) = parent(&mut kernel);
    let effects = work(
        &mut kernel,
        call,
        WorkStep::delegate(vec![assignment("child", &[])]),
    );
    assert!(
        work_calls(&effects)
            .iter()
            .any(|(_, input)| input.job == parent.job)
    );
}

#[test]
fn failed_or_cancelled_child_interrupts_group_without_finishing_other_children() {
    for cancel in [false, true] {
        let mut kernel = kernel();
        let (call, parent) = parent(&mut kernel);
        let effects = work(
            &mut kernel,
            call,
            WorkStep::delegate_and_wait(vec![assignment("a", &[]), assignment("b", &[])]),
        );
        let calls = work_calls(&effects);
        let effects = if cancel {
            kernel.control(NOW, KernelControl::Cancel(calls[0].1.job)).1
        } else {
            work(
                &mut kernel,
                calls[0].0,
                WorkStep::Fail(Completion::new("unable to verify")),
            )
        };
        let resumed = work_calls(&effects);
        assert_eq!(resumed.len(), 1);
        assert_eq!(resumed[0].1.job, parent.job);
        assert_eq!(outcomes(&kernel, parent.job), 1);
        assert!(matches!(
            kernel.job_status(calls[1].1.job),
            JobStatus::Running
        ));
        work(
            &mut kernel,
            resumed[0].0,
            WorkStep::Finish(Completion::new("premature")),
        );
        assert!(!finished_as(&kernel, parent.job, OutcomeKind::Completed));
    }
}

#[test]
fn group_wait_accepts_already_finished_members_and_deduplicates_outcomes() {
    let mut kernel = kernel();
    let (call, parent) = parent(&mut kernel);
    let effects = work(
        &mut kernel,
        call,
        WorkStep::delegate(vec![assignment("a", &[]), assignment("b", &[])]),
    );
    let calls = work_calls(&effects);
    let (parent_call, _) = calls
        .iter()
        .find(|(_, input)| input.job == parent.job)
        .unwrap();
    let children = calls
        .iter()
        .filter(|(_, input)| input.job != parent.job)
        .collect::<Vec<_>>();
    work(
        &mut kernel,
        children[0].0,
        WorkStep::Finish(Completion::new("done")),
    );
    let effects = work(
        &mut kernel,
        *parent_call,
        WorkStep::Wait(Await::Jobs(
            children.iter().map(|(_, input)| input.job).collect(),
        )),
    );
    assert!(work_calls(&effects).is_empty());
    let effects = work(
        &mut kernel,
        children[1].0,
        WorkStep::Finish(Completion::new("done")),
    );
    assert_eq!(work_calls(&effects).len(), 1);
    assert_eq!(outcomes(&kernel, parent.job), 2);
}

#[test]
fn new_user_input_interrupts_a_group_wait() {
    let mut kernel = kernel();
    let (call, parent) = parent(&mut kernel);
    work(
        &mut kernel,
        call,
        WorkStep::delegate_and_wait(vec![assignment("a", &[]), assignment("b", &[])]),
    );
    let update = Input::new(InputId(2), "stop and review this changed requirement");
    let (_, effects) = kernel.accept(NOW, update.clone()).unwrap();
    let effects = route_existing(&mut kernel, &effects, parent.job, &[update.id]);
    let calls = work_calls(&effects);
    assert!(calls.iter().any(|(_, input)| input.job == parent.job));
    assert_eq!(
        kernel.inputs[&update.id].pending_review_by,
        Some(parent.job)
    );
}

#[test]
fn invalid_group_is_rejected_without_changing_ownership() {
    for jobs in [
        vec![],
        vec![JobId(1)],
        vec![JobId(999)],
        vec![JobId(999), JobId(999)],
    ] {
        let mut kernel = kernel();
        let (call, parent) = parent(&mut kernel);
        let effects = work(&mut kernel, call, WorkStep::Wait(Await::Jobs(jobs)));
        assert_eq!(kernel.jobs.len(), 1);
        assert_eq!(kernel.jobs[&parent.job].work_rejections.total, 1);
        assert_eq!(work_calls(&effects).len(), 1);
    }
}

#[test]
fn leaf_job_cannot_create_grandchildren_even_with_a_valid_nonempty_proposal() {
    let mut kernel = kernel();
    let (call, parent) = parent(&mut kernel);
    let effects = work(
        &mut kernel,
        call,
        WorkStep::delegate_and_wait(vec![assignment("leaf", &[])]),
    );
    let (call, child) = work_calls(&effects).pop().unwrap();
    assert_eq!(child.delegation, crate::DelegationLimits::default());
    let effects = work(
        &mut kernel,
        call,
        WorkStep::delegate_and_wait(vec![assignment("forbidden", &[])]),
    );
    assert_eq!(kernel.jobs.len(), 2);
    assert_eq!(kernel.jobs[&child.job].work_rejections.total, 1);
    assert!(
        work_calls(&effects)
            .iter()
            .all(|(_, input)| input.job == child.job)
    );
    assert!(matches!(
        kernel.job_status(parent.job),
        JobStatus::Waiting(WaitView::Jobs(_))
    ));
}

#[test]
fn siblings_share_ancestor_budget_and_finished_descendants_do_not_refill_it() {
    let mut kernel = Kernel::new(
        AgentLimits {
            job_budget: 4,
            job_depth: 3,
            ..AgentLimits::default()
        },
        vec![],
    )
    .unwrap();
    let (root_call, root) = parent(&mut kernel);
    let branch = |name: &str| {
        let mut assignment = assignment(name, &[]);
        assignment.delegation = crate::DelegationLimits {
            max_descendants: 2,
            max_depth: 1,
        };
        assignment
    };
    let effects = work(
        &mut kernel,
        root_call,
        WorkStep::delegate(vec![branch("a"), branch("b")]),
    );
    let calls = calls_by_goal(work_calls(&effects));
    let root_call = calls["parent"].0;
    let a = &calls["a"];
    let b = &calls["b"];
    assert_eq!(b.1.delegation.max_descendants, 2);
    let effects = work(
        &mut kernel,
        a.0,
        WorkStep::delegate_and_wait(vec![assignment("a-leaf", &[])]),
    );
    let a_leaf = work_calls(&effects).pop().unwrap();
    let effects = work(
        &mut kernel,
        b.0,
        WorkStep::delegate_and_wait(vec![assignment("b-one", &[]), assignment("b-two", &[])]),
    );
    assert_eq!(
        kernel.jobs.len(),
        4,
        "stale capacity cannot partially create a batch"
    );
    let retry = work_calls(&effects).pop().unwrap();
    assert_eq!(retry.1.delegation.max_descendants, 1);
    let effects = work(
        &mut kernel,
        retry.0,
        WorkStep::delegate_and_wait(vec![assignment("b-leaf", &[])]),
    );
    let b_leaf = work_calls(&effects).pop().unwrap();
    assert_eq!(kernel.jobs.len(), 5);
    work(
        &mut kernel,
        a_leaf.0,
        WorkStep::Finish(Completion::new("done")),
    );
    work(
        &mut kernel,
        b_leaf.0,
        WorkStep::Finish(Completion::new("done")),
    );
    assert_eq!(
        kernel.delegation_capacity(root.job),
        crate::DelegationLimits::default()
    );
    work(
        &mut kernel,
        root_call,
        WorkStep::delegate(vec![assignment("over budget", &[])]),
    );
    assert_eq!(kernel.jobs.len(), 5);
    assert_eq!(kernel.jobs[&root.job].work_rejections.total, 1);
}

#[test]
fn children_cannot_escalate_depth_or_budget_and_invalid_batch_is_atomic() {
    for requested in [
        crate::DelegationLimits {
            max_descendants: 10,
            max_depth: 1,
        },
        crate::DelegationLimits {
            max_descendants: 1,
            max_depth: 2,
        },
    ] {
        let mut kernel = Kernel::new(
            AgentLimits {
                job_budget: 4,
                job_depth: 3,
                ..AgentLimits::default()
            },
            vec![],
        )
        .unwrap();
        let (call, root) = parent(&mut kernel);
        let mut invalid = assignment("invalid", &[]);
        invalid.delegation = requested;
        work(
            &mut kernel,
            call,
            WorkStep::delegate_and_wait(vec![assignment("valid", &[]), invalid]),
        );
        assert_eq!(kernel.jobs.len(), 1);
        assert_eq!(kernel.jobs[&root.job].work_rejections.total, 1);
        assert_eq!(kernel.delegation_capacity(root.job).max_descendants, 4);
    }
}

#[test]
fn host_can_disable_delegation_and_tighten_existing_job_limits() {
    let mut kernel = kernel();
    let (call, root) = parent(&mut kernel);
    kernel.limits.job_budget = 0;
    work(
        &mut kernel,
        call,
        WorkStep::delegate(vec![assignment("forbidden", &[])]),
    );
    assert_eq!(kernel.jobs.len(), 1);
    assert_eq!(kernel.jobs[&root.job].work_rejections.total, 1);
    assert_eq!(
        kernel.delegation_capacity(root.job),
        crate::DelegationLimits::default()
    );
}

#[test]
fn group_waiting_job_can_answer_an_inquiry_and_keep_waiting() {
    let mut kernel = kernel();
    let (call, root) = parent(&mut kernel);
    let mut branch = assignment("branch", &[]);
    branch.delegation = crate::DelegationLimits {
        max_descendants: 2,
        max_depth: 1,
    };
    let effects = work(&mut kernel, call, WorkStep::delegate(vec![branch]));
    let calls = calls_by_goal(work_calls(&effects));
    let branch = &calls["branch"];
    work(
        &mut kernel,
        branch.0,
        WorkStep::delegate_and_wait(vec![assignment("leaf", &[])]),
    );
    let effects = work(
        &mut kernel,
        calls["parent"].0,
        WorkStep::Inquire {
            job: branch.1.job,
            question: "status?".into(),
        },
    );
    let call = work_calls(&effects).pop().unwrap();
    assert_eq!(call.1.job, branch.1.job);
    let effects = kernel.step(
        NOW,
        Event::WorkFinished {
            call: call.0,
            result: Ok(WorkProposal {
                note: None,
                report: None,
                answers: vec![InquiryAnswer {
                    inquiry: call.1.inquiries[0].id,
                    response: InquiryResponse::Answer(ReportDraft {
                        summary: "leaf still working".into(),
                        evidence: vec![],
                    }),
                }],
                step: WorkStep::Continue,
            }),
        },
    );
    assert!(matches!(
        kernel.job_status(branch.1.job),
        JobStatus::Waiting(WaitView::Jobs(_))
    ));
    assert!(
        work_calls(&effects)
            .iter()
            .all(|(_, input)| input.job == root.job)
    );
}

#[test]
fn snapshot_preserves_subtree_limits_and_missing_current_limits_are_invalid() {
    let mut kernel = kernel();
    let (call, root) = parent(&mut kernel);
    work(
        &mut kernel,
        call,
        WorkStep::delegate_and_wait(vec![assignment("leaf", &[])]),
    );
    let snapshot = kernel.durable_snapshot().unwrap();
    let (restored, _) = Kernel::restore(
        snapshot.clone(),
        kernel.records.values().cloned().collect(),
        AgentLimits::default(),
        vec![],
    )
    .unwrap();
    assert_eq!(
        restored.jobs[&root.job].delegation,
        kernel.jobs[&root.job].delegation
    );
    assert_eq!(
        restored.delegation_capacity(root.job),
        kernel.delegation_capacity(root.job)
    );
    let mut invalid = snapshot;
    invalid.payload["jobs"][root.job.to_string()]
        .as_object_mut()
        .unwrap()
        .remove("delegation");
    assert!(
        Kernel::restore(
            invalid,
            kernel.records.values().cloned().collect(),
            AgentLimits::default(),
            vec![]
        )
        .is_err()
    );
}

#[test]
fn legacy_pending_delegation_migrates_without_replaying_and_v2_counters_are_required() {
    let mut kernel = kernel();
    let (call, root) = parent(&mut kernel);
    for version in [1, 2] {
        let mut snapshot = kernel.durable_snapshot().unwrap();
        snapshot.version = version;
        let job = &mut snapshot.payload["jobs"][root.job.to_string()];
        job.as_object_mut().unwrap().remove("delegation");
        if version == 1 {
            job.as_object_mut().unwrap().remove("work_rejections");
        }
        let mut legacy = serde_json::to_value(assignment("old child", &[])).unwrap();
        legacy.as_object_mut().unwrap().remove("delegation");
        job["state"] = json!({"Waiting":{"Commit":{"call":call,"step":{"Delegate":[legacy]}}}});
        let (restored, effects) = Kernel::restore(
            snapshot,
            kernel.records.values().cloned().collect(),
            AgentLimits::default(),
            vec![],
        )
        .unwrap();
        assert_eq!(restored.jobs.len(), 1);
        assert!(finished_as(&restored, root.job, OutcomeKind::Failed));
        assert!(starts(&effects).next().is_none());
    }
    let mut snapshot = kernel.durable_snapshot().unwrap();
    snapshot.version = 2;
    snapshot.payload["jobs"][root.job.to_string()]
        .as_object_mut()
        .unwrap()
        .remove("work_rejections");
    assert!(
        Kernel::restore(
            snapshot,
            kernel.records.values().cloned().collect(),
            AgentLimits::default(),
            vec![]
        )
        .is_err()
    );
}
