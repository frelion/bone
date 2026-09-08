//! Explore callback orders independently of the Tokio scheduler.
mod support;
use bone_agent::*;
use serde_json::json;
use support::*;

#[test]
fn target_change_stop_tool_result_and_old_worker_are_safe_in_every_order() {
    for order in permutations() {
        for old in [
            CallOutcome::work(WorkProposal {
                operation: Some(ToolCall::new("write", json!({}))),
                next: Next::Continue,
                ..Default::default()
            }),
            CallOutcome::work(answer("OLD ANSWER")),
            CallOutcome::failed("old failure"),
            CallOutcome {
                result: Err(CallError {
                    kind: CallErrorKind::TimedOut,
                    message: "old timeout".into(),
                }),
                external_effect: ExternalEffect::None,
            },
            cancelled(),
        ] {
            let mut s = Scenario::new();
            let (job, initial) = s.create(1, "Investigate A");
            let effects = s.work(
                initial,
                WorkProposal {
                    next: Next::Continue,
                    ..operation("lookup")
                },
            );
            let read = tool(&effects, "lookup");
            let work = worker(&effects, job).0;
            let route = routing(&s.say(2, "Stop using A; use B")).0;
            let events = [
                finished(work, old),
                finished(
                    route,
                    decision(vec![update(
                        job,
                        Some("B"),
                        JobAction::Keep,
                        vec![InputId(2)],
                        true,
                    )]),
                ),
                finished(read, CallOutcome::artifact("new material")),
                Event::Stop,
            ];
            let mut stopped = false;
            for index in order {
                stopped |= index == 3;
                s.kernel.step(events[index].clone());
                let snapshot = s.kernel.snapshot();
                assert!(!snapshot.calls.iter().any(|call| matches!(&call.request, CallRequest::Tool(tool) if tool.name == "write")), "order={order:?}");
                assert!(!snapshot.record.iter().any(|entry| matches!(&entry.kind, RecordKind::Notice(Notice::Reply { text, .. }) if text == "OLD ANSWER")), "order={order:?}");
                if stopped {
                    assert!(
                        snapshot.jobs.iter().all(|job| job.active_call.is_none()),
                        "late callback revived a stopped job: {order:?}"
                    );
                } else if s.job(job).goal == "B" {
                    assert!(
                        !matches!(s.job(job).state, JobState::Failed { .. } | JobState::Paused),
                        "old failure damaged replacement: {order:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn duplicate_real_results_never_republish_after_a_job_is_finished() {
    let mut s = Scenario::new();
    let (_, call) = s.create(1, "answer");
    s.work(call, answer("once"));
    let baseline = s.kernel.snapshot();
    for _ in 0..3 {
        let effects = s.work(call, answer("once"));
        no_start(&effects);
        assert!(replies(&effects).is_empty());
        assert_eq!(s.kernel.snapshot(), baseline);
    }
}

fn permutations() -> Vec<[usize; 4]> {
    let mut result = vec![];
    for a in 0..4 {
        for b in 0..4 {
            for c in 0..4 {
                for d in 0..4 {
                    let order = [a, b, c, d];
                    if (0..4)
                        .all(|value| order.iter().filter(|&&entry| entry == value).count() == 1)
                    {
                        result.push(order);
                    }
                }
            }
        }
    }
    result
}
