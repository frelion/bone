//! Independent ordering checks: the same observations arrive in every order.
//! These assertions describe authority and user-visible behavior, not private state.

use bone_agent::*;
use serde_json::json;

#[test]
fn changed_input_prevents_old_work_from_acting_in_every_callback_order() {
    for order in permutations() {
        for old_result in old_results() {
            let mut kernel = Kernel::new(
                KernelConfig::default(),
                vec![
                    tool("read", ToolEffect::ReadOnly),
                    tool("write", ToolEffect::ExternalWrite),
                ],
            )
            .unwrap();
            let start = kernel.step(message(1, "Investigate A"));
            let first = model_id(&start, false);
            let running = kernel.step(finished(
                first,
                JobOutcome::work(WorkResult {
                    autonomy: Autonomy::Run,
                    operation: Some(Operation::Tool(ToolCall::new("read", json!({})))),
                    next: Next::Continue,
                    ..Default::default()
                }),
            ));
            let old_work = model_id(&running, false);
            let read = kernel
                .snapshot()
                .jobs
                .iter()
                .find_map(|job| matches!(job.request, JobRequest::Tool(_)).then_some(job.id))
                .unwrap();
            let review = model_id(&kernel.step(message(2, "Do not use A; reconsider B")), true);
            let events = [
                finished(old_work, old_result),
                finished(
                    review,
                    JobOutcome::review(InputReview {
                        disposition: InputDisposition::Reconsider,
                        reply: None,
                        note: "The original words need the solver".into(),
                    }),
                ),
                finished(read, JobOutcome::artifact("new facts")),
                Event::Stop,
            ];
            let mut stopped = false;
            for index in order {
                stopped |= index == 3;
                kernel.step(events[index].clone());
                let snapshot = kernel.snapshot();
                assert!(
                    !snapshot.jobs.iter().any(|job| matches!(&job.request,
                    JobRequest::Tool(call) if call.name == "write")),
                    "order={order:?}"
                );
                assert!(
                    !snapshot.record.iter().any(|entry| matches!(&entry.kind,
                    RecordKind::Notice(Notice::Reply { text, .. }) if text == "OLD ANSWER")),
                    "order={order:?}"
                );
                let redirected = snapshot.record.iter().any(|entry| {
                    matches!(
                        entry.kind,
                        RecordKind::InputReviewed {
                            disposition: InputDisposition::Reconsider,
                            ..
                        }
                    )
                });
                if redirected && !stopped {
                    assert!(
                        snapshot.autonomous,
                        "an old callback paused its replacement: {order:?}"
                    );
                }
                if stopped {
                    assert!(
                        !snapshot.autonomous,
                        "a callback resumed stopped work: {order:?}"
                    );
                }
            }
        }
    }
}

fn old_results() -> Vec<JobOutcome> {
    let mut results = vec![JobOutcome::work(WorkResult {
        reply: Some("OLD ANSWER".into()),
        operation: Some(Operation::Tool(ToolCall::new("write", json!({})))),
        next: Next::Continue,
        ..Default::default()
    })];
    for kind in [
        JobErrorKind::Failed,
        JobErrorKind::Cancelled,
        JobErrorKind::TimedOut,
    ] {
        results.push(JobOutcome {
            result: Err(JobError {
                kind,
                message: "old call ended".into(),
            }),
            external_effect: ExternalEffect::None,
        });
    }
    results
}

fn permutations() -> Vec<[usize; 4]> {
    let mut result = Vec::new();
    for a in 0..4 {
        for b in 0..4 {
            for c in 0..4 {
                for d in 0..4 {
                    let order = [a, b, c, d];
                    if (0..4).all(|value| order.iter().filter(|&&item| item == value).count() == 1)
                    {
                        result.push(order);
                    }
                }
            }
        }
    }
    result
}

fn tool(name: &str, effect: ToolEffect) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: name.into(),
        parameters: json!({"type":"object"}),
        effect,
    }
}

fn model_id(effects: &[Effect], review: bool) -> JobId {
    effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Start {
                id,
                call: Call::Model(input),
                ..
            } if matches!(input.task, ModelTask::ReviewInput { .. }) == review => Some(*id),
            _ => None,
        })
        .expect("the expected model request should start")
}

fn message(id: u64, text: &str) -> Event {
    Event::UserMessage {
        id: MessageId(id),
        text: text.into(),
    }
}

fn finished(id: JobId, outcome: JobOutcome) -> Event {
    Event::JobFinished { id, outcome }
}
