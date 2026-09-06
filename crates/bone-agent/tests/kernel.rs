//! Behavior specifications: each test chooses the event order; no Future runs.

use bone_agent::*;
use serde_json::json;
use std::time::Duration;

struct Scenario {
    kernel: Kernel,
    next_message: u64,
}

impl Scenario {
    fn new() -> Self {
        let tools = [
            ("lookup", ToolEffect::ReadOnly),
            ("write", ToolEffect::ExternalWrite),
        ]
        .into_iter()
        .map(|(name, effect)| ToolSpec {
            name: name.into(),
            description: name.into(),
            parameters: json!({"type": "object"}),
            effect,
        })
        .collect();
        Self {
            kernel: Kernel::new(
                KernelConfig {
                    soft_deadline: Duration::from_secs(10),
                    ..Default::default()
                },
                tools,
            )
            .unwrap(),
            next_message: 0,
        }
    }

    fn running() -> (Self, JobId) {
        let mut s = Self::new();
        let (first, _) = work(&s.say("Investigate A"));
        let (next, _) = work(&s.complete(
            first,
            WorkResult {
                requirement: Some("Investigate A".into()),
                autonomy: Autonomy::Run,
                next: Next::Continue,
                ..Default::default()
            },
        ));
        (s, next)
    }

    fn say(&mut self, text: &str) -> Vec<Effect> {
        self.next_message += 1;
        self.kernel.step(Event::UserMessage {
            id: MessageId(self.next_message),
            text: text.into(),
        })
    }
    fn complete(&mut self, id: JobId, result: WorkResult) -> Vec<Effect> {
        self.finish(id, JobOutcome::work(result))
    }
    fn reviewed(&mut self, id: JobId, disposition: InputDisposition) -> Vec<Effect> {
        self.finish(
            id,
            JobOutcome::review(InputReview {
                disposition,
                reply: None,
                note: format!("{disposition:?}"),
            }),
        )
    }
    fn finish(&mut self, id: JobId, outcome: JobOutcome) -> Vec<Effect> {
        self.kernel.step(Event::JobFinished { id, outcome })
    }
    fn progress(&mut self, id: JobId, message: &str) -> Vec<Effect> {
        self.kernel.step(Event::JobProgress {
            id,
            progress: JobProgress {
                message: message.into(),
                percent: None,
            },
        })
    }
}

fn tool(name: &str) -> WorkResult {
    WorkResult {
        operation: Some(Operation::Tool(ToolCall::new(name, json!({})))),
        ..Default::default()
    }
}
fn applied() -> JobOutcome {
    JobOutcome {
        external_effect: ExternalEffect::Applied,
        ..JobOutcome::artifact(json!({"receipt": "committed"}))
    }
}
fn cancelled() -> JobOutcome {
    JobOutcome {
        result: Err(JobError {
            kind: JobErrorKind::Cancelled,
            message: "local invocation cancelled".into(),
        }),
        external_effect: ExternalEffect::None,
    }
}
fn model(effects: &[Effect], reviewing: bool) -> (JobId, ModelInput) {
    let calls = effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Start {
                id,
                call: Call::Model(input),
                ..
            } if matches!(input.task, ModelTask::ReviewInput { .. }) == reviewing => {
                Some((*id, input.clone()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        calls.len(),
        1,
        "expected one {}: {effects:#?}",
        if reviewing { "ReviewInput" } else { "Work" }
    );
    calls.into_iter().next().unwrap()
}
fn work(effects: &[Effect]) -> (JobId, ModelInput) {
    model(effects, false)
}
fn review(effects: &[Effect]) -> (JobId, ModelInput) {
    model(effects, true)
}
fn batch(input: &ModelInput) -> Vec<MessageId> {
    match &input.task {
        ModelTask::Work { messages } | ModelTask::ReviewInput { messages } => {
            messages.iter().map(|message| message.id).collect()
        }
    }
}
fn started_tool(effects: &[Effect], name: &str) -> JobId {
    let calls = effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Start {
                id,
                call: Call::Tool(call),
                ..
            } if call.name == name => Some(*id),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 1, "expected one {name}: {effects:#?}");
    calls[0]
}
fn wake(effects: &[Effect]) -> WakeId {
    effects
        .iter()
        .find_map(|effect| match effect {
            Effect::WakeAfter { id, .. } => Some(*id),
            _ => None,
        })
        .expect("reminder")
}
fn replies(effects: &[Effect]) -> Vec<&str> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Publish(Notice::Reply { text, .. }) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}
fn assert_no_start(effects: &[Effect]) {
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::Start { .. })),
        "unexpected start: {effects:#?}"
    );
}
fn assert_no_tool(effects: &[Effect]) {
    assert!(
        !effects.iter().any(|effect| matches!(
            effect,
            Effect::Start {
                call: Call::Tool(_),
                ..
            }
        )),
        "unexpected tool: {effects:#?}"
    );
}
fn assert_cancelled(effects: &[Effect], job: JobId) {
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::RequestCancel { id } if *id == job)),
        "missing cancellation: {effects:#?}"
    );
}
fn assert_discarded(s: &Scenario, id: JobId) {
    assert!(
        s.kernel
            .snapshot()
            .record
            .iter()
            .any(|entry| matches!(entry.kind, RecordKind::PlanDiscarded { job, .. } if job == id))
    );
}
fn assert_error(effects: &[Effect]) {
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Publish(Notice::Error { .. })))
    );
}

#[test]
fn uninterrupted_work_goes_directly_through_tools_with_zero_reviews() {
    let mut s = Scenario::new();
    let (first, input) = work(&s.say("Read the evidence"));
    assert_eq!(batch(&input), [MessageId(1)]);
    let read = started_tool(
        &s.complete(
            first,
            WorkResult {
                autonomy: Autonomy::Run,
                requirement: Some("Read the evidence".into()),
                ..tool("lookup")
            },
        ),
        "lookup",
    );
    let (last, input) = work(&s.finish(read, JobOutcome::artifact(json!({"value": 7}))));
    assert!(batch(&input).is_empty());
    let effects = s.complete(
        last,
        WorkResult {
            reply: Some("The value is 7".into()),
            next: Next::Finish,
            ..Default::default()
        },
    );
    assert_eq!(replies(&effects), ["The value is 7"]);
    assert!(effects.iter().any(|effect| matches!(effect, Effect::Publish(Notice::Finished { cleanup }) if cleanup.is_empty())));
    assert!(
        s.kernel
            .snapshot()
            .jobs
            .iter()
            .all(|job| !matches!(job.request, JobRequest::ReviewInput { .. }))
    );
}

#[test]
fn three_continue_results_need_no_tools_and_finish_stops_autonomy() {
    let mut s = Scenario::new();
    let (mut id, _) = work(&s.say("Reason through the problem"));
    for index in 0..3 {
        let effects = s.complete(
            id,
            WorkResult {
                note: format!("Conclusion {index}"),
                autonomy: Autonomy::Run,
                next: Next::Continue,
                ..Default::default()
            },
        );
        assert_no_tool(&effects);
        id = work(&effects).0;
    }
    assert_no_start(&s.complete(
        id,
        WorkResult {
            reply: Some("Done".into()),
            next: Next::Finish,
            ..Default::default()
        },
    ));
    assert!(!s.kernel.snapshot().autonomous);
    assert_eq!(s.kernel.snapshot().jobs.len(), 4);
}

#[test]
fn status_questions_reply_without_restarting_work_or_changing_its_basis() {
    let (mut s, main) = Scenario::running();
    let basis = s.kernel.snapshot().revision;
    for _ in 0..3 {
        let (id, input) = review(&s.say("How far along?"));
        let effects = s.finish(
            id,
            JobOutcome::review(InputReview {
                disposition: InputDisposition::Keep,
                reply: Some("Still working".into()),
                note: "status only".into(),
            }),
        );
        assert_no_start(&effects);
        assert_eq!(replies(&effects), ["Still working"]);
        assert!(effects.iter().any(|effect| matches!(effect, Effect::Publish(Notice::Reply { as_of, .. }) if *as_of == input.snapshot.record_cursor)));
        assert_eq!(s.kernel.snapshot().work, Some(main));
        assert_eq!(s.kernel.snapshot().revision, basis);
    }
    started_tool(&s.complete(main, tool("lookup")), "lookup");
    assert_eq!(
        s.kernel
            .snapshot()
            .jobs
            .iter()
            .filter(|job| matches!(job.request, JobRequest::Work { .. }))
            .count(),
        2
    );
}

#[test]
fn fixed_batches_hold_the_entire_proposal_until_every_later_input_is_explained() {
    let mut s = Scenario::new();
    let (main, original) = work(&s.say("Work on A"));
    let (first, input) = review(&s.say("Progress?"));
    assert_eq!(batch(&input), [MessageId(2)]);
    assert_no_start(&s.say("Still there?"));
    assert_no_start(&s.say("Any progress?"));
    let effects = s.complete(
        main,
        WorkResult {
            reply: Some("A is ready".into()),
            requirement: Some("A".into()),
            autonomy: Autonomy::Run,
            ..tool("lookup")
        },
    );
    assert_no_start(&effects);
    assert!(replies(&effects).is_empty());
    let snapshot = s.kernel.snapshot();
    assert_eq!(snapshot.work, None);
    assert_eq!(snapshot.candidate, Some(main));
    assert_eq!(snapshot.requirement, None);
    assert!(!snapshot.autonomous);
    assert_eq!(batch(&original), [MessageId(1)]);
    assert!(snapshot.record.iter().any(|entry| matches!(&entry.kind, RecordKind::WorkHeld { job, messages } if *job == main && messages == &[MessageId(2), MessageId(3), MessageId(4)])));
    let (second, input) = review(&s.reviewed(first, InputDisposition::Keep));
    assert_eq!(batch(&input), [MessageId(3), MessageId(4)]);
    assert_eq!(s.kernel.snapshot().candidate, Some(main));
    let effects = s.reviewed(second, InputDisposition::Keep);
    started_tool(&effects, "lookup");
    assert_eq!(replies(&effects), ["A is ready"]);
    let snapshot = s.kernel.snapshot();
    assert_eq!(snapshot.requirement.as_deref(), Some("A"));
    assert!(snapshot.autonomous);
    let mut handled = snapshot
        .record
        .iter()
        .filter_map(|entry| match &entry.kind {
            RecordKind::InputsHandled { messages, .. } => Some(messages.clone()),
            _ => None,
        })
        .flatten()
        .collect::<Vec<_>>();
    handled.sort();
    assert_eq!(
        handled,
        [MessageId(1), MessageId(2), MessageId(3), MessageId(4)]
    );
}

#[test]
fn reconsider_transfers_original_batches_and_waits_for_local_cancellation() {
    let mut s = Scenario::new();
    let (old, _) = work(&s.say("Investigate A"));
    let (control, _) = review(&s.say("Do not use A; use B"));
    let effects = s.reviewed(control, InputDisposition::Reconsider);
    assert_cancelled(&effects, old);
    assert_no_start(&effects);
    assert_eq!(s.kernel.snapshot().work, Some(old));
    assert_no_start(&s.say("Include migration cost"));
    let (new, input) = work(&s.finish(old, cancelled()));
    assert_eq!(batch(&input), [MessageId(1), MessageId(2), MessageId(3)]);
    assert_ne!(old, new);
    assert!(input.snapshot.record.iter().any(|entry| matches!(
        entry.kind,
        RecordKind::InputReviewed {
            disposition: InputDisposition::Reconsider,
            ..
        }
    )));
    started_tool(
        &s.complete(
            new,
            WorkResult {
                requirement: Some("B including migration cost".into()),
                autonomy: Autonomy::Run,
                ..tool("lookup")
            },
        ),
        "lookup",
    );
    assert_eq!(
        s.kernel.snapshot().requirement.as_deref(),
        Some("B including migration cost")
    );
}

#[test]
fn a_pure_reasoning_answer_for_a_is_material_not_a_reply_after_reconsidering_b() {
    let mut s = Scenario::new();
    let (old, _) = work(&s.say("Explore direction A"));
    let (control, _) = review(&s.say("No A. Explore B."));
    let result = WorkResult {
        note: "Reusable A calculation".into(),
        reply: Some("Final answer A".into()),
        requirement: Some("A".into()),
        next: Next::Finish,
        ..Default::default()
    };
    assert!(replies(&s.complete(old, result.clone())).is_empty());
    let effects = s.reviewed(control, InputDisposition::Reconsider);
    assert!(replies(&effects).is_empty());
    let (current, input) = work(&effects);
    assert_eq!(batch(&input), [MessageId(1), MessageId(2)]);
    assert_eq!(input.snapshot.requirement, None);
    assert_discarded(&s, old);
    assert!(input.snapshot.jobs.iter().any(|job| {
        job.id == old
            && matches!(&job.state, JobState::Finished(outcome)
            if outcome.result == Ok(JobOutput::Work(result.clone())))
    }));
    assert_eq!(
        replies(&s.complete(
            current,
            WorkResult {
                reply: Some("Final answer B".into()),
                next: Next::Finish,
                ..Default::default()
            }
        )),
        ["Final answer B"]
    );
}

#[test]
fn revoked_success_and_timeout_only_release_the_old_slot_without_pausing_replacement() {
    for outcome in [
        JobOutcome::work(WorkResult {
            reply: Some("Obsolete".into()),
            autonomy: Autonomy::Run,
            ..tool("write")
        }),
        JobOutcome {
            result: Err(JobError {
                kind: JobErrorKind::TimedOut,
                message: "old timeout".into(),
            }),
            external_effect: ExternalEffect::None,
        },
    ] {
        let (mut s, old) = Scenario::running();
        let (control, _) = review(&s.say("Change the task"));
        s.reviewed(control, InputDisposition::Reconsider);
        let effects = s.finish(old, outcome);
        assert_no_tool(&effects);
        assert!(replies(&effects).is_empty());
        assert!(!effects.iter().any(|effect| matches!(
            effect,
            Effect::Publish(Notice::Paused | Notice::Error { .. })
        )));
        let (current, _) = work(&effects);
        started_tool(&s.complete(current, tool("lookup")), "lookup");
    }
}

#[test]
fn tool_completion_invalidates_old_work_even_when_the_write_gate_opens() {
    let (mut s, id) = Scenario::running();
    let effects = s.complete(
        id,
        WorkResult {
            next: Next::Continue,
            ..tool("write")
        },
    );
    let write = started_tool(&effects, "write");
    let (old, input) = work(&effects);
    assert_no_start(&s.finish(write, applied()));
    assert!(s.kernel.snapshot().revision > input.snapshot.revision);
    let effects = s.complete(old, tool("write"));
    assert_no_tool(&effects);
    assert_discarded(&s, old);
    let (new, input) = work(&effects);
    assert!(
        input
            .snapshot
            .jobs
            .iter()
            .any(|job| job.id == write && !job.is_unresolved())
    );
    assert_no_start(&s.complete(
        new,
        WorkResult {
            next: Next::Finish,
            ..Default::default()
        },
    ));
}

#[test]
fn stale_first_work_returns_its_batch_without_publishing_reply_or_requirement() {
    let (mut s, id) = Scenario::running();
    let read = started_tool(&s.complete(id, tool("lookup")), "lookup");
    s.kernel.step(Event::Stop);
    let (first, _) = work(&s.say("Resume as B"));
    assert_no_start(&s.finish(read, JobOutcome::artifact(json!("late fact"))));
    let effects = s.complete(
        first,
        WorkResult {
            requirement: Some("B".into()),
            reply: Some("B acknowledged".into()),
            autonomy: Autonomy::Run,
            ..tool("write")
        },
    );
    assert_no_tool(&effects);
    assert!(replies(&effects).is_empty());
    assert_eq!(
        s.kernel.snapshot().requirement.as_deref(),
        Some("Investigate A")
    );
    let (new, input) = work(&effects);
    assert_eq!(batch(&input), [MessageId(2)]);
    started_tool(
        &s.complete(
            new,
            WorkResult {
                requirement: Some("B".into()),
                autonomy: Autonomy::Run,
                ..tool("lookup")
            },
        ),
        "lookup",
    );
}

#[test]
fn several_tool_results_merge_into_one_followup_and_never_invoke_review() {
    let (mut s, id) = Scenario::running();
    let first = s.complete(
        id,
        WorkResult {
            next: Next::Continue,
            ..tool("lookup")
        },
    );
    let read1 = started_tool(&first, "lookup");
    let (id, _) = work(&first);
    let second = s.complete(
        id,
        WorkResult {
            next: Next::Continue,
            ..tool("lookup")
        },
    );
    let read2 = started_tool(&second, "lookup");
    let (old, _) = work(&second);
    assert_no_start(&s.finish(read1, JobOutcome::artifact(json!(1))));
    assert_no_start(&s.finish(read2, JobOutcome::artifact(json!(2))));
    let (current, _) = work(&s.complete(old, WorkResult::default()));
    assert_no_start(&s.complete(current, WorkResult::default()));
    assert!(
        s.kernel
            .snapshot()
            .jobs
            .iter()
            .all(|job| !matches!(job.request, JobRequest::ReviewInput { .. }))
    );
}

#[test]
fn tool_results_do_not_invalidate_the_fixed_review_batch() {
    let (mut s, id) = Scenario::running();
    let effects = s.complete(
        id,
        WorkResult {
            next: Next::Continue,
            ..tool("lookup")
        },
    );
    let read = started_tool(&effects, "lookup");
    let (old, _) = work(&effects);
    let (control, _) = review(&s.say("How far along?"));
    s.finish(read, JobOutcome::artifact(json!(7)));
    s.complete(old, tool("write"));
    let effects = s.reviewed(control, InputDisposition::Keep);
    assert_no_tool(&effects);
    let (_, input) = work(&effects);
    assert!(batch(&input).is_empty());
    assert_eq!(
        s.kernel
            .snapshot()
            .jobs
            .iter()
            .filter(|job| matches!(job.request, JobRequest::ReviewInput { .. }))
            .count(),
        1
    );
}

#[test]
fn ordinary_progress_neither_wakes_a_model_nor_invalidates_work() {
    let (mut s, id) = Scenario::running();
    let effects = s.complete(
        id,
        WorkResult {
            next: Next::Continue,
            ..tool("lookup")
        },
    );
    let read = started_tool(&effects, "lookup");
    let (current, input) = work(&effects);
    assert_no_start(&s.progress(read, "Still reading"));
    assert_eq!(s.kernel.snapshot().revision, input.snapshot.revision);
    assert!(s.kernel.record_cursor() > input.snapshot.record_cursor);
    started_tool(&s.complete(current, tool("write")), "write");
}

#[test]
fn a_hung_tool_reminder_goes_to_work_and_does_not_declare_the_tool_failed() {
    let (mut s, id) = Scenario::running();
    let effects = s.complete(id, tool("lookup"));
    let read = started_tool(&effects, "lookup");
    let reminder = wake(&effects);
    let (current, input) = work(&s.kernel.step(Event::Wake { id: reminder }));
    assert!(
        input
            .snapshot
            .jobs
            .iter()
            .any(|job| job.id == read && job.is_running())
    );
    assert!(input.snapshot.record.iter().any(
        |entry| matches!(entry.kind, RecordKind::Reminder { job: Some(id), .. } if id == read)
    ));
    assert_no_start(&s.kernel.step(Event::Wake { id: reminder }));
    assert_no_start(&s.complete(current, WorkResult::default()));
    let (_, input) = work(&s.say("Do something else"));
    assert_eq!(batch(&input), [MessageId(2)]);
}

#[test]
fn reminders_during_work_merge_and_survive_a_wait_result() {
    let (mut s, id) = Scenario::running();
    let first = s.complete(
        id,
        WorkResult {
            next: Next::Continue,
            ..tool("lookup")
        },
    );
    let reminder1 = wake(&first);
    let (id, _) = work(&first);
    let second = s.complete(
        id,
        WorkResult {
            next: Next::Continue,
            ..tool("lookup")
        },
    );
    let reminder2 = wake(&second);
    let (current, input) = work(&second);
    assert_no_start(&s.kernel.step(Event::Wake { id: reminder1 }));
    assert_no_start(&s.kernel.step(Event::Wake { id: reminder2 }));
    assert_eq!(s.kernel.snapshot().revision, input.snapshot.revision);
    let (next, _) = work(&s.complete(current, WorkResult::default()));
    assert_no_start(&s.complete(next, WorkResult::default()));
}

#[test]
fn model_requested_wait_reminders_are_consumed_once() {
    let (mut s, id) = Scenario::running();
    let effects = s.complete(
        id,
        WorkResult {
            next: Next::Wait {
                reconsider_after: Some(Duration::from_secs(5)),
            },
            ..Default::default()
        },
    );
    let reminder = wake(&effects);
    let (current, _) = work(&s.kernel.step(Event::Wake { id: reminder }));
    assert_no_start(&s.complete(
        current,
        WorkResult {
            next: Next::Finish,
            ..Default::default()
        },
    ));
    assert_no_start(&s.kernel.step(Event::Wake { id: reminder }));
}

#[test]
fn stop_revokes_both_roles_and_waits_for_their_local_completions() {
    let mut s = Scenario::new();
    let (old, _) = work(&s.say("Start A"));
    let (control, _) = review(&s.say("Progress?"));
    let generation = s.kernel.snapshot().generation;
    let stopped = s.kernel.step(Event::Stop);
    assert_cancelled(&stopped, old);
    assert_cancelled(&stopped, control);
    assert!(s.kernel.snapshot().generation > generation);
    assert_no_start(&s.say("Start B"));
    let basis = s.kernel.snapshot().revision;
    let late = s.complete(
        old,
        WorkResult {
            reply: Some("Old A".into()),
            requirement: Some("A".into()),
            autonomy: Autonomy::Run,
            ..tool("write")
        },
    );
    assert_no_start(&late);
    assert!(replies(&late).is_empty());
    assert_eq!(s.kernel.snapshot().revision, basis);
    let (current, input) = work(&s.reviewed(control, InputDisposition::Reconsider));
    assert_eq!(batch(&input), [MessageId(3)]);
    assert_eq!(input.snapshot.requirement, None);
    assert!(!input.snapshot.autonomous);
    started_tool(
        &s.complete(
            current,
            WorkResult {
                autonomy: Autonomy::Run,
                ..tool("lookup")
            },
        ),
        "lookup",
    );
}

#[test]
fn review_pause_retains_only_input_after_its_own_batch_boundary() {
    let mut s = Scenario::new();
    let (old, _) = work(&s.say("Investigate A"));
    let (control, _) = review(&s.say("Pause"));
    s.say("Continue as B");
    let effects = s.reviewed(control, InputDisposition::Pause);
    assert_cancelled(&effects, old);
    assert_no_start(&effects);
    assert!(!s.kernel.snapshot().autonomous);
    assert_eq!(
        s.kernel
            .snapshot()
            .pending_messages
            .iter()
            .map(|message| message.id)
            .collect::<Vec<_>>(),
        [MessageId(3)]
    );
    let (current, input) = work(&s.finish(old, cancelled()));
    assert_eq!(batch(&input), [MessageId(3)]);
    started_tool(
        &s.complete(
            current,
            WorkResult {
                requirement: Some("B".into()),
                autonomy: Autonomy::Run,
                ..tool("lookup")
            },
        ),
        "lookup",
    );
}

#[test]
fn review_pause_discards_a_held_candidate_but_keeps_the_next_batch() {
    let mut s = Scenario::new();
    let (old, _) = work(&s.say("A"));
    let (control, _) = review(&s.say("Pause"));
    s.complete(
        old,
        WorkResult {
            reply: Some("Old final A".into()),
            next: Next::Finish,
            ..Default::default()
        },
    );
    s.say("Now B");
    let effects = s.reviewed(control, InputDisposition::Pause);
    assert!(replies(&effects).is_empty());
    let (_, input) = work(&effects);
    assert_eq!(batch(&input), [MessageId(3)]);
    assert_discarded(&s, old);
}

#[test]
fn pause_boundaries_use_record_order_even_when_message_ids_are_not_monotonic() {
    let mut s = Scenario::new();
    let (old, _) = work(&s.kernel.step(Event::UserMessage {
        id: MessageId(100),
        text: "A".into(),
    }));
    let (control, _) = review(&s.kernel.step(Event::UserMessage {
        id: MessageId(10),
        text: "Pause".into(),
    }));
    s.kernel.step(Event::UserMessage {
        id: MessageId(5),
        text: "Resume as B".into(),
    });
    s.reviewed(control, InputDisposition::Pause);
    assert_eq!(batch(&work(&s.finish(old, cancelled())).1), [MessageId(5)]);
}

#[test]
fn old_success_failure_and_timers_cannot_resume_after_stop() {
    let (mut s, id) = Scenario::running();
    let effects = s.complete(
        id,
        WorkResult {
            next: Next::Continue,
            ..tool("write")
        },
    );
    let write = started_tool(&effects, "write");
    let reminder = wake(&effects);
    let (old, _) = work(&effects);
    s.kernel.step(Event::Stop);
    assert_no_start(&s.finish(old, JobOutcome::failed("old request timed out")));
    assert_no_start(&s.kernel.step(Event::Wake { id: reminder }));
    assert_no_start(&s.finish(write, applied()));
    assert!(!s.kernel.snapshot().autonomous);
    assert!(s.kernel.snapshot().pending_messages.is_empty());
}

#[test]
fn current_model_failure_retains_input_without_retrying_until_new_input() {
    let mut s = Scenario::new();
    let (old, _) = work(&s.say("Start"));
    let effects = s.finish(old, JobOutcome::failed("Model unavailable"));
    assert_error(&effects);
    assert_no_start(&effects);
    assert_no_start(&s.kernel.step(Event::Wake { id: WakeId(999) }));
    assert!(!s.kernel.snapshot().autonomous);
    let (current, input) = work(&s.say("Please retry"));
    assert_eq!(batch(&input), [MessageId(1), MessageId(2)]);
    started_tool(
        &s.complete(
            current,
            WorkResult {
                autonomy: Autonomy::Run,
                ..tool("lookup")
            },
        ),
        "lookup",
    );
}

#[test]
fn review_failure_revokes_held_work_and_keeps_both_batches_for_the_next_user_attempt() {
    let mut s = Scenario::new();
    let (old, _) = work(&s.say("Start A"));
    let (control, _) = review(&s.say("Change to B"));
    s.complete(
        old,
        WorkResult {
            reply: Some("A".into()),
            autonomy: Autonomy::Run,
            ..tool("write")
        },
    );
    let effects = s.finish(control, JobOutcome::failed("review timed out"));
    assert_error(&effects);
    assert_no_start(&effects);
    assert!(replies(&effects).is_empty());
    let (_, input) = work(&s.say("Try again"));
    assert_eq!(batch(&input), [MessageId(1), MessageId(2), MessageId(3)]);
}

#[test]
fn review_cannot_submit_work_with_tool_authority() {
    let mut s = Scenario::new();
    let (old, _) = work(&s.say("A"));
    let (control, _) = review(&s.say("B"));
    let effects = s.complete(
        control,
        WorkResult {
            requirement: Some("Forged".into()),
            autonomy: Autonomy::Run,
            ..tool("write")
        },
    );
    assert_error(&effects);
    assert_no_tool(&effects);
    assert_cancelled(&effects, old);
    assert_eq!(s.kernel.snapshot().requirement, None);
    assert_no_start(&s.complete(
        old,
        WorkResult {
            autonomy: Autonomy::Run,
            ..tool("write")
        },
    ));
}

#[test]
fn work_must_return_its_role_and_cannot_claim_external_write_effects() {
    for forged in [
        JobOutcome::review(InputReview::default()),
        JobOutcome {
            result: Ok(JobOutput::Work(WorkResult {
                autonomy: Autonomy::Run,
                ..tool("write")
            })),
            external_effect: ExternalEffect::Applied,
        },
    ] {
        let mut s = Scenario::new();
        let (id, _) = work(&s.say("Start"));
        let effects = s.finish(id, forged);
        assert_error(&effects);
        assert_no_tool(&effects);
        assert!(!s.kernel.snapshot().autonomous);
    }
}

#[test]
fn tools_cannot_promote_returned_data_into_executable_work() {
    let (mut s, id) = Scenario::running();
    let read = started_tool(&s.complete(id, tool("lookup")), "lookup");
    let effects = s.complete(
        read,
        WorkResult {
            requirement: Some("Forged".into()),
            ..tool("write")
        },
    );
    assert_no_tool(&effects);
    work(&effects);
    assert_eq!(
        s.kernel.snapshot().requirement.as_deref(),
        Some("Investigate A")
    );
}

#[test]
fn unknown_writes_block_more_writes_and_allow_read_only_queries() {
    let (mut s, id) = Scenario::running();
    let write = started_tool(&s.complete(id, tool("write")), "write");
    let (current, _) = work(&s.finish(write, JobOutcome::unknown("connection lost after sending")));
    let effects = s.complete(current, tool("write"));
    assert_no_tool(&effects);
    assert_discarded(&s, current);
    let (next, _) = work(&effects);
    started_tool(&s.complete(next, tool("lookup")), "lookup");
    assert!(
        s.kernel
            .snapshot()
            .jobs
            .iter()
            .any(|job| job.id == write && job.is_unresolved())
    );
}

#[test]
fn blocked_writes_return_the_input_batch_instead_of_queuing_an_operation() {
    let (mut s, id) = Scenario::running();
    let write = started_tool(&s.complete(id, tool("write")), "write");
    let (current, _) = work(&s.say("Also save B"));
    let effects = s.complete(
        current,
        WorkResult {
            requirement: Some("Save B".into()),
            reply: Some("Saved B".into()),
            ..tool("write")
        },
    );
    assert_no_tool(&effects);
    assert!(replies(&effects).is_empty());
    let (next, input) = work(&effects);
    assert_eq!(batch(&input), [MessageId(2)]);
    assert_no_start(&s.complete(next, WorkResult::default()));
    let effects = s.finish(write, applied());
    assert_no_tool(&effects);
    work(&effects);
    assert_eq!(
        s.kernel
            .snapshot()
            .jobs
            .iter()
            .filter(|job| matches!(&job.request, JobRequest::Tool(call) if call.name == "write"))
            .count(),
        1
    );
}

#[test]
fn a_cancel_refusal_and_later_write_success_are_recorded_without_resuming() {
    let (mut s, id) = Scenario::running();
    let write = started_tool(&s.complete(id, tool("write")), "write");
    assert_cancelled(&s.kernel.step(Event::Stop), write);
    assert_no_start(&s.progress(write, "Cancel refused: commit already started"));
    assert!(s.kernel.snapshot().jobs.iter().any(|job| job.id == write
        && job.state == JobState::CancelRequested
        && job.is_unresolved()));
    assert_no_start(&s.finish(write, applied()));
    assert!(s.kernel.snapshot().jobs.iter().any(|job| job.id == write && matches!(&job.state, JobState::Finished(outcome) if outcome.external_effect == ExternalEffect::Applied)));
}

#[test]
fn an_ended_unknown_write_cannot_silently_accept_a_cancellation_request() {
    let (mut s, id) = Scenario::running();
    let write = started_tool(&s.complete(id, tool("write")), "write");
    let (current, _) = work(&s.finish(write, JobOutcome::unknown("Reply lost")));
    let effects = s.complete(
        current,
        WorkResult {
            operation: Some(Operation::Cancel(write)),
            ..Default::default()
        },
    );
    assert_discarded(&s, current);
    assert_error(&effects);
    assert!(effects.iter().any(|effect| matches!(effect, Effect::Publish(Notice::Error { message }) if message.contains("query and confirm"))));
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::RequestCancel { id } if *id == write))
    );
    assert!(
        s.kernel
            .snapshot()
            .jobs
            .iter()
            .any(|job| job.id == write && job.is_unresolved())
    );
}

#[test]
fn cancellation_of_a_running_call_is_idempotent_and_does_not_confirm_a_write() {
    let (mut s, id) = Scenario::running();
    let effects = s.complete(
        id,
        WorkResult {
            next: Next::Continue,
            ..tool("write")
        },
    );
    let write = started_tool(&effects, "write");
    let (id, _) = work(&effects);
    let effects = s.complete(
        id,
        WorkResult {
            operation: Some(Operation::Cancel(write)),
            next: Next::Continue,
            ..Default::default()
        },
    );
    assert_cancelled(&effects, write);
    let (id, _) = work(&effects);
    let effects = s.complete(
        id,
        WorkResult {
            operation: Some(Operation::Cancel(write)),
            ..Default::default()
        },
    );
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::RequestCancel { id } if *id == write))
    );
    assert!(
        s.kernel
            .snapshot()
            .jobs
            .iter()
            .any(|job| job.id == write && job.is_unresolved())
    );
}

#[test]
fn reconciling_an_unknown_write_invalidates_old_work_and_known_duplicates_are_ignored() {
    let (mut s, id) = Scenario::running();
    let write = started_tool(&s.complete(id, tool("write")), "write");
    let (old, _) = work(&s.finish(write, JobOutcome::unknown("Reply lost")));
    assert_no_start(&s.finish(write, applied()));
    let effects = s.complete(old, tool("write"));
    assert_no_tool(&effects);
    let (current, _) = work(&effects);
    started_tool(&s.complete(current, tool("write")), "write");
    assert!(s.finish(write, applied()).is_empty());
}

#[test]
fn finish_cancels_abandoned_reads_and_reports_cleanup_without_faking_completion() {
    let (mut s, id) = Scenario::running();
    let effects = s.complete(
        id,
        WorkResult {
            next: Next::Continue,
            ..tool("lookup")
        },
    );
    let read = started_tool(&effects, "lookup");
    let reminder = wake(&effects);
    let (current, _) = work(&effects);
    let effects = s.complete(
        current,
        WorkResult {
            reply: Some("Enough evidence".into()),
            next: Next::Finish,
            ..Default::default()
        },
    );
    assert_cancelled(&effects, read);
    assert!(effects.iter().any(|effect| matches!(effect, Effect::Publish(Notice::Finished { cleanup }) if cleanup == &[read])));
    assert!(
        s.kernel
            .snapshot()
            .jobs
            .iter()
            .any(|job| job.id == read && job.state == JobState::CancelRequested)
    );
    assert_no_start(&s.kernel.step(Event::Wake { id: reminder }));
    let late = s.finish(read, JobOutcome::artifact(json!("unused late result")));
    assert_no_start(&late);
    assert!(
        !late
            .iter()
            .any(|effect| matches!(effect, Effect::Publish(Notice::Finished { .. })))
    );
}

#[test]
fn unresolved_writes_block_success_even_when_the_future_has_returned_unknown() {
    for unknown in [false, true] {
        let (mut s, id) = Scenario::running();
        let effects = s.complete(
            id,
            WorkResult {
                next: Next::Continue,
                ..tool("write")
            },
        );
        let write = started_tool(&effects, "write");
        let (mut current, _) = work(&effects);
        if unknown {
            s.finish(write, JobOutcome::unknown("unknown outcome"));
            current = work(&s.complete(current, WorkResult::default())).0;
        }
        let effects = s.complete(
            current,
            WorkResult {
                reply: Some("Success".into()),
                next: Next::Finish,
                ..Default::default()
            },
        );
        assert_error(&effects);
        assert!(replies(&effects).is_empty());
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::Publish(Notice::Finished { .. })))
        );
    }
}

#[test]
fn finish_cannot_start_a_tool_but_can_cancel_a_discarded_read() {
    let (mut s, id) = Scenario::running();
    let effects = s.complete(
        id,
        WorkResult {
            next: Next::Finish,
            ..tool("lookup")
        },
    );
    assert_error(&effects);
    assert_no_start(&effects);
    let (mut s, id) = Scenario::running();
    let effects = s.complete(
        id,
        WorkResult {
            next: Next::Continue,
            ..tool("lookup")
        },
    );
    let read = started_tool(&effects, "lookup");
    let (current, _) = work(&effects);
    let effects = s.complete(
        current,
        WorkResult {
            operation: Some(Operation::Cancel(read)),
            next: Next::Finish,
            ..Default::default()
        },
    );
    assert_cancelled(&effects, read);
    assert_eq!(
        effects
            .iter()
            .filter(|effect| matches!(effect, Effect::RequestCancel { id } if *id == read))
            .count(),
        1
    );
}

#[test]
fn duplicate_messages_completions_and_expired_timers_do_not_publish_or_execute_twice() {
    let mut s = Scenario::new();
    let (id, _) = work(&s.say("Read"));
    let cursor = s.kernel.record_cursor();
    assert!(
        s.kernel
            .step(Event::UserMessage {
                id: MessageId(1),
                text: "Read".into()
            })
            .is_empty()
    );
    assert_eq!(s.kernel.record_cursor(), cursor);
    let effects = s.complete(
        id,
        WorkResult {
            autonomy: Autonomy::Run,
            ..tool("lookup")
        },
    );
    let read = started_tool(&effects, "lookup");
    let reminder = wake(&effects);
    let outcome = JobOutcome::artifact(json!(7));
    let (current, _) = work(&s.finish(read, outcome.clone()));
    assert!(s.finish(read, outcome).is_empty());
    assert_no_start(&s.kernel.step(Event::Wake { id: reminder }));
    let result = WorkResult {
        next: Next::Finish,
        ..Default::default()
    };
    s.complete(current, result.clone());
    assert!(s.complete(current, result).is_empty());
    assert!(s.finish(JobId(999), cancelled()).is_empty());
}

#[test]
fn a_polite_reply_after_stop_does_not_resume_and_background_events_cannot_resume() {
    let (mut s, id) = Scenario::running();
    let effects = s.complete(id, tool("lookup"));
    let read = started_tool(&effects, "lookup");
    let reminder = wake(&effects);
    s.kernel.step(Event::Stop);
    let (current, _) = work(&s.say("Thanks"));
    assert_no_start(&s.complete(
        current,
        WorkResult {
            reply: Some("You are welcome".into()),
            ..Default::default()
        },
    ));
    assert!(!s.kernel.snapshot().autonomous);
    assert_no_start(&s.kernel.step(Event::Wake { id: reminder }));
    assert_no_start(&s.finish(read, cancelled()));
}

#[test]
fn invalid_work_control_does_not_partially_publish_or_retry() {
    for result in [
        WorkResult {
            reply: Some("not published".into()),
            next: Next::Continue,
            ..Default::default()
        },
        WorkResult {
            reply: Some("not published".into()),
            autonomy: Autonomy::Run,
            operation: Some(Operation::Tool(ToolCall::new("missing", json!({})))),
            ..Default::default()
        },
        WorkResult {
            reply: Some("not published".into()),
            next: Next::Wait {
                reconsider_after: Some(Duration::ZERO),
            },
            ..Default::default()
        },
    ] {
        let mut s = Scenario::new();
        let (current, _) = work(&s.say("Start"));
        let effects = s.complete(current, result);
        assert_error(&effects);
        assert_no_start(&effects);
        assert!(replies(&effects).is_empty());
    }
}

#[test]
fn a_question_is_published_before_the_pause_that_ends_a_cli_turn() {
    let mut s = Scenario::new();
    let (id, _) = work(&s.say("Read my file"));
    let effects = s.complete(
        id,
        WorkResult {
            reply: Some("Which file should I read?".into()),
            autonomy: Autonomy::Pause,
            ..Default::default()
        },
    );
    let relevant = effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Publish(Notice::Reply { .. }) => Some("reply"),
            Effect::Publish(Notice::Paused) => Some("paused"),
            Effect::Publish(Notice::Finished { .. }) => Some("finished"),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(relevant, ["reply", "paused"]);
    assert_no_start(&effects);
}

#[test]
fn finish_with_pause_autonomy_publishes_only_the_finish_terminal_notice() {
    let mut s = Scenario::new();
    let (id, _) = work(&s.say("Give a short answer"));
    let effects = s.complete(
        id,
        WorkResult {
            reply: Some("The answer".into()),
            autonomy: Autonomy::Pause,
            next: Next::Finish,
            ..Default::default()
        },
    );
    let relevant = effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Publish(Notice::Reply { .. }) => Some("reply"),
            Effect::Publish(Notice::Paused) => Some("paused"),
            Effect::Publish(Notice::Finished { .. }) => Some("finished"),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(relevant, ["reply", "finished"]);
    assert!(!s.kernel.snapshot().autonomous);
}
