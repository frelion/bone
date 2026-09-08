//! Deterministic protocol specifications. No model, clock, or network is needed.
mod support;
use bone_agent::*;
use serde_json::json;
use std::time::Duration;
use support::*;

#[test]
fn routing_precedes_the_first_worker_without_a_temporary_input_job() {
    let mut s = Scenario::new();
    let (route, request) = routing(&s.say(1, "original words"));
    assert!(s.kernel.snapshot().jobs.is_empty());
    assert!(
        matches!(&request.task, ModelTask::Kernel { inputs, .. } if inputs == &vec![input(1, "original words")])
    );
    let effects = s.finish(
        route,
        decision(vec![create("short goal", vec![InputId(1)])]),
    );
    let job = s.kernel.snapshot().jobs[0].id;
    let (_, request) = worker(&effects, job);
    assert!(
        matches!(request.task, ModelTask::Work { messages, .. } if messages == vec![input(1, "original words")])
    );
    assert!(matches!(s.state(1), InputState::Handled));
}

#[test]
fn background_a_continues_while_kernel_routes_b_to_a_full_worker() {
    let mut s = Scenario::new();
    let (a, first) = s.create(1, "A");
    let (held, _) = worker(
        &s.work(
            first,
            WorkProposal {
                next: Next::Continue,
                ..Default::default()
            },
        ),
        a,
    );
    let (route, _) = routing(&s.say(2, "B"));
    assert_eq!(s.job(a).active_call, Some(held));
    assert_eq!(s.call(held).state, CallState::Running);
    let effects = s.finish(route, decision(vec![create("B", vec![InputId(2)])]));
    let b = s.kernel.snapshot().jobs[1].id;
    let (call, _) = worker(&effects, b);
    let effects = s.work(call, operation("lookup"));
    tool(&effects, "lookup");
    assert_eq!(s.job(a).active_call, Some(held));
    no_route(&effects);
}

#[test]
fn ten_tool_rounds_keep_one_semantic_job_and_need_no_further_routing() {
    let mut s = Scenario::new();
    let (job, mut call) = s.create(1, "Read ten times");
    for round in 0..10 {
        let effects = s.work(call, operation("lookup"));
        let read = tool(&effects, "lookup");
        no_route(&effects);
        let effects = s.finish(read, CallOutcome::artifact(json!({"round":round})));
        no_route(&effects);
        call = worker(&effects, job).0;
    }
    let effects = s.work(call, answer("done"));
    assert_eq!(replies(&effects), ["done"]);
    assert_eq!(s.job(job).state, JobState::Completed);
    assert_eq!(s.kernel.snapshot().jobs.len(), 1);
    assert!(matches!(
        s.state(1),
        InputState::Finished(InputOutcome::Completed)
    ));
}

#[test]
fn pure_reasoning_continues_without_tools_or_a_second_kernel_model() {
    let mut s = Scenario::new();
    let (job, mut call) = s.create(1, "reason");
    for _ in 0..3 {
        let effects = s.work(
            call,
            WorkProposal {
                next: Next::Continue,
                ..Default::default()
            },
        );
        no_route(&effects);
        no_tool(&effects);
        call = worker(&effects, job).0;
    }
    s.work(call, answer("reasoned"));
    assert_eq!(s.job(job).state, JobState::Completed);
}

#[test]
fn changed_goal_revokes_old_call_but_late_failure_cannot_pause_replacement() {
    for outcome in [
        CallOutcome::work(answer("obsolete")),
        CallOutcome::failed("old failure"),
        cancelled(),
    ] {
        let mut s = Scenario::new();
        let (job, first) = s.create(1, "A");
        let old = worker(
            &s.work(
                first,
                WorkProposal {
                    next: Next::Continue,
                    ..Default::default()
                },
            ),
            job,
        )
        .0;
        let (route, _) = routing(&s.say(2, "replace A by B"));
        let effects = s.finish(
            route,
            decision(vec![update(
                job,
                Some("B"),
                JobAction::Keep,
                vec![InputId(2)],
                true,
            )]),
        );
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::RequestCancel { id } if *id == old))
        );
        let replacement = worker(&effects, job).0;
        assert_ne!(replacement, old);
        assert!(s.job(job).version > 0);
        let late = s.finish(old, outcome);
        assert!(replies(&late).is_empty());
        no_tool(&late);
        assert_eq!(s.job(job).active_call, Some(replacement));
        assert_eq!(s.job(job).goal, "B");
        s.work(replacement, answer("B done"));
        assert_eq!(s.job(job).state, JobState::Completed);
    }
}

#[test]
fn input_control_commits_before_a_later_batch_and_does_not_wait_for_an_answer() {
    let mut s = Scenario::new();
    let (job, _) = s.create(1, "A");
    let (pause, _) = routing(&s.say(2, "pause A"));
    s.say(3, "resume A read only");
    let effects = s.finish(
        pause,
        decision(vec![update(job, None, JobAction::Pause, vec![], false)]),
    );
    assert_eq!(s.job(job).state, JobState::Paused);
    assert!(matches!(
        s.state(2),
        InputState::Finished(InputOutcome::Completed)
    ));
    let (resume, request) = routing(&effects);
    assert!(
        matches!(request.task, ModelTask::Kernel { inputs, .. } if inputs.iter().map(|input| input.id).collect::<Vec<_>>() == vec![InputId(3)])
    );
    s.finish(
        resume,
        decision(vec![update(
            job,
            Some("A read only"),
            JobAction::Resume,
            vec![InputId(3)],
            true,
        )]),
    );
    assert_ne!(s.job(job).state, JobState::Paused);
}

#[test]
fn routing_changes_are_atomic_when_a_later_target_is_invalid() {
    let mut s = Scenario::new();
    let (job, _) = s.create(1, "A");
    let (route, _) = routing(&s.say(2, "pause both"));
    let before = s.job(job);
    let effects = s.finish(
        route,
        decision(vec![
            update(job, None, JobAction::Pause, vec![], false),
            update(JobId(9999), None, JobAction::Cancel, vec![], false),
        ]),
    );
    assert_eq!(s.job(job), before);
    assert!(
        notices(&effects)
            .iter()
            .any(|notice| matches!(notice, Notice::InputRoutingFailed { .. }))
    );
}

#[test]
fn unknown_input_holds_an_old_answer_then_finite_drain_allows_delivery() {
    let mut s = Scenario::new();
    let (job, old) = s.create(1, "A");
    let (route, _) = routing(&s.say(2, "unrelated input"));
    let held = s.work(old, answer("A answer"));
    assert!(replies(&held).is_empty());
    assert_eq!(
        s.job(job).state,
        JobState::Waiting(WaitReason::Coordination)
    );
    assert_eq!(s.job(job).active_call, None);
    assert!(!s.call(old).is_running());
    assert!(notices(&held).iter().any(|notice| matches!(notice,
        Notice::JobChanged { job: changed }
            if changed.id == job && changed.state == JobState::Waiting(WaitReason::Coordination))));
    assert_eq!(
        s.kernel.admit(&input(3, "not accepted")),
        Err(AdmissionError::Busy)
    );
    let effects = s.finish(route, decision(vec![create("ack", vec![InputId(2)])]));
    assert_eq!(replies(&effects), ["A answer"]);
    assert_eq!(s.job(job).state, JobState::Completed);
    assert_eq!(s.kernel.admit(&input(3, "not accepted")), Ok(None));
}

#[test]
fn all_accepted_batches_are_explained_before_held_answer_publication() {
    let mut s = Scenario::new();
    let (_, old) = s.create(1, "A");
    let (first, _) = routing(&s.say(2, "E1"));
    s.say(3, "E2");
    assert!(replies(&s.work(old, answer("held"))).is_empty());
    let effects = s.finish(first, decision(vec![create("ack E1", vec![InputId(2)])]));
    assert!(replies(&effects).is_empty());
    let (second, _) = routing(&effects);
    assert_eq!(
        replies(&s.finish(second, decision(vec![create("ack E2", vec![InputId(3)])]))),
        ["held"]
    );
}

#[test]
fn input_capacity_counts_inflight_and_queued_inputs_and_duplicates_use_no_slot() {
    let mut s = Scenario::new();
    s.say(1, "first");
    for id in 2..=32 {
        s.say(id, "queued");
    }
    let receipt = s.kernel.receipt(InputId(1)).unwrap();
    assert_eq!(s.kernel.admit(&input(1, "first")), Ok(Some(receipt)));
    assert_eq!(
        s.kernel.admit(&input(33, "overflow")),
        Err(AdmissionError::Busy)
    );
    assert_eq!(s.kernel.snapshot().inputs.len(), 32);
    assert!(s.kernel.receipt(InputId(33)).is_none());
}

#[test]
fn same_input_id_with_different_text_or_reply_target_is_rejected() {
    let mut s = Scenario::new();
    s.say(91, "same");
    assert_eq!(
        s.kernel.admit(&input(91, "changed")),
        Err(AdmissionError::ConflictingInput)
    );
    assert_eq!(
        s.kernel.admit(&input(91, "same").replying_to(InputId(5))),
        Err(AdmissionError::ConflictingInput)
    );
    assert_eq!(
        s.kernel.admit(&input(92, "reply").replying_to(InputId(5))),
        Err(AdmissionError::InvalidReply)
    );
}

#[test]
fn clarification_can_enter_while_a_candidate_closes_ordinary_admission() {
    let mut s = Scenario::new();
    let (_, old) = s.create(1, "A");
    let (route, _) = routing(&s.say(2, "pause that one"));
    s.work(old, answer("old"));
    let effects = s.finish(
        route,
        CallOutcome::kernel(KernelDecision {
            disposition: RoutingDisposition::Clarify {
                question: "which job?".into(),
            },
            ..Default::default()
        }),
    );
    assert!(
        notices(&effects)
            .iter()
            .any(|notice| matches!(notice, Notice::Clarification { .. }))
    );
    let reply = input(3, "A").replying_to(InputId(2));
    assert_eq!(s.kernel.admit(&reply), Ok(None));
    let effects = s.kernel.step(Event::Input(reply));
    let (_, request) = routing(&effects);
    assert!(
        matches!(request.task, ModelTask::Kernel { inputs, .. } if inputs.iter().any(|input| input.id == InputId(2)) && inputs.iter().any(|input| input.id == InputId(3)))
    );
}

#[test]
fn kernel_failure_keeps_original_input_and_requires_explicit_retry() {
    let mut s = Scenario::new();
    let (route, _) = routing(&s.say(1, "original"));
    let effects = s.finish(route, CallOutcome::failed("routing failed"));
    no_start(&effects);
    assert!(matches!(s.state(1), InputState::RoutingFailed { .. }));
    assert!(notices(&effects).iter().any(|notice| matches!(notice, Notice::InputRoutingFailed { inputs, .. } if inputs == &vec![InputId(1)])));
    s.kernel.validate_retry(InputId(1)).unwrap();
    let (retry, request) = routing(&s.kernel.step(Event::RetryInput { id: InputId(1) }));
    assert_ne!(retry, route);
    assert!(
        matches!(request.task, ModelTask::Kernel { inputs, .. } if inputs == vec![input(1, "original")])
    );
    s.finish(retry, decision(vec![create("resolved", vec![InputId(1)])]));
    assert!(s.kernel.validate_retry(InputId(1)).is_err());
}

#[test]
fn failed_routing_does_not_publish_a_preexisting_candidate() {
    let mut s = Scenario::new();
    let (job, old) = s.create(1, "A");
    let (route, _) = routing(&s.say(2, "change"));
    s.work(old, answer("held"));
    let effects = s.finish(route, CallOutcome::failed("failed"));
    assert!(replies(&effects).is_empty());
    assert!(matches!(s.state(2), InputState::RoutingFailed { .. }));
    assert_eq!(
        s.job(job).state,
        JobState::Waiting(WaitReason::Coordination)
    );
    assert_eq!(s.job(job).active_call, None);
}

#[test]
fn stop_revokes_pending_routing_and_late_route_cannot_create_jobs() {
    let mut s = Scenario::new();
    let (route, _) = routing(&s.say(1, "new task"));
    let stopped = s.kernel.step(Event::Stop);
    assert!(
        stopped
            .iter()
            .any(|effect| matches!(effect, Effect::RequestCancel { id } if *id == route))
    );
    let effects = s.finish(route, decision(vec![create("late", vec![InputId(1)])]));
    no_start(&effects);
    assert!(s.kernel.snapshot().jobs.is_empty());
    assert!(matches!(
        s.state(1),
        InputState::Finished(InputOutcome::Cancelled)
    ));
    assert!(s.kernel.validate_retry(InputId(1)).is_err());
}

#[test]
fn stop_prevents_old_success_failure_progress_and_timer_from_resuming_jobs() {
    for outcome in [
        CallOutcome::work(answer("old")),
        CallOutcome::failed("old"),
        cancelled(),
    ] {
        let mut s = Scenario::new();
        let (job, call) = s.create(1, "A");
        let timer = wake(&s.work(
            call,
            WorkProposal {
                next: Next::Wait {
                    reconsider_after: Some(Duration::from_secs(10)),
                },
                ..Default::default()
            },
        ));
        let wake_effects = s.kernel.step(Event::Wake { id: timer });
        let old = worker(&wake_effects, job).0;
        s.kernel.step(Event::Stop);
        no_start(&s.finish(old, outcome));
        no_start(&s.kernel.step(Event::Wake { id: timer }));
        no_start(&s.kernel.step(Event::CallProgress {
            id: old,
            progress: CallProgress {
                message: "late".into(),
                percent: None,
            },
        }));
        assert!(matches!(
            s.job(job).state,
            JobState::Paused | JobState::Cancelled
        ));
    }
}

#[test]
fn a_new_short_job_after_stop_does_not_resume_the_old_job() {
    let mut s = Scenario::new();
    let (old, call) = s.create(1, "A");
    s.kernel.step(Event::Stop);
    s.finish(call, cancelled());
    let (thanks, worker) = s.create(2, "Thanks");
    s.work(worker, answer("welcome"));
    assert_eq!(s.job(thanks).state, JobState::Completed);
    assert!(matches!(
        s.job(old).state,
        JobState::Paused | JobState::Cancelled
    ));
}

#[test]
fn completing_one_of_three_jobs_keeps_other_work_and_its_timer() {
    let mut s = Scenario::new();
    let (a, call) = s.create(1, "A");
    let a_call = worker(
        &s.work(
            call,
            WorkProposal {
                next: Next::Continue,
                ..Default::default()
            },
        ),
        a,
    )
    .0;
    let (c, call) = s.create(2, "monitor C");
    let timer = wake(&s.work(
        call,
        WorkProposal {
            next: Next::Wait {
                reconsider_after: Some(Duration::from_secs(30)),
            },
            ..Default::default()
        },
    ));
    let (b, call) = s.create(3, "B");
    let effects = s.work(call, answer("B delivered"));
    assert_eq!(s.job(b).state, JobState::Completed);
    assert_eq!(s.job(a).active_call, Some(a_call));
    assert_eq!(s.job(c).state, JobState::Waiting(WaitReason::Timer));
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::CancelWake { id } if *id == timer))
    );
    worker(&s.kernel.step(Event::Wake { id: timer }), c);
}

#[test]
fn input_completion_waits_for_all_delivery_jobs_and_replies_keep_ownership() {
    let mut s = Scenario::new();
    let (route, _) = routing(&s.say(1, "report and code"));
    s.finish(
        route,
        decision(vec![
            create("report", vec![InputId(1)]),
            create("code", vec![InputId(1)]),
        ]),
    );
    let jobs = s.kernel.snapshot().jobs;
    assert_eq!(s.kernel.snapshot().inputs[0].required_jobs.len(), 2);
    for (index, job) in jobs.iter().enumerate() {
        let call = s.job(job.id).active_call.expect("next deliverable starts");
        let effects = s.work(call, answer(&job.goal));
        assert!(notices(&effects).iter().any(|notice| matches!(notice, Notice::Reply { job: owner, reply_to, .. } if *owner == job.id && reply_to == &vec![InputId(1)])));
        if index == 0 {
            assert_eq!(s.state(1), InputState::Handled);
        }
    }
    assert_eq!(s.state(1), InputState::Finished(InputOutcome::Completed));
}

#[test]
fn control_acknowledgement_does_not_wait_for_the_affected_jobs_delivery() {
    let mut s = Scenario::new();
    let (job, _) = s.create(1, "A");
    let (route, _) = routing(&s.say(2, "pause A"));
    s.finish(
        route,
        decision(vec![update(job, None, JobAction::Pause, vec![], false)]),
    );
    assert_eq!(s.state(2), InputState::Finished(InputOutcome::Completed));
    assert_ne!(s.state(1), InputState::Finished(InputOutcome::Completed));
}

#[test]
fn ordinary_progress_neither_invokes_kernel_nor_revokes_the_worker() {
    let mut s = Scenario::new();
    let (job, call) = s.create(1, "A");
    let effects = s.kernel.step(Event::CallProgress {
        id: call,
        progress: CallProgress {
            message: "public".into(),
            percent: Some(20),
        },
    });
    no_start(&effects);
    assert_eq!(s.job(job).active_call, Some(call));
    assert_eq!(
        replies(&s.work(call, answer("still valid"))),
        ["still valid"]
    );
}

#[test]
fn additional_tool_evidence_does_not_invalidate_a_snapshot_answer() {
    let mut s = Scenario::new();
    let (job, call) = s.create(1, "A");
    let effects = s.work(
        call,
        WorkProposal {
            next: Next::Continue,
            ..operation("lookup")
        },
    );
    let read = tool(&effects, "lookup");
    let (held, request) = worker(&effects, job);
    s.finish(read, CallOutcome::artifact("appended material"));
    let effects = s.work(held, answer("as of my snapshot"));
    assert_eq!(replies(&effects), ["as of my snapshot"]);
    assert!(notices(&effects).iter().any(|notice| matches!(notice, Notice::Reply { as_of, .. } if *as_of == request.snapshot.record_cursor)));
}

#[test]
fn waiting_and_expired_timers_do_not_consume_calls_or_fire_twice() {
    let mut s = Scenario::new();
    let (job, call) = s.create(1, "monitor");
    let timer = wake(&s.work(
        call,
        WorkProposal {
            next: Next::Wait {
                reconsider_after: Some(Duration::from_secs(7)),
            },
            ..Default::default()
        },
    ));
    assert_eq!(s.job(job).active_call, None);
    let effects = s.kernel.step(Event::Wake { id: timer });
    worker(&effects, job);
    no_route(&effects);
    no_start(&s.kernel.step(Event::Wake { id: timer }));
}

#[test]
fn round_robin_moves_continuations_behind_other_background_work() {
    let mut s = Scenario::with_config(KernelConfig {
        background_concurrency: 1,
        ..Default::default()
    });
    let (a, first) = s.create(1, "A");
    let a_running = worker(
        &s.work(
            first,
            WorkProposal {
                next: Next::Continue,
                ..Default::default()
            },
        ),
        a,
    )
    .0;
    let (b, first) = s.create(2, "B");
    no_start(&s.work(
        first,
        WorkProposal {
            next: Next::Continue,
            ..Default::default()
        },
    ));
    let effects = s.work(
        a_running,
        WorkProposal {
            next: Next::Continue,
            ..Default::default()
        },
    );
    let b_running = worker(&effects, b).0;
    assert_eq!(s.job(a).active_call, None);
    worker(
        &s.work(
            b_running,
            WorkProposal {
                next: Next::Continue,
                ..Default::default()
            },
        ),
        a,
    );
}

#[test]
fn unknown_write_survives_job_cancel_and_only_host_resolution_releases_it() {
    let mut s = Scenario::new();
    let (job, call) = s.create(1, "write");
    let write = tool(&s.work(call, operation("write")), "write");
    s.finish(write, CallOutcome::unknown("lost reply"));
    let (route, _) = routing(&s.say(2, "cancel job"));
    s.finish(
        route,
        decision(vec![update(job, None, JobAction::Cancel, vec![], false)]),
    );
    assert_eq!(s.job(job).state, JobState::Cancelled);
    assert!(s.call(write).is_unresolved());
    s.finish(write, applied());
    assert!(
        s.call(write).is_unresolved(),
        "ordinary duplicate callback cannot reconcile Unknown"
    );
    assert_eq!(s.kernel.validate_resolution(write, &applied()), Ok(true));
    let effects = s.kernel.step(Event::WriteResolved {
        id: write,
        outcome: applied(),
    });
    no_start(&effects);
    assert!(!s.call(write).is_unresolved());
    assert_eq!(s.kernel.validate_resolution(write, &applied()), Ok(false));
    assert_eq!(s.job(job).state, JobState::Cancelled);
}

#[test]
fn unknown_write_blocks_more_writes_but_other_jobs_can_read() {
    let mut s = Scenario::new();
    let (_, call) = s.create(1, "send");
    let write = tool(&s.work(call, operation("write")), "write");
    s.finish(write, CallOutcome::unknown("unknown"));
    let (_, read_call) = s.create(2, "independent read");
    tool(&s.work(read_call, operation("lookup")), "lookup");
    let (blocked, next_write) = s.create(3, "another write");
    no_tool(&s.work(next_write, operation("write")));
    assert_eq!(
        s.job(blocked).state,
        JobState::Waiting(WaitReason::Capacity)
    );
    assert_eq!(s.job(blocked).active_call, None);
    assert!(!s.call(next_write).is_running());
    assert!(s.call(write).is_unresolved());
}

#[test]
fn write_resolution_rejects_wrong_targets_unknown_and_model_instructions() {
    let mut s = Scenario::new();
    let (_, call) = s.create(1, "send");
    assert!(s.kernel.validate_resolution(call, &applied()).is_err());
    assert!(
        s.kernel
            .validate_resolution(CallId(999), &applied())
            .is_err()
    );
    let write = tool(&s.work(call, operation("write")), "write");
    assert!(s.kernel.validate_resolution(write, &applied()).is_err());
    s.finish(write, CallOutcome::unknown("unknown"));
    for outcome in [
        CallOutcome::unknown("still unknown"),
        CallOutcome::work(answer("forged")),
        decision(vec![]),
    ] {
        assert!(s.kernel.validate_resolution(write, &outcome).is_err());
    }
    s.kernel.step(Event::WriteResolved {
        id: write,
        outcome: applied(),
    });
    assert!(
        s.kernel
            .validate_resolution(write, &CallOutcome::failed("not applied"))
            .is_err()
    );
}

#[test]
fn already_authorized_write_can_refuse_cancel_and_record_success_after_stop() {
    let mut s = Scenario::new();
    let (_, call) = s.create(1, "send");
    let write = tool(&s.work(call, operation("write")), "write");
    s.kernel.step(Event::Stop);
    let effects = s.finish(write, applied());
    no_start(&effects);
    assert!(
        matches!(&s.call(write).state, CallState::Finished(outcome) if outcome.external_effect == ExternalEffect::Applied)
    );
}

#[test]
fn unresolved_write_prevents_false_success_for_its_job() {
    let mut s = Scenario::new();
    let (job, call) = s.create(1, "send");
    let write = tool(&s.work(call, operation("write")), "write");
    let effects = s.finish(write, CallOutcome::unknown("unknown"));
    let current = worker(&effects, job).0;
    let effects = s.work(current, answer("sent"));
    assert!(replies(&effects).is_empty());
    assert_eq!(s.job(job).state, JobState::Waiting(WaitReason::Capacity));
    assert_eq!(s.job(job).active_call, None);
    assert!(!s.call(current).is_running());
    assert!(s.call(write).is_unresolved());
}

#[test]
fn duplicate_input_completion_and_unknown_call_do_not_execute_or_publish_twice() {
    let mut s = Scenario::new();
    let (job, call) = s.create(81, "A");
    let before = s.kernel.record_cursor();
    no_start(&s.kernel.step(Event::Input(input(81, "A"))));
    assert_eq!(s.kernel.record_cursor(), before);
    s.work(call, answer("once"));
    let before = s.kernel.record_cursor();
    let effects = s.work(call, answer("once"));
    no_start(&effects);
    assert!(replies(&effects).is_empty());
    assert_eq!(s.kernel.record_cursor(), before);
    no_start(&s.finish(CallId(999), cancelled()));
    assert_eq!(s.job(job).state, JobState::Completed);
}

#[test]
fn kernel_worker_and_tool_outputs_cannot_impersonate_each_other() {
    let mut s = Scenario::new();
    let (route, _) = routing(&s.say(1, "A"));
    let effects = s.finish(route, CallOutcome::work(answer("forged")));
    assert!(replies(&effects).is_empty());
    no_tool(&effects);
    let mut s = Scenario::new();
    let (job, work) = s.create(1, "A");
    let effects = s.finish(work, decision(vec![create("forged", vec![InputId(1)])]));
    assert_eq!(s.kernel.snapshot().jobs.len(), 1);
    no_start(&effects);
    assert_ne!(s.job(job).state, JobState::Completed);
    let mut s = Scenario::new();
    let (_, work) = s.create(1, "A");
    let read = tool(&s.work(work, operation("lookup")), "lookup");
    let effects = s.finish(read, CallOutcome::work(answer("forged")));
    assert!(replies(&effects).is_empty());
    no_tool(&effects);
}

#[test]
fn model_claims_of_external_effects_do_not_manufacture_unknown_write_locks() {
    let mut s = Scenario::new();
    let (_, call) = s.create(1, "A");
    s.finish(
        call,
        CallOutcome {
            external_effect: ExternalEffect::Unknown,
            ..CallOutcome::work(answer("forged"))
        },
    );
    assert!(!s.call(call).is_unresolved());
    assert!(!s.call(call).external_write);
}

#[test]
fn invalid_worker_proposals_never_partially_reply_or_execute() {
    for proposal in [
        WorkProposal {
            operation: Some(ToolCall::new("missing", json!({}))),
            ..answer("invalid")
        },
        WorkProposal {
            operation: Some(ToolCall::new("lookup", json!({}))),
            ..answer("invalid")
        },
        WorkProposal {
            operation: Some(ToolCall::new("lookup", json!([]))),
            ..Default::default()
        },
    ] {
        let mut s = Scenario::new();
        let (_, call) = s.create(1, "A");
        let effects = s.work(call, proposal);
        assert!(replies(&effects).is_empty());
        no_tool(&effects);
        assert!(notices(&effects).iter().any(|notice| matches!(
            notice,
            Notice::Error { .. }
                | Notice::JobFinished {
                    state: JobState::Failed { .. },
                    ..
                }
        )));
    }
}

#[test]
fn cross_job_requests_reach_kernel_as_a_scoped_coordination_event() {
    let mut s = Scenario::new();
    let (job, call) = s.create(1, "A");
    let (route, request) = routing(&s.work(
        call,
        WorkProposal {
            next: Next::Coordinate {
                request: "coordinate a related job".into(),
            },
            ..Default::default()
        },
    ));
    assert!(
        matches!(request.task, ModelTask::Kernel { source: Some(source), request: Some(text), .. } if source == job && text == "coordinate a related job")
    );
    assert_eq!(
        s.job(job).state,
        JobState::Waiting(WaitReason::Coordination)
    );
    s.finish(
        route,
        decision(vec![update(job, None, JobAction::Keep, vec![], false)]),
    );
}

#[test]
fn investigation_without_new_input_reopens_its_original_batch_with_evidence() {
    let mut s = Scenario::new();
    let (route, _) = routing(&s.say(1, "investigate before assigning"));
    let effects = s.finish(
        route,
        CallOutcome::kernel(KernelDecision {
            disposition: RoutingDisposition::Investigate {
                goal: "read the evidence".into(),
            },
            ..Default::default()
        }),
    );
    let investigation = match s.state(1) {
        InputState::Investigating { job } => job,
        state => panic!("investigation state: {state:?}"),
    };
    let (work, _) = worker(&effects, investigation);
    no_route(&effects);
    let effects = s.work(
        work,
        WorkProposal {
            note: "evidence is available".into(),
            next: Next::Finish,
            ..Default::default()
        },
    );
    assert!(replies(&effects).is_empty());
    let (_, request) = routing(&effects);
    assert!(
        matches!(request.task, ModelTask::Kernel { inputs, .. } if inputs == vec![input(1, "investigate before assigning")])
    );
    assert!(
        request
            .snapshot
            .jobs
            .iter()
            .any(|job| job.id == investigation && job.note == "evidence is available")
    );
}

#[test]
fn new_input_takes_investigation_ownership_and_late_evidence_cannot_reopen_the_batch() {
    for finish_before_route_commit in [false, true] {
        let mut s = Scenario::new();
        let route = routing(&s.say(90, "investigate before assigning")).0;
        let effects = s.finish(
            route,
            CallOutcome::kernel(KernelDecision {
                disposition: RoutingDisposition::Investigate {
                    goal: "read the evidence".into(),
                },
                ..Default::default()
            }),
        );
        let investigation = match s.state(90) {
            InputState::Investigating { job } => job,
            state => panic!("{state:?}"),
        };
        let work = worker(&effects, investigation).0;
        let (new_route, request) = routing(&s.say(2, "resolve both now"));
        assert!(
            matches!(request.task, ModelTask::Kernel { inputs, source: None, .. }
            if inputs == vec![input(90, "investigate before assigning"), input(2, "resolve both now")])
        );
        let evidence = WorkProposal {
            note: "late evidence remains useful".into(),
            ..answer("internal investigation conclusion")
        };
        if finish_before_route_commit {
            let effects = s.work(work, evidence.clone());
            no_route(&effects);
            assert!(replies(&effects).is_empty());
            assert_eq!(s.state(90), InputState::Routing);
        }
        let effects = s.finish(
            new_route,
            decision(vec![create(
                "combined answer",
                vec![InputId(90), InputId(2)],
            )]),
        );
        let delivery = s.kernel.snapshot().jobs.last().unwrap().id;
        let delivery_call = worker(&effects, delivery).0;
        if !finish_before_route_commit {
            let effects = s.work(work, evidence);
            no_route(&effects);
            assert!(replies(&effects).is_empty());
        }
        assert_eq!(s.state(90), InputState::Handled);
        assert_eq!(s.state(2), InputState::Handled);
        assert_eq!(s.job(investigation).state, JobState::Completed);
        assert_eq!(s.job(investigation).note, "late evidence remains useful");
        assert_eq!(s.job(delivery).active_call, Some(delivery_call));
        assert!(s.kernel.validate_retry(InputId(90)).is_err());
        let effects = s.work(delivery_call, answer("combined answer"));
        assert_eq!(replies(&effects), ["combined answer"]);
        no_route(&effects);
        for id in [90, 2] {
            assert_eq!(s.state(id), InputState::Finished(InputOutcome::Completed));
        }
    }
}

#[test]
fn a_worker_question_is_a_clarification_not_a_final_answer() {
    let mut s = Scenario::new();
    let (job, call) = s.create(1, "A");
    let effects = s.work(
        call,
        WorkProposal {
            next: Next::AskUser {
                question: "which date?".into(),
            },
            ..Default::default()
        },
    );
    assert!(replies(&effects).is_empty());
    assert!(notices(&effects).iter().any(|notice| matches!(notice, Notice::Clarification { job: Some(owner), question, .. } if *owner == job && question == "which date?")));
    assert_eq!(
        s.job(job).state,
        JobState::Waiting(WaitReason::User {
            question: "which date?".into()
        })
    );
    assert_ne!(s.state(1), InputState::Finished(InputOutcome::Completed));
}

#[test]
fn waiting_for_a_result_does_not_require_the_producer_to_terminate() {
    let mut s = Scenario::new();
    let (producer, call) = s.create(1, "monitor");
    let effects = s.work(
        call,
        WorkProposal {
            next: Next::Continue,
            ..operation("lookup")
        },
    );
    let read = tool(&effects, "lookup");
    let ongoing = worker(&effects, producer).0;
    let (consumer, call) = s.create(2, "report");
    s.work(
        call,
        WorkProposal {
            next: Next::WaitForResult { job: producer },
            ..Default::default()
        },
    );
    let effects = s.finish(read, CallOutcome::artifact(json!({"result":42})));
    worker(&effects, consumer);
    assert_eq!(s.job(producer).active_call, Some(ongoing));
    assert!(!s.job(producer).state.terminal());
}

#[test]
fn identifiers_do_not_replace_actual_acceptance_order() {
    let mut s = Scenario::new();
    let (first, _) = routing(&s.say(90, "first"));
    s.say(2, "second");
    let (_, request) =
        routing(&s.finish(first, decision(vec![create("first", vec![InputId(90)])])));
    assert!(matches!(request.task, ModelTask::Kernel { inputs, .. } if inputs[0].id == InputId(2)));
    let inputs = s.kernel.snapshot().inputs;
    let a = inputs
        .iter()
        .find(|input| input.input.id == InputId(90))
        .unwrap();
    let b = inputs
        .iter()
        .find(|input| input.input.id == InputId(2))
        .unwrap();
    assert!(a.received_at < b.received_at);
}

#[test]
fn cancelled_calls_keep_their_capacity_until_their_local_completion() {
    let mut s = Scenario::with_config(KernelConfig {
        background_concurrency: 1,
        ..Default::default()
    });
    let (job, first) = s.create(1, "A");
    let old = worker(
        &s.work(
            first,
            WorkProposal {
                next: Next::Continue,
                ..Default::default()
            },
        ),
        job,
    )
    .0;
    let route = routing(&s.say(2, "B")).0;
    let replacement = worker(
        &s.finish(
            route,
            decision(vec![update(
                job,
                Some("B"),
                JobAction::Keep,
                vec![InputId(2)],
                true,
            )]),
        ),
        job,
    )
    .0;
    let route = routing(&s.say(3, "C")).0;
    let effects = s.finish(
        route,
        decision(vec![update(
            job,
            Some("C"),
            JobAction::Keep,
            vec![InputId(3)],
            true,
        )]),
    );
    assert!(
        model_calls(&effects)
            .iter()
            .all(|(_, input)| !matches!(input.task, ModelTask::Work { .. }))
    );
    assert!(s.call(old).is_running());
    assert!(s.call(replacement).is_running());
    let effects = s.finish(old, cancelled());
    let current = worker(&effects, job).0;
    assert_ne!(current, replacement);
    assert!(matches!(
        s.call(current).request,
        CallRequest::Work {
            interactive: false,
            ..
        }
    ));
    assert_eq!(
        s.kernel
            .snapshot()
            .calls
            .iter()
            .filter(|call| call.is_running() && matches!(call.request, CallRequest::Work { .. }))
            .count(),
        2
    );
    no_start(&s.finish(replacement, cancelled()));
    assert_eq!(s.job(job).active_call, Some(current));
    assert_eq!(s.job(job).goal, "C");
}

#[test]
fn late_progress_from_a_revoked_call_does_not_overwrite_replacement_progress() {
    let mut s = Scenario::new();
    let (job, first) = s.create(1, "A");
    let old = worker(
        &s.work(
            first,
            WorkProposal {
                next: Next::Continue,
                ..Default::default()
            },
        ),
        job,
    )
    .0;
    let route = routing(&s.say(2, "B")).0;
    let current = worker(
        &s.finish(
            route,
            decision(vec![update(
                job,
                Some("B"),
                JobAction::Keep,
                vec![InputId(2)],
                true,
            )]),
        ),
        job,
    )
    .0;
    let progress = CallProgress {
        message: "B new progress".into(),
        percent: Some(30),
    };
    s.kernel.step(Event::CallProgress {
        id: current,
        progress: progress.clone(),
    });
    let stale = CallProgress {
        message: "A obsolete progress".into(),
        percent: Some(90),
    };
    s.kernel.step(Event::CallProgress {
        id: old,
        progress: stale.clone(),
    });
    assert_eq!(
        s.call(old).progress,
        Some(stale),
        "retain the real call observation"
    );
    assert_eq!(
        s.job(job).progress,
        Some(progress),
        "old observations cannot replace the new job's progress"
    );
}

#[test]
fn tool_capacity_waits_without_losing_the_queued_operation() {
    let mut s = Scenario::with_config(KernelConfig {
        tool_concurrency: 1,
        ..Default::default()
    });
    let (a, call) = s.create(1, "A");
    let read = tool(&s.work(call, operation("lookup")), "lookup");
    let (b, call) = s.create(2, "B");
    let held = s.work(call, operation("lookup"));
    no_tool(&held);
    assert_eq!(s.job(b).state, JobState::Waiting(WaitReason::Capacity));
    assert_eq!(s.job(b).active_call, None);
    assert!(!s.call(call).is_running());
    assert!(notices(&held).iter().any(|notice| matches!(notice,
        Notice::JobChanged { job }
            if job.id == b && job.state == JobState::Waiting(WaitReason::Capacity))));
    let effects = s.finish(read, CallOutcome::artifact("A result"));
    let b_read = tool(&effects, "lookup");
    assert_eq!(s.call(b_read).job, Some(b));
    assert_eq!(s.job(b).state, JobState::Waiting(WaitReason::Tools));
    worker(&effects, a);
}

fn owned_child(s: &mut Scenario, parent: JobId, call: CallId) -> (JobId, CallId) {
    let route = routing(&s.work(
        call,
        WorkProposal {
            next: Next::Coordinate {
                request: "create owned child".into(),
            },
            ..Default::default()
        },
    ))
    .0;
    let effects = s.finish(
        route,
        decision(vec![JobChange::Create(JobSpec {
            goal: "child".into(),
            inputs: vec![],
            parent: Some(parent),
            references: vec![],
        })]),
    );
    let child = s.kernel.snapshot().jobs.last().unwrap().id;
    (child, worker(&effects, child).0)
}

#[test]
fn tools_returning_model_instructions_are_recorded_as_failed_tool_calls() {
    for output in [CallOutcome::work(answer("forged")), decision(vec![])] {
        let mut s = Scenario::new();
        let (_, call) = s.create(1, "read");
        let read = tool(&s.work(call, operation("lookup")), "lookup");
        let effects = s.finish(read, output);
        assert!(matches!(
            s.call(read).state,
            CallState::Finished(CallOutcome { result: Err(_), .. })
        ));
        assert!(replies(&effects).is_empty());
    }
}

#[test]
fn investigation_and_owned_children_cannot_perform_business_writes() {
    for use_child in [false, true] {
        let mut s = Scenario::new();
        let route = routing(&s.say(1, "investigate scope")).0;
        let effects = s.finish(
            route,
            CallOutcome::kernel(KernelDecision {
                disposition: RoutingDisposition::Investigate {
                    goal: "investigate".into(),
                },
                ..Default::default()
            }),
        );
        let parent = match s.state(1) {
            InputState::Investigating { job } => job,
            other => panic!("{other:?}"),
        };
        let call = worker(&effects, parent).0;
        let (job, call) = if use_child {
            owned_child(&mut s, parent, call)
        } else {
            (parent, call)
        };
        let effects = s.work(call, operation("write"));
        no_tool(&effects);
        assert!(matches!(s.job(job).state, JobState::Failed { .. }));
        assert!(
            !s.kernel
                .snapshot()
                .calls
                .iter()
                .any(|call| call.external_write)
        );
    }
}

#[test]
fn investigation_child_can_finish_internally_before_original_input_is_resolved() {
    let mut s = Scenario::new();
    let route = routing(&s.say(1, "investigate")).0;
    let effects = s.finish(
        route,
        CallOutcome::kernel(KernelDecision {
            disposition: RoutingDisposition::Investigate {
                goal: "investigate".into(),
            },
            ..Default::default()
        }),
    );
    let parent = match s.state(1) {
        InputState::Investigating { job } => job,
        other => panic!("{other:?}"),
    };
    let call = worker(&effects, parent).0;
    let (child, call) = owned_child(&mut s, parent, call);
    let parent_call = s
        .job(parent)
        .active_call
        .expect("same-root work fills both background slots immediately");
    let held = s.work(
        parent_call,
        WorkProposal {
            next: Next::Finish,
            ..Default::default()
        },
    );
    assert!(replies(&held).is_empty());
    no_start(&held);
    assert_eq!(
        s.job(parent).state,
        JobState::Waiting(WaitReason::Job { job: child })
    );
    assert_eq!(s.job(parent).active_call, None);
    assert!(!s.call(parent_call).is_running());
    let effects = s.work(
        call,
        WorkProposal {
            note: "internal evidence".into(),
            ..answer("internal conclusion")
        },
    );
    assert!(replies(&effects).is_empty());
    assert_eq!(s.job(child).state, JobState::Completed);
    assert!(matches!(s.state(1), InputState::Investigating { .. }));
    let call = s.job(parent).active_call.unwrap();
    let effects = s.work(
        call,
        WorkProposal {
            note: "investigation complete".into(),
            next: Next::Finish,
            ..Default::default()
        },
    );
    routing(&effects);
    assert!(replies(&effects).is_empty());
}

#[test]
fn a_child_cannot_wait_for_its_parent_to_finish() {
    let mut s = Scenario::new();
    let (parent, call) = s.create(1, "parent");
    let (child, call) = owned_child(&mut s, parent, call);
    s.work(
        call,
        WorkProposal {
            next: Next::WaitForJob { job: parent },
            ..Default::default()
        },
    );
    assert!(matches!(s.job(child).state, JobState::Failed { .. }));
    assert!(!s.job(parent).state.terminal());
}

#[test]
fn a_result_arriving_before_the_worker_waits_is_not_lost() {
    let mut s = Scenario::new();
    let (job, call) = s.create(1, "A");
    let effects = s.work(
        call,
        WorkProposal {
            next: Next::Continue,
            ..operation("lookup")
        },
    );
    let read = tool(&effects, "lookup");
    let held = worker(&effects, job).0;
    s.finish(read, CallOutcome::artifact("arrived while reasoning"));
    let effects = s.work(held, WorkProposal::default());
    let next = worker(&effects, job).0;
    assert_ne!(next, held);
    no_route(&effects);
}

#[test]
fn cancelling_parent_and_modifying_descendant_rejects_the_entire_decision_in_both_orders() {
    for reversed in [false, true] {
        for creating in [false, true] {
            let mut s = Scenario::new();
            let (parent, call) = s.create(1, "parent");
            let (child, _) = owned_child(&mut s, parent, call);
            let route = routing(&s.say(2, "conflicting controls")).0;
            let before = s.kernel.snapshot().jobs;
            let dependent = if creating {
                JobChange::Create(JobSpec {
                    goal: "new child".into(),
                    inputs: vec![InputId(2)],
                    parent: Some(parent),
                    references: vec![],
                })
            } else {
                update(
                    child,
                    Some("changed"),
                    JobAction::Keep,
                    vec![InputId(2)],
                    true,
                )
            };
            let mut changes = vec![
                update(parent, None, JobAction::Cancel, vec![], false),
                dependent,
            ];
            if reversed {
                changes.reverse();
            }
            let effects = s.finish(route, decision(changes));
            assert_eq!(s.kernel.snapshot().jobs, before);
            assert!(
                !effects
                    .iter()
                    .any(|effect| matches!(effect, Effect::RequestCancel { .. }))
            );
            assert!(
                notices(&effects)
                    .iter()
                    .any(|notice| matches!(notice, Notice::InputRoutingFailed { .. }))
            );
        }
    }
}

#[test]
fn three_initial_workers_can_use_reserved_and_background_capacity_but_a_fourth_waits() {
    let mut s = Scenario::new();
    let route = routing(&s.say(1, "four jobs")).0;
    let effects = s.finish(
        route,
        decision(
            (0..4)
                .map(|index| create(&format!("job {index}"), vec![InputId(1)]))
                .collect(),
        ),
    );
    assert_eq!(model_calls(&effects).len(), 3);
    let calls = s.kernel.snapshot().calls;
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.is_running()
                && matches!(
                    call.request,
                    CallRequest::Work {
                        interactive: true,
                        ..
                    }
                ))
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.is_running()
                && matches!(
                    call.request,
                    CallRequest::Work {
                        interactive: false,
                        ..
                    }
                ))
            .count(),
        2
    );
    assert_eq!(
        s.kernel
            .snapshot()
            .jobs
            .iter()
            .filter(|job| job.active_call.is_none())
            .count(),
        1
    );
}

#[test]
fn worker_created_children_use_background_capacity_and_leave_the_reserve_for_user_input() {
    let mut s = Scenario::new();
    let (parent, call) = s.create(1, "parent");
    let (child, child_call) = owned_child(&mut s, parent, call);
    assert!(matches!(
        s.call(child_call).request,
        CallRequest::Work {
            interactive: false,
            ..
        }
    ));
    assert_eq!(s.job(child).parent, Some(parent));
    let (user_job, user_call) = s.create(2, "new user work");
    assert!(matches!(
        s.call(user_call).request,
        CallRequest::Work {
            interactive: true,
            ..
        }
    ));
    assert_eq!(s.call(user_call).job, Some(user_job));
}

#[test]
fn worker_coordination_cannot_promote_its_own_continuation_to_the_interactive_lane() {
    for keep_source in [false, true] {
        let mut s = Scenario::new();
        let (job, call) = s.create(1, "parent");
        let route = routing(&s.work(
            call,
            WorkProposal {
                next: Next::Coordinate {
                    request: "confirm no cross-job change is needed".into(),
                },
                ..Default::default()
            },
        ))
        .0;
        let changes = if keep_source {
            vec![update(job, None, JobAction::Keep, vec![], false)]
        } else {
            vec![]
        };
        let effects = s.finish(route, decision(changes));
        let next = worker(&effects, job).0;
        assert!(
            matches!(
                s.call(next).request,
                CallRequest::Work {
                    interactive: false,
                    ..
                }
            ),
            "worker-origin coordination cannot grant user priority"
        );
    }
}

#[test]
fn worker_coordination_cannot_acquire_fresh_user_authority() {
    for (goal, action, inputs, required) in [
        (Some("invented new goal"), JobAction::Keep, vec![], false),
        (None, JobAction::Resume, vec![], false),
        (None, JobAction::Keep, vec![InputId(1)], false),
        (None, JobAction::Keep, vec![], true),
        (None, JobAction::Keep, vec![InputId(1)], true),
    ] {
        let mut s = Scenario::new();
        let (job, call) = s.create(1, "user goal");
        let route = routing(&s.work(
            call,
            WorkProposal {
                next: Next::Coordinate {
                    request: "change my assignment".into(),
                },
                ..Default::default()
            },
        ))
        .0;
        let effects = s.finish(
            route,
            decision(vec![update(job, goal, action, inputs, required)]),
        );
        assert_eq!(s.job(job).goal, "user goal");
        assert!(matches!(s.job(job).state, JobState::Failed { .. }));
        assert!(notices(&effects).iter().any(|notice| matches!(notice,
            Notice::InputRoutingFailed { message, .. } if message.contains("user authority"))));
        no_start(&effects);
        assert!(replies(&effects).is_empty());
    }
}

#[test]
fn a_parent_cannot_undo_the_users_pause_or_replace_the_childs_goal() {
    for (goal, action, inputs, required) in [
        (None, JobAction::Keep, vec![], false),
        (None, JobAction::Resume, vec![], false),
        (Some("parent replacement"), JobAction::Keep, vec![], false),
        (None, JobAction::Keep, vec![InputId(1)], false),
        (None, JobAction::Keep, vec![], true),
        (None, JobAction::Keep, vec![InputId(1)], true),
    ] {
        let mut s = Scenario::new();
        let (parent, call) = s.create(1, "parent");
        let (child, child_call) = owned_child(&mut s, parent, call);
        let route = routing(&s.say(2, "pause the child")).0;
        s.finish(
            route,
            decision(vec![update(child, None, JobAction::Pause, vec![], false)]),
        );
        s.finish(child_call, cancelled());
        let paused = s.job(child);
        assert_eq!(paused.state, JobState::Paused);
        let parent_call = s.job(parent).active_call.unwrap();
        let route = routing(&s.work(
            parent_call,
            WorkProposal {
                next: Next::Coordinate {
                    request: "control my child".into(),
                },
                ..Default::default()
            },
        ))
        .0;
        let forbidden =
            goal.is_some() || action == JobAction::Resume || !inputs.is_empty() || required;
        let effects = s.finish(
            route,
            decision(vec![update(child, goal, action, inputs, required)]),
        );
        assert_eq!(
            s.job(child),
            paused,
            "parent coordination must not lift the user pause"
        );
        if forbidden {
            assert!(notices(&effects).iter().any(|notice| matches!(notice,
                Notice::InputRoutingFailed { message, .. } if message.contains("user authority"))));
            no_start(&effects);
        } else {
            let call = worker(&effects, parent).0;
            assert!(matches!(
                s.call(call).request,
                CallRequest::Work {
                    interactive: false,
                    ..
                }
            ));
        }
        let route = routing(&s.say(3, "resume the child myself")).0;
        let effects = s.finish(
            route,
            decision(vec![update(
                child,
                None,
                JobAction::Resume,
                vec![InputId(3)],
                false,
            )]),
        );
        let call = worker(&effects, child).0;
        assert!(matches!(
            s.call(call).request,
            CallRequest::Work {
                interactive: true,
                ..
            }
        ));
    }
}

#[test]
fn a_new_resume_input_reinterprets_failed_pause_words_in_received_order() {
    for first_id in [1, 90] {
        let mut s = Scenario::new();
        let (job, old_call) = s.create(100, "ongoing user work");
        let route = routing(&s.say(first_id, "pause the ongoing work")).0;
        let effects = s.finish(route, CallOutcome::failed("routing unavailable"));
        assert!(matches!(
            s.state(first_id),
            InputState::RoutingFailed { .. }
        ));
        assert_eq!(s.kernel.validate_retry(InputId(first_id)), Ok(()));
        no_route(&effects);
        no_route(&s.kernel.step(Event::CallProgress {
            id: old_call,
            progress: CallProgress {
                message: "old work continues".into(),
                percent: None,
            },
        }));
        let (route, request) = routing(&s.say(2, "resume instead"));
        assert!(
            matches!(request.task, ModelTask::Kernel { inputs, source: None, .. }
            if inputs == vec![input(first_id, "pause the ongoing work"), input(2, "resume instead")])
        );
        assert!(s.kernel.validate_retry(InputId(first_id)).is_err());
        let effects = s.finish(
            route,
            decision(vec![update(
                job,
                None,
                JobAction::Resume,
                vec![InputId(first_id), InputId(2)],
                false,
            )]),
        );
        assert!(
            notices(&effects).iter().any(|notice| matches!(notice,
            Notice::InputHandled { inputs, .. } if inputs == &vec![InputId(first_id), InputId(2)]))
        );
        for id in [first_id, 2] {
            assert_eq!(s.state(id), InputState::Finished(InputOutcome::Completed));
            assert!(s.kernel.validate_retry(InputId(id)).is_err());
        }
        let current = s.job(job);
        no_start(&s.kernel.step(Event::RetryInput {
            id: InputId(first_id),
        }));
        assert_eq!(s.job(job), current);
    }
}

#[test]
fn a_new_ordinary_input_includes_words_waiting_for_clarification() {
    let mut s = Scenario::new();
    let route = routing(&s.say(90, "change the schedule")).0;
    let effects = s.finish(
        route,
        CallOutcome::kernel(KernelDecision {
            disposition: RoutingDisposition::Clarify {
                question: "which schedule?".into(),
            },
            ..Default::default()
        }),
    );
    assert!(matches!(s.state(90), InputState::WaitingForUser { .. }));
    no_route(&effects);
    no_route(&s.kernel.step(Event::Wake { id: WakeId(999) }));
    let (route, request) = routing(&s.say(2, "leave all schedules unchanged"));
    assert!(
        matches!(request.task, ModelTask::Kernel { inputs, source: None, .. }
        if inputs == vec![input(90, "change the schedule"), input(2, "leave all schedules unchanged")])
    );
    s.finish(route, decision(vec![]));
    for id in [90, 2] {
        assert_eq!(s.state(id), InputState::Finished(InputOutcome::Completed));
        assert!(s.kernel.validate_retry(InputId(id)).is_err());
    }
}

#[test]
fn cancelling_an_input_owning_investigation_fails_routing_and_allows_explicit_retry() {
    let mut s = Scenario::new();
    let route = routing(&s.say(1, "investigate scope")).0;
    let effects = s.finish(
        route,
        CallOutcome::kernel(KernelDecision {
            disposition: RoutingDisposition::Investigate {
                goal: "read evidence".into(),
            },
            ..Default::default()
        }),
    );
    let investigation = match s.state(1) {
        InputState::Investigating { job } => job,
        other => panic!("{other:?}"),
    };
    let call = worker(&effects, investigation).0;
    let route = routing(&s.work(
        call,
        WorkProposal {
            next: Next::Coordinate {
                request: "cancel this investigation".into(),
            },
            ..Default::default()
        },
    ))
    .0;
    let effects = s.finish(
        route,
        decision(vec![update(
            investigation,
            None,
            JobAction::Cancel,
            vec![],
            false,
        )]),
    );
    assert_eq!(s.job(investigation).state, JobState::Cancelled);
    assert!(matches!(s.state(1), InputState::RoutingFailed { .. }));
    assert!(notices(&effects).iter().any(|notice| matches!(notice,
        Notice::InputRoutingFailed { inputs, .. } if inputs == &vec![InputId(1)])));
    assert_eq!(s.kernel.validate_retry(InputId(1)), Ok(()));
    no_route(&effects);
    no_route(&s.kernel.step(Event::Wake { id: WakeId(999) }));
    let (retry, request) = routing(&s.kernel.step(Event::RetryInput { id: InputId(1) }));
    assert!(
        matches!(request.task, ModelTask::Kernel { inputs, source: None, .. }
        if inputs == vec![input(1, "investigate scope")])
    );
    no_route(&s.work(call, answer("obsolete investigation result")));
    s.finish(retry, decision(vec![]));
    assert_eq!(s.state(1), InputState::Finished(InputOutcome::Completed));
    assert_eq!(s.job(investigation).state, JobState::Cancelled);
}

#[test]
fn held_mutual_waits_are_revalidated_at_commit_before_any_invalid_reply() {
    for reverse in [false, true] {
        let mut s = Scenario::new();
        let (a, a_call) = s.create(1, "A");
        let (b, b_call) = s.create(2, "B");
        let route = routing(&s.say(3, "check whether these answers still apply")).0;
        let mut order = [(a, a_call, b, "A waits"), (b, b_call, a, "B waits")];
        if reverse {
            order.reverse();
        }
        for (_, call, target, reply) in order {
            let effects = s.work(
                call,
                WorkProposal {
                    reply: Some(reply.into()),
                    next: Next::WaitForJob { job: target },
                    ..Default::default()
                },
            );
            assert!(replies(&effects).is_empty());
            no_start(&effects);
        }
        assert!(!s.job(a).state.terminal());
        assert!(!s.job(b).state.terminal());
        let effects = s.finish(route, decision(vec![]));
        let (first, _, _, first_reply) = order[0];
        let (second, _, _, _) = order[1];
        assert_eq!(replies(&effects), [first_reply]);
        assert!(
            matches!(s.job(second).state, JobState::Failed { ref message } if message.contains("cycle"))
        );
        assert!(
            s.job(second).results.is_empty(),
            "invalid reply must not be partially committed"
        );
        worker(&effects, first);
        no_route(&effects);
        assert!(notices(&effects).iter().any(|notice| matches!(notice,
            Notice::JobFinished { id, state: JobState::Failed { .. } } if *id == second)));
    }
}

#[test]
fn fifo_tool_candidates_prevent_a_fast_low_id_job_from_starving_b() {
    let mut s = Scenario::with_config(KernelConfig {
        tool_concurrency: 1,
        ..Default::default()
    });
    let (a, call) = s.create(1, "fast A");
    let proposal = WorkProposal {
        next: Next::Continue,
        ..operation("lookup")
    };
    let effects = s.work(call, proposal.clone());
    let mut a_tool = tool(&effects, "lookup");
    let mut a_call = worker(&effects, a).0;
    let (b, mut b_call) = s.create(2, "waiting B");
    assert!(a < b);
    for round in 0..4 {
        no_tool(&s.work(b_call, operation("lookup")));
        no_tool(&s.work(a_call, proposal.clone()));
        let effects = s.finish(a_tool, CallOutcome::artifact(json!({"a_round": round})));
        let b_tool = tool(&effects, "lookup");
        assert_eq!(
            s.call(b_tool).job,
            Some(b),
            "B was queued before A's next request"
        );
        assert_eq!(
            s.kernel
                .snapshot()
                .calls
                .iter()
                .filter(|call| call.is_running() && matches!(call.request, CallRequest::Tool(_)))
                .count(),
            1
        );
        let effects = s.finish(b_tool, CallOutcome::artifact(json!({"b_round": round})));
        a_tool = tool(&effects, "lookup");
        assert_eq!(s.call(a_tool).job, Some(a));
        a_call = worker(&effects, a).0;
        b_call = worker(&effects, b).0;
        no_route(&effects);
    }
}

#[test]
fn resolving_unknown_recomputes_owner_and_references_without_dispatching_stale_candidates() {
    for reference_candidate in [false, true] {
        let mut s = Scenario::new();
        let (unrelated, unrelated_call) =
            s.create(1, "unrelated work retains the interactive lane");
        let (owner, call) = s.create(2, "send once");
        let write = tool(&s.work(call, operation("write")), "write");
        let effects = s.finish(write, CallOutcome::unknown("receipt was lost"));
        let stale_owner = worker(&effects, owner).0;
        let route = routing(&s.say(3, "report the send status")).0;
        let effects = s.finish(
            route,
            decision(vec![JobChange::Create(JobSpec {
                goal: "report the send status".into(),
                inputs: vec![InputId(3)],
                parent: None,
                references: vec![owner],
            })]),
        );
        let reference = s.kernel.snapshot().jobs.last().unwrap().id;
        let stale_reference = worker(&effects, reference).0;
        let route = routing(&s.say(4, "check whether the result is still wanted")).0;
        no_tool(&s.work(stale_owner, operation("write")));
        if reference_candidate {
            let effects = s.work(stale_reference, answer("send status still unknown"));
            assert!(replies(&effects).is_empty());
        }
        let owner_version = s.job(owner).version;
        let reference_version = s.job(reference).version;
        let unrelated_before = s.job(unrelated);
        assert_eq!(s.kernel.validate_resolution(write, &applied()), Ok(true));
        let effects = s.kernel.step(Event::WriteResolved {
            id: write,
            outcome: applied(),
        });
        no_tool(&effects);
        no_route(&effects);
        assert!(replies(&effects).is_empty());
        assert!(s.job(owner).version > owner_version);
        assert!(s.job(reference).version > reference_version);
        assert_eq!(s.job(unrelated), unrelated_before);
        assert_eq!(s.job(unrelated).active_call, Some(unrelated_call));
        let (new_owner, request) = worker(&effects, owner);
        assert_ne!(new_owner, stale_owner);
        assert!(request.snapshot.calls.iter().any(|call| call.id == write
            && matches!(&call.state, CallState::Finished(outcome) if outcome.external_effect == ExternalEffect::Applied)));
        let reference_effects = if reference_candidate {
            effects
        } else {
            assert_eq!(s.call(stale_reference).state, CallState::CancelRequested);
            assert_eq!(
                s.job(reference).active_call,
                None,
                "unended revoked call still consumes background capacity"
            );
            let late = s.work(stale_reference, answer("send status still unknown"));
            assert!(replies(&late).is_empty());
            no_tool(&late);
            late
        };
        let (new_reference, request) = worker(&reference_effects, reference);
        assert_ne!(new_reference, stale_reference);
        assert!(request.snapshot.jobs.iter().any(|job| {
            job.id == owner
                && job
                    .results
                    .iter()
                    .any(|value| value["value"]["receipt"] == "saved")
        }));
        let before_duplicate = s.kernel.snapshot().jobs;
        assert_eq!(s.kernel.validate_resolution(write, &applied()), Ok(false));
        no_start(&s.kernel.step(Event::WriteResolved {
            id: write,
            outcome: applied(),
        }));
        assert_eq!(s.kernel.snapshot().jobs, before_duplicate);
        let effects = s.finish(route, decision(vec![]));
        no_tool(&effects);
        assert!(
            replies(&effects).is_empty(),
            "opening input admission must not release the old answer"
        );
        let effects = s.work(new_owner, answer("already sent; no retry"));
        assert_eq!(replies(&effects), ["already sent; no retry"]);
        no_tool(&effects);
        let effects = s.work(new_reference, answer("delivery confirmed"));
        assert_eq!(replies(&effects), ["delivery confirmed"]);
        assert_eq!(
            s.kernel
                .snapshot()
                .calls
                .iter()
                .filter(|call| call.external_write)
                .count(),
            1
        );
    }
}

#[test]
fn required_new_words_revoke_the_old_call_even_without_a_goal_change() {
    for old_finishes_before_routing in [false, true] {
        let mut s = Scenario::new();
        let (job, old_call) = s.create(90, "prepare the report");
        let before = s.job(job);
        let route = routing(&s.say(2, "also include the exact budget numbers")).0;
        if old_finishes_before_routing {
            let effects = s.work(old_call, answer("report without the budget"));
            assert!(replies(&effects).is_empty());
            assert_eq!(s.state(2), InputState::Routing);
        }
        let effects = s.finish(
            route,
            decision(vec![update(
                job,
                None,
                JobAction::Keep,
                vec![InputId(2)],
                true,
            )]),
        );
        assert_eq!(s.job(job).goal, before.goal);
        assert!(s.job(job).version > before.version);
        assert_eq!(s.state(2), InputState::Handled);
        assert!(replies(&effects).is_empty());
        assert!(
            !notices(&effects)
                .iter()
                .any(|notice| matches!(notice, Notice::InputFinished { id: InputId(2), .. }))
        );
        if !old_finishes_before_routing {
            assert_eq!(s.call(old_call).state, CallState::CancelRequested);
            assert!(effects.iter().any(|effect| matches!(effect,
                Effect::RequestCancel { id } if *id == old_call)));
        }
        let (replacement, request) = worker(&effects, job);
        assert_ne!(replacement, old_call);
        assert!(matches!(request.task, ModelTask::Work { messages, .. }
            if messages == vec![input(90, "prepare the report"), input(2, "also include the exact budget numbers")]));
        let late = s.work(old_call, answer("report without the budget"));
        assert!(replies(&late).is_empty());
        assert_eq!(s.job(job).active_call, Some(replacement));
        assert_eq!(s.state(2), InputState::Handled);
        let effects = s.work(replacement, answer("report including budget numbers"));
        assert_eq!(replies(&effects), ["report including budget numbers"]);
        assert!(notices(&effects).iter().any(|notice| matches!(notice,
            Notice::Reply { reply_to, .. } if reply_to == &vec![InputId(90), InputId(2)])));
        for id in [90, 2] {
            assert_eq!(s.state(id), InputState::Finished(InputOutcome::Completed));
        }
    }
}

#[test]
fn created_and_updated_job_inputs_follow_receipt_order_not_model_array_order() {
    let mut s = Scenario::new();
    let gate = routing(&s.say(1000, "no work needed for this input")).0;
    s.say(90, "first words");
    s.say(2, "second words");
    let route = routing(&s.finish(gate, decision(vec![]))).0;
    let effects = s.finish(
        route,
        decision(vec![create(
            "combined work",
            vec![InputId(2), InputId(90), InputId(2)],
        )]),
    );
    let job = s.kernel.snapshot().jobs[0].id;
    let (call, request) = worker(&effects, job);
    assert_eq!(s.job(job).inputs, [InputId(90), InputId(2)]);
    assert!(matches!(request.task, ModelTask::Work { messages, .. }
        if messages == vec![input(90, "first words"), input(2, "second words")]));
    let gate = routing(&s.work(
        call,
        WorkProposal {
            next: Next::Coordinate {
                request: "check for cross-job impact".into(),
            },
            ..Default::default()
        },
    ))
    .0;
    s.say(70, "third words");
    s.say(1, "fourth words");
    let route = routing(&s.finish(gate, decision(vec![]))).0;
    let effects = s.finish(
        route,
        decision(vec![update(
            job,
            None,
            JobAction::Keep,
            vec![InputId(1), InputId(70), InputId(1)],
            true,
        )]),
    );
    assert_eq!(
        s.job(job).inputs,
        [InputId(90), InputId(2), InputId(70), InputId(1)]
    );
    let (call, request) = worker(&effects, job);
    assert!(matches!(request.task, ModelTask::Work { messages, .. }
        if messages == vec![input(90, "first words"), input(2, "second words"), input(70, "third words"), input(1, "fourth words")]));
    let effects = s.work(call, answer("all four inputs handled"));
    assert!(notices(&effects).iter().any(|notice| matches!(notice,
        Notice::Reply { reply_to, .. } if reply_to == &vec![InputId(90), InputId(2), InputId(70), InputId(1)])));
}

#[test]
fn keep_without_inputs_does_not_wake_waiting_jobs_or_cancel_their_timers() {
    for waiting_for in ["timer", "tool", "user", "job", "result"] {
        let mut s = Scenario::new();
        let (dependency, _) = s.create(1, "unfinished dependency");
        let (job, call) = s.create(2, "waiting work");
        let proposal = match waiting_for {
            "timer" => WorkProposal {
                next: Next::Wait {
                    reconsider_after: Some(Duration::from_secs(7)),
                },
                ..Default::default()
            },
            "tool" => operation("lookup"),
            "user" => WorkProposal {
                next: Next::AskUser {
                    question: "which date?".into(),
                },
                ..Default::default()
            },
            "job" => WorkProposal {
                next: Next::WaitForJob { job: dependency },
                ..Default::default()
            },
            "result" => WorkProposal {
                next: Next::WaitForResult { job: dependency },
                ..Default::default()
            },
            _ => unreachable!(),
        };
        let waiting_effects = s.work(call, proposal);
        let waiting = s.job(job);
        assert!(matches!(waiting.state, JobState::Waiting(_)));
        for (id, required) in [(3, false), (4, true)] {
            let route = routing(&s.say(id, "leave this job waiting")).0;
            let effects = s.finish(
                route,
                decision(vec![update(job, None, JobAction::Keep, vec![], required)]),
            );
            no_start(&effects);
            assert_eq!(
                s.job(job),
                waiting,
                "empty Keep changed a {waiting_for} wait"
            );
            assert!(
                !effects
                    .iter()
                    .any(|effect| matches!(effect, Effect::CancelWake { .. }))
            );
        }
        if waiting_for == "timer" {
            let timer = wake(&waiting_effects);
            worker(&s.kernel.step(Event::Wake { id: timer }), job);
        }
    }
}

#[test]
fn keep_with_new_inputs_cancels_the_old_timer_before_starting_work() {
    for required in [false, true] {
        let mut s = Scenario::new();
        let (job, call) = s.create(90, "monitor every seven seconds");
        let wait = WorkProposal {
            next: Next::Wait {
                reconsider_after: Some(Duration::from_secs(7)),
            },
            ..Default::default()
        };
        let old_timer = wake(&s.work(call, wait.clone()));
        let route = routing(&s.say(2, "check the latest observation now")).0;
        let effects = s.finish(
            route,
            decision(vec![update(
                job,
                None,
                JobAction::Keep,
                vec![InputId(2)],
                required,
            )]),
        );
        assert!(effects.iter().any(|effect| matches!(effect,
            Effect::CancelWake { id } if *id == old_timer)));
        let (call, request) = worker(&effects, job);
        assert!(matches!(request.task, ModelTask::Work { messages, .. }
            if messages == vec![input(90, "monitor every seven seconds"), input(2, "check the latest observation now")]));
        let new_timer = wake(&s.work(call, wait));
        assert_ne!(old_timer, new_timer);
        let waiting = s.job(job);
        assert_eq!(waiting.state, JobState::Waiting(WaitReason::Timer));
        // A stale wake must not fire a later timer on the same goal/version.
        no_start(&s.kernel.step(Event::Wake { id: old_timer }));
        assert_eq!(s.job(job), waiting);
        worker(&s.kernel.step(Event::Wake { id: new_timer }), job);
        no_start(&s.kernel.step(Event::Wake { id: new_timer }));
    }
}

#[test]
fn ask_user_with_a_reply_is_rejected_atomically_even_behind_an_input_barrier() {
    for pending in [false, true] {
        for operation in [
            None,
            Some(ToolCall::new("lookup", json!({}))),
            Some(ToolCall::new("write", json!({}))),
        ] {
            let mut s = Scenario::new();
            let (job, call) = s.create(1, "prepare the original answer");
            let route = pending.then(|| routing(&s.say(2, "this may replace the answer")).0);
            let effects = s.work(
                call,
                WorkProposal {
                    reply: Some("obsolete final answer".into()),
                    operation,
                    next: Next::AskUser {
                        question: "is that okay?".into(),
                    },
                    ..Default::default()
                },
            );
            no_start(&effects);
            assert!(replies(&effects).is_empty());
            assert!(
                !notices(&effects)
                    .iter()
                    .any(|notice| matches!(notice, Notice::Clarification { .. }))
            );
            assert!(matches!(s.job(job).state, JobState::Failed { .. }));
            assert!(s.job(job).results.is_empty());
            if let Some(route) = route {
                assert_eq!(s.state(2), InputState::Routing);
                let effects = s.finish(route, decision(vec![]));
                no_start(&effects);
                assert!(replies(&effects).is_empty());
            }
        }
    }
}

#[test]
fn user_goal_changes_and_resume_require_their_original_input_ids() {
    for (goal, action) in [
        (Some("replacement goal"), JobAction::Keep),
        (None, JobAction::Resume),
    ] {
        for required in [false, true] {
            let mut s = Scenario::new();
            let (job, call) = s.create(1, "original goal");
            let route = routing(&s.say(2, "pause this work")).0;
            s.finish(
                route,
                decision(vec![update(job, None, JobAction::Pause, vec![], false)]),
            );
            s.finish(call, cancelled());
            let before = s.job(job);
            let route = routing(&s.say(3, "change or resume this work")).0;
            let effects = s.finish(
                route,
                decision(vec![update(job, goal, action, vec![], required)]),
            );
            assert_eq!(s.job(job), before);
            assert!(matches!(s.state(3), InputState::RoutingFailed { .. }));
            assert!(notices(&effects).iter().any(|notice| matches!(notice,
                Notice::InputRoutingFailed { inputs, message }
                    if inputs == &vec![InputId(3)] && message.contains("original input"))));
            no_start(&effects);
            assert!(replies(&effects).is_empty());
        }
    }
}

#[test]
fn keep_on_a_paused_job_attaches_new_words_but_never_implicitly_resumes() {
    for goal in [None, Some("revised goal")] {
        for required in [false, true] {
            let mut s = Scenario::new();
            let (job, call) = s.create(90, "original goal");
            let route = routing(&s.say(2, "pause this work")).0;
            s.finish(
                route,
                decision(vec![update(job, None, JobAction::Pause, vec![], false)]),
            );
            s.finish(call, cancelled());
            let route = routing(&s.say(3, "here are revised details; stay paused")).0;
            let effects = s.finish(
                route,
                decision(vec![update(
                    job,
                    goal,
                    JobAction::Keep,
                    vec![InputId(3)],
                    required,
                )]),
            );
            no_start(&effects);
            assert_eq!(s.job(job).state, JobState::Paused);
            assert_eq!(s.job(job).active_call, None);
            assert_eq!(s.job(job).goal, goal.unwrap_or("original goal"));
            assert_eq!(s.job(job).inputs, [InputId(90), InputId(3)]);
            assert_eq!(
                s.state(3),
                if required {
                    InputState::Handled
                } else {
                    InputState::Finished(InputOutcome::Completed)
                }
            );
            let route = routing(&s.say(4, "resume now with the revised details")).0;
            let effects = s.finish(
                route,
                decision(vec![update(
                    job,
                    None,
                    JobAction::Resume,
                    vec![InputId(4)],
                    true,
                )]),
            );
            let (call, request) = worker(&effects, job);
            assert!(matches!(request.task, ModelTask::Work { messages, .. }
                if messages == vec![input(90, "original goal"), input(3, "here are revised details; stay paused"), input(4, "resume now with the revised details")]));
            let effects = s.work(call, answer("resumed and handled the revised details"));
            assert_eq!(
                replies(&effects),
                ["resumed and handled the revised details"]
            );
            for id in [90, 3, 4] {
                assert_eq!(s.state(id), InputState::Finished(InputOutcome::Completed));
            }
        }
    }
}

#[test]
fn directed_correction_can_replace_failed_routing_during_drain_using_one_reserved_envelope() {
    let mut s = Scenario::with_config(KernelConfig {
        input_capacity: 1,
        ..Default::default()
    });
    let (job, call) = s.create(100, "original work");
    let route = routing(&s.say(90, "change the report")).0;
    assert!(replies(&s.work(call, answer("old held answer"))).is_empty());
    let effects = s.finish(route, CallOutcome::failed("could not interpret the change"));
    no_route(&effects);
    assert!(matches!(s.state(90), InputState::RoutingFailed { .. }));
    assert_eq!(
        s.kernel.admit(&input(2, "use this exact corrected scope")),
        Err(AdmissionError::Busy)
    );
    let correction = input(2, "use this exact corrected scope").replying_to(InputId(90));
    assert_eq!(s.kernel.admit(&correction), Ok(None));
    let (route, request) = routing(&s.kernel.step(Event::Input(correction.clone())));
    assert!(
        matches!(request.task, ModelTask::Kernel { inputs, source: None, .. }
        if inputs == vec![input(90, "change the report"), correction.clone()])
    );
    assert_eq!(
        s.kernel
            .snapshot()
            .inputs
            .iter()
            .filter(|input| input.state.unresolved())
            .count(),
        2
    );
    assert_eq!(
        s.kernel.admit(&correction),
        Ok(s.kernel.receipt(InputId(2)))
    );
    assert_eq!(
        s.kernel
            .admit(&input(3, "another correction").replying_to(InputId(90))),
        Err(AdmissionError::Busy)
    );
    assert_eq!(
        s.kernel
            .admit(&input(3, "unknown target").replying_to(InputId(999))),
        Err(AdmissionError::InvalidReply)
    );
    assert_eq!(s.kernel.receipt(InputId(3)), None);
    let effects = s.finish(
        route,
        decision(vec![update(
            job,
            Some("corrected scope"),
            JobAction::Keep,
            vec![InputId(90), InputId(2)],
            true,
        )]),
    );
    assert!(replies(&effects).is_empty());
    let (call, request) = worker(&effects, job);
    assert!(matches!(request.task, ModelTask::Work { messages, .. }
        if messages == vec![input(100, "original work"), input(90, "change the report"), correction]));
    assert!(s.kernel.validate_retry(InputId(90)).is_err());
    let effects = s.work(call, answer("corrected answer"));
    assert_eq!(replies(&effects), ["corrected answer"]);
    for id in [100, 90, 2] {
        assert_eq!(s.state(id), InputState::Finished(InputOutcome::Completed));
    }
    assert_eq!(
        s.kernel.admit(&input(3, "normal admission is open again")),
        Ok(None)
    );
}

#[test]
fn two_same_root_children_fill_both_background_slots_without_an_extra_event() {
    let mut s = Scenario::new();
    let (parent, call) = s.create(1, "parent work");
    let route = routing(&s.work(
        call,
        WorkProposal {
            next: Next::Coordinate {
                request: "create two children".into(),
            },
            ..Default::default()
        },
    ))
    .0;
    let effects = s.finish(
        route,
        decision(
            (0..2)
                .map(|index| {
                    JobChange::Create(JobSpec {
                        goal: format!("child {index}"),
                        inputs: vec![],
                        parent: Some(parent),
                        references: vec![],
                    })
                })
                .collect(),
        ),
    );
    let children: Vec<_> = s
        .kernel
        .snapshot()
        .jobs
        .into_iter()
        .filter(|job| job.parent == Some(parent))
        .collect();
    assert_eq!(children.len(), 2);
    assert_eq!(model_calls(&effects).len(), 2);
    for child in children {
        let call = worker(&effects, child.id).0;
        assert_eq!(child.active_call, Some(call));
        assert_eq!(child.state, JobState::Running);
        assert!(matches!(
            s.call(call).request,
            CallRequest::Work {
                interactive: false,
                ..
            }
        ));
    }
    assert_eq!(
        s.job(parent).active_call,
        None,
        "children precede their parent's continuation within the root FIFO"
    );
    assert_eq!(
        s.kernel
            .snapshot()
            .calls
            .iter()
            .filter(|call| call.is_running() && matches!(call.request, CallRequest::Work { .. }))
            .count(),
        2
    );
    no_route(&effects);
}

#[test]
fn root_round_robin_across_steps_gives_b_a_turn_before_as_remaining_children() {
    let mut s = Scenario::new();
    let (a, call) = s.create(1, "large root A");
    let route = routing(&s.work(
        call,
        WorkProposal {
            next: Next::Coordinate {
                request: "create eight children".into(),
            },
            ..Default::default()
        },
    ))
    .0;
    let effects = s.finish(
        route,
        decision(
            (0..8)
                .map(|index| {
                    JobChange::Create(JobSpec {
                        goal: format!("A child {index}"),
                        inputs: vec![],
                        parent: Some(a),
                        references: vec![],
                    })
                })
                .collect(),
        ),
    );
    let children: Vec<_> = s
        .kernel
        .snapshot()
        .jobs
        .into_iter()
        .filter(|job| job.parent == Some(a))
        .map(|job| job.id)
        .collect();
    let first = worker(&effects, children[0]).0;
    let second = worker(&effects, children[1]).0;
    assert_eq!(model_calls(&effects).len(), 2);
    let (b, first_b) = s.create(2, "independent root B");
    assert!(matches!(
        s.call(first_b).request,
        CallRequest::Work {
            interactive: true,
            ..
        }
    ));
    let continuation = WorkProposal {
        next: Next::Continue,
        ..Default::default()
    };
    no_start(&s.work(first_b, continuation.clone()));
    assert_eq!(s.job(b).state, JobState::Ready);
    for child in &children[2..] {
        assert_eq!(s.job(*child).state, JobState::Ready);
    }
    let effects = s.work(first, answer("first child complete"));
    let b_call = worker(&effects, b).0;
    assert_eq!(model_calls(&effects).len(), 1);
    assert!(matches!(
        s.call(b_call).request,
        CallRequest::Work {
            interactive: false,
            ..
        }
    ));
    assert_eq!(s.job(children[1]).active_call, Some(second));
    assert!(
        children[2..]
            .iter()
            .all(|child| s.job(*child).active_call.is_none())
    );
    no_route(&effects);
    // B yields: A's next child gets the next slot, preserving root-local FIFO.
    let effects = s.work(b_call, continuation);
    worker(&effects, children[2]);
    assert_eq!(model_calls(&effects).len(), 1);
    assert_eq!(s.job(b).active_call, None);
    // On another event, the remembered A turn returns the next free slot to B.
    let effects = s.work(second, answer("second child complete"));
    worker(&effects, b);
    assert_eq!(model_calls(&effects).len(), 1);
    assert!(
        children[3..]
            .iter()
            .all(|child| s.job(*child).active_call.is_none())
    );
    no_route(&effects);
}
