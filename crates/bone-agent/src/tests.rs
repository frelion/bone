use std::{collections::BTreeMap, time::Duration};

use serde_json::json;

use crate::{
    AdmissionError, AgentLimits, Assignment, Await, CallId, CheckpointDraft, CompactInput,
    Completion, DeliveryTarget, ExternalEffect, Input, InputId, InputOutcome, InputStatus,
    InquiryAnswer, InquiryResponse, JobAction, JobChange, JobSpec, JobStatus, KernelDecision,
    MonoTime, OutcomeKind, ReadQuery, RecordBody, ReportDraft, Seq, ToolCall, ToolEffect,
    ToolOutcome, ToolSpec, WorkInput, WorkProposal, WorkStep,
    context::{self, PreparedWork},
    kernel::Kernel,
    ports::{Call, Effect, Event},
};

const NOW: MonoTime = MonoTime(Duration::ZERO);

fn spec(goal: &str) -> JobSpec {
    JobSpec::new(goal, format!("{goal} scope"), format!("{goal} done"))
}

fn assignment(goal: &str, inputs: &[InputId]) -> Assignment {
    let mut assignment = Assignment::new(spec(goal));
    assignment.inputs = inputs.to_vec();
    assignment
}

fn kernel() -> Kernel {
    Kernel::new(AgentLimits::default(), Vec::new()).unwrap()
}

fn kernel_with(tool: ToolSpec) -> Kernel {
    Kernel::new(AgentLimits::default(), vec![tool]).unwrap()
}

fn starts(effects: &[Effect]) -> impl Iterator<Item = (CallId, &Call)> {
    effects.iter().filter_map(|effect| match effect {
        Effect::Start { id, call, .. } => Some((*id, call.as_ref())),
        Effect::Cancel(_) | Effect::Notify(_) => None,
    })
}

fn coordinate_call(effects: &[Effect]) -> CallId {
    starts(effects)
        .find_map(|(id, call)| matches!(call, Call::Coordinate(_)).then_some(id))
        .unwrap()
}

fn work_calls(effects: &[Effect]) -> Vec<(CallId, WorkInput)> {
    starts(effects)
        .filter_map(|(id, call)| match call {
            Call::Work(input) => Some((id, input.clone())),
            Call::Coordinate(_) | Call::Compact(_) | Call::Tool(_) => None,
        })
        .collect()
}

fn tool_call(effects: &[Effect]) -> CallId {
    starts(effects)
        .find_map(|(id, call)| matches!(call, Call::Tool(_)).then_some(id))
        .unwrap()
}

fn compact_call(effects: &[Effect]) -> Option<(CallId, CompactInput)> {
    starts(effects).find_map(|(id, call)| match call {
        Call::Compact(input) => Some((id, input.clone())),
        Call::Coordinate(_) | Call::Work(_) | Call::Tool(_) => None,
    })
}

fn create_roots(
    kernel: &mut Kernel,
    input: Input,
    assignments: Vec<Assignment>,
) -> Vec<(CallId, WorkInput)> {
    let (_, effects) = kernel.accept(NOW, input).unwrap();
    let effects = kernel.step(
        NOW,
        Event::CoordinateFinished {
            call: coordinate_call(&effects),
            result: Ok(KernelDecision::Apply {
                changes: assignments.into_iter().map(JobChange::Create).collect(),
                constraints: None,
            }),
        },
    );
    work_calls(&effects)
}

fn work(kernel: &mut Kernel, call: CallId, step: WorkStep) -> Vec<Effect> {
    kernel.step(
        NOW,
        Event::WorkFinished {
            call,
            result: Ok(WorkProposal::new(step)),
        },
    )
}

fn calls_by_goal(calls: Vec<(CallId, WorkInput)>) -> BTreeMap<String, (CallId, WorkInput)> {
    calls
        .into_iter()
        .map(|(call, input)| (input.spec.goal.clone(), (call, input)))
        .collect()
}

fn input_status(kernel: &Kernel, id: InputId) -> InputStatus {
    kernel
        .view()
        .inputs
        .into_iter()
        .find(|input| input.input.id == id)
        .unwrap()
        .status
}

fn finished_as(kernel: &Kernel, job: crate::JobId, kind: OutcomeKind) -> bool {
    matches!(kernel.job_status(job), JobStatus::Finished(outcome) if outcome.kind == kind)
}

#[test]
fn accepting_the_same_input_twice_is_idempotent() {
    let mut kernel = kernel();
    let input = Input::new(InputId(7), "ship it");

    let (first, _) = kernel.accept(NOW, input.clone()).unwrap();
    let sequence = kernel.view().sequence;
    let (second, repeated_effects) = kernel.accept(NOW, input.clone()).unwrap();

    assert_eq!(first, second);
    assert!(repeated_effects.is_empty());
    assert_eq!(kernel.view().sequence, sequence);
    assert!(matches!(
        kernel.accept(NOW, Input::new(input.id, "different")),
        Err(AdmissionError::ConflictingInput)
    ));
}

#[test]
fn delegate_validates_the_whole_batch_before_writing_any_part() {
    let mut kernel = kernel();
    let input = Input::new(InputId(1), "build both pieces");
    let mut calls = create_roots(
        &mut kernel,
        input.clone(),
        vec![assignment("parent", &[input.id])],
    );
    let (call, work_input) = calls.pop().unwrap();

    let invalid = assignment("invalid child", &[InputId(999)]);
    let proposal = WorkProposal {
        note: Some("must not be committed".into()),
        report: Some(ReportDraft {
            summary: "must not be committed either".into(),
            evidence: Vec::new(),
        }),
        answers: Vec::new(),
        step: WorkStep::Delegate(vec![assignment("valid child", &[input.id]), invalid]),
    };
    kernel.step(
        NOW,
        Event::WorkFinished {
            call,
            result: Ok(proposal),
        },
    );

    assert_eq!(kernel.jobs.len(), 1);
    assert!(finished_as(&kernel, work_input.job, OutcomeKind::Failed));
    assert!(!kernel.records.values().any(|record| matches!(
        &record.body,
        RecordBody::Note { text, .. } if text == "must not be committed"
    )));
    assert!(
        !kernel
            .records
            .values()
            .any(|record| matches!(record.body, RecordBody::Report { .. }))
    );
}

#[test]
fn each_worker_receives_only_its_own_record_stream() {
    let mut kernel = kernel();
    let input = Input::new(InputId(1), "two independent jobs");
    let calls = calls_by_goal(create_roots(
        &mut kernel,
        input.clone(),
        vec![
            assignment("alpha", &[input.id]),
            assignment("beta", &[input.id]),
        ],
    ));
    let (alpha_call, alpha) = &calls["alpha"];
    let beta = &calls["beta"].1;

    let marker = "alpha-private-note";
    let proposal = WorkProposal {
        note: Some(marker.into()),
        report: None,
        answers: Vec::new(),
        step: WorkStep::Wait(Await::After(Duration::from_secs(60))),
    };
    kernel.step(
        NOW,
        Event::WorkFinished {
            call: *alpha_call,
            result: Ok(proposal),
        },
    );
    let note = kernel
        .records
        .values()
        .find(|record| matches!(&record.body, RecordBody::Note { text, .. } if text == marker))
        .unwrap()
        .seq;

    assert!(kernel.jobs[&alpha.job].context.records.contains(&note));
    assert!(!kernel.jobs[&beta.job].context.records.contains(&note));
    let PreparedWork::Work { input, .. } = context::prepare_work(&kernel, beta.job).unwrap() else {
        panic!("beta context unexpectedly needed compaction");
    };
    assert!(!input.records.iter().any(|record| record.source == note));
}

#[test]
fn finish_waits_for_children_and_requires_their_delivery_to_be_read() {
    let mut kernel = kernel();
    let input = Input::new(InputId(1), "finish a tree");
    let mut calls = create_roots(
        &mut kernel,
        input.clone(),
        vec![assignment("parent", &[input.id])],
    );
    let (parent_call, parent_input) = calls.pop().unwrap();
    let parent = parent_input.job;

    let calls = calls_by_goal(work_calls(&work(
        &mut kernel,
        parent_call,
        WorkStep::Delegate(vec![assignment("child", &[])]),
    )));
    let (parent_call, _) = &calls["parent"];
    let (child_call, child_input) = &calls["child"];

    work(
        &mut kernel,
        *parent_call,
        WorkStep::Finish(crate::Completion::new("parent complete")),
    );
    assert!(matches!(
        kernel.job_status(parent),
        JobStatus::Waiting(crate::WaitView::Commit)
    ));

    let effects = work(
        &mut kernel,
        *child_call,
        WorkStep::Finish(crate::Completion::new("child complete")),
    );
    assert!(finished_as(
        &kernel,
        child_input.job,
        OutcomeKind::Completed
    ));
    let (parent_call, parent_context) = work_calls(&effects)
        .into_iter()
        .find(|(_, input)| input.job == parent)
        .unwrap();
    assert!(parent_context.records.iter().any(|record| {
        matches!(
            &kernel.records[&record.source].body,
            RecordBody::Delivery {
                to: DeliveryTarget::Job(job),
                ..
            } if *job == parent
        )
    }));

    work(
        &mut kernel,
        parent_call,
        WorkStep::Finish(crate::Completion::new("parent complete")),
    );
    assert!(finished_as(&kernel, parent, OutcomeKind::Completed));
    assert_eq!(
        input_status(&kernel, input.id),
        InputStatus::Finished(InputOutcome::Completed)
    );
}

#[test]
fn pause_discards_a_late_model_turn() {
    let mut kernel = kernel();
    let input = Input::new(InputId(1), "pause me");
    let mut calls = create_roots(
        &mut kernel,
        input.clone(),
        vec![assignment("job", &[input.id])],
    );
    let (call, work_input) = calls.pop().unwrap();
    let job = work_input.job;

    let effects = kernel.step(NOW, Event::Pause(job));
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Cancel(id) if *id == call))
    );
    let proposal = WorkProposal {
        note: Some("late-note".into()),
        report: None,
        answers: Vec::new(),
        step: WorkStep::Finish(crate::Completion::new("late finish")),
    };
    kernel.step(
        NOW,
        Event::WorkFinished {
            call,
            result: Ok(proposal),
        },
    );

    assert_eq!(kernel.job_status(job), JobStatus::Paused);
    assert!(!kernel.records.values().any(
        |record| matches!(&record.body, RecordBody::Note { text, .. } if text == "late-note")
    ));
    assert!(!kernel.records.values().any(
        |record| matches!(record.body, RecordBody::Outcome { job: owner, .. } if owner == job)
    ));

    let effects = kernel.step(NOW, Event::Resume(job));
    assert!(
        work_calls(&effects)
            .iter()
            .any(|(_, input)| input.job == job)
    );
}

#[test]
fn cancellation_records_late_tool_truth_without_reviving_the_job() {
    let tool = ToolSpec {
        name: "write".into(),
        description: "write once".into(),
        parameters: json!({ "type": "object" }),
        effect: ToolEffect::ExternalWrite,
    };
    let mut kernel = kernel_with(tool);
    let input = Input::new(InputId(1), "write something");
    let mut calls = create_roots(
        &mut kernel,
        input.clone(),
        vec![assignment("writer", &[input.id])],
    );
    let (work_call, work_input) = calls.pop().unwrap();
    let job = work_input.job;
    let effects = work(
        &mut kernel,
        work_call,
        WorkStep::Tool(ToolCall::new("write", json!({}))),
    );
    let tool_call = tool_call(&effects);

    let effects = kernel.step(NOW, Event::Cancel(job));
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Cancel(id) if *id == tool_call))
    );
    assert!(finished_as(&kernel, job, OutcomeKind::Cancelled));

    let truth = ToolOutcome {
        result: Ok(json!({ "written": true })),
        external_effect: ExternalEffect::Applied,
    };
    kernel.step(
        NOW,
        Event::ToolFinished {
            call: tool_call,
            result: truth.clone(),
        },
    );
    kernel.step(
        NOW,
        Event::ToolFinished {
            call: tool_call,
            result: truth,
        },
    );

    assert!(finished_as(&kernel, job, OutcomeKind::Cancelled));
    assert_eq!(
        kernel
            .records
            .values()
            .filter(|record| matches!(record.body, RecordBody::ToolFinished { call, .. } if call == tool_call))
            .count(),
        1
    );
}

#[test]
fn a_completed_child_can_seed_its_follow_up() {
    let mut kernel = kernel();
    let input = Input::new(InputId(1), "reuse completed research");
    let mut calls = create_roots(
        &mut kernel,
        input.clone(),
        vec![assignment("parent", &[input.id])],
    );
    let (parent_call, parent_input) = calls.pop().unwrap();
    let parent = parent_input.job;

    let calls = calls_by_goal(work_calls(&work(
        &mut kernel,
        parent_call,
        WorkStep::Delegate(vec![assignment("research", &[])]),
    )));
    let (stale_parent_call, _) = &calls["parent"];
    let (child_call, child_input) = &calls["research"];
    let child = child_input.job;
    work(
        &mut kernel,
        *child_call,
        WorkStep::Finish(crate::Completion::new("facts worth reusing")),
    );

    let effects = work(&mut kernel, *stale_parent_call, WorkStep::Continue);
    let (fresh_parent_call, _) = work_calls(&effects)
        .into_iter()
        .find(|(_, input)| input.job == parent)
        .unwrap();
    let mut follow_up = assignment("follow-up", &[]);
    follow_up.seed = Some(child);
    work(
        &mut kernel,
        fresh_parent_call,
        WorkStep::Delegate(vec![follow_up]),
    );

    let follow_up = *kernel.jobs.keys().max().unwrap();
    assert_ne!(follow_up, child);
    assert!(kernel.jobs[&follow_up].context.checkpoint.is_none());
    assert_eq!(kernel.jobs[&follow_up].context.read_through, Seq::ZERO);
    let memory = kernel.jobs[&follow_up]
        .context
        .records
        .iter()
        .find_map(|seq| match &kernel.records[seq].body {
            RecordBody::ImportedMemory {
                source_job,
                summary,
                ..
            } => Some((*source_job, summary.as_str())),
            _ => None,
        });
    assert_eq!(memory, Some((child, "facts worth reusing")));
}

#[test]
fn stop_terminalizes_the_forest_and_late_turns_cannot_restart_it() {
    let mut kernel = kernel();
    let input = Input::new(InputId(1), "stop a tree");
    let mut calls = create_roots(
        &mut kernel,
        input.clone(),
        vec![assignment("parent", &[input.id])],
    );
    let (parent_call, _) = calls.pop().unwrap();
    let running = work_calls(&work(
        &mut kernel,
        parent_call,
        WorkStep::Delegate(vec![assignment("child", &[])]),
    ));

    kernel.step(NOW, Event::Stop);
    assert!(
        kernel
            .jobs
            .keys()
            .all(|job| finished_as(&kernel, *job, OutcomeKind::Cancelled))
    );
    assert_eq!(
        input_status(&kernel, input.id),
        InputStatus::Finished(InputOutcome::Cancelled)
    );

    for (call, _) in running {
        let effects = work(
            &mut kernel,
            call,
            WorkStep::Finish(crate::Completion::new("too late")),
        );
        assert!(starts(&effects).next().is_none());
    }
    assert!(
        kernel
            .jobs
            .keys()
            .all(|job| finished_as(&kernel, *job, OutcomeKind::Cancelled))
    );
}

#[test]
fn clarification_reply_uses_the_reserved_input_envelope() {
    let limits = AgentLimits {
        pending_inputs: 1,
        ..AgentLimits::default()
    };
    let mut kernel = Kernel::new(limits, Vec::new()).unwrap();
    let input = Input::new(InputId(1), "ambiguous request");
    let (_, effects) = kernel.accept(NOW, input.clone()).unwrap();
    kernel.step(
        NOW,
        Event::CoordinateFinished {
            call: coordinate_call(&effects),
            result: Ok(KernelDecision::Clarify("which target?".into())),
        },
    );

    assert!(matches!(
        kernel.accept(NOW, Input::new(InputId(2), "unrelated")),
        Err(AdmissionError::Busy)
    ));
    let reply = Input::new(InputId(3), "the library target").replying_to(input.id);
    let (_, effects) = kernel.accept(NOW, reply).unwrap();

    assert!(starts(&effects).any(|(_, call)| matches!(call, Call::Coordinate(_))));
}

#[test]
fn a_job_question_delivers_its_answer_without_reopening_old_routings() {
    let mut kernel = kernel();
    let original = Input::new(InputId(1), "build the feature");
    let mut calls = create_roots(
        &mut kernel,
        original.clone(),
        vec![assignment("builder", &[original.id])],
    );
    let (call, work_input) = calls.pop().unwrap();
    let job = work_input.job;
    work(
        &mut kernel,
        call,
        WorkStep::AskUser("which public name?".into()),
    );

    assert_eq!(
        input_status(&kernel, original.id),
        InputStatus::WaitingForUser {
            question: "which public name?".into()
        }
    );
    assert!(
        kernel
            .routings
            .values()
            .all(|routing| routing.state == crate::kernel::RoutingState::Closed)
    );

    let answer = Input::new(InputId(2), "call it Agent").replying_to(original.id);
    let (_, effects) = kernel.accept(NOW, answer).unwrap();
    assert!(!starts(&effects).any(|(_, call)| matches!(call, Call::Coordinate(_))));
    let (_, resumed) = work_calls(&effects)
        .into_iter()
        .find(|(_, input)| input.job == job)
        .unwrap();
    assert!(resumed.records.iter().any(|view| {
        matches!(&kernel.records[&view.source].body, RecordBody::Input(input) if input.id == InputId(2))
    }));
    assert!(resumed.records.iter().any(|view| {
        matches!(&kernel.records[&view.source].body, RecordBody::Clarification { question, .. } if question == "which public name?")
    }));
    assert!(
        kernel
            .routings
            .values()
            .all(|routing| routing.state == crate::kernel::RoutingState::Closed)
    );
}

#[test]
fn pausing_a_job_invalidates_its_coordination_authority() {
    let mut kernel = kernel();
    let input = Input::new(InputId(1), "coordinate a change");
    let mut calls = create_roots(
        &mut kernel,
        input.clone(),
        vec![assignment("parent", &[input.id])],
    );
    let (work_call, parent) = calls.pop().unwrap();
    let effects = work(
        &mut kernel,
        work_call,
        WorkStep::Coordinate("re-plan my children".into()),
    );
    let coordination = coordinate_call(&effects);

    let effects = kernel.step(NOW, Event::Pause(parent.job));
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Cancel(call) if *call == coordination))
    );
    let effects = kernel.step(
        NOW,
        Event::CoordinateFinished {
            call: coordination,
            result: Ok(KernelDecision::Apply {
                changes: vec![JobChange::Create(assignment("stale child", &[]))],
                constraints: None,
            }),
        },
    );

    assert_eq!(kernel.jobs.len(), 1);
    assert_eq!(kernel.job_status(parent.job), JobStatus::Paused);
    assert!(starts(&effects).next().is_none());

    let effects = kernel.step(NOW, Event::Resume(parent.job));
    assert!(
        work_calls(&effects)
            .iter()
            .any(|(_, input)| input.job == parent.job)
    );
}

#[test]
fn invalidating_coordination_cancels_its_investigation_tree() {
    let mut kernel = kernel();
    let input = Input::new(InputId(1), "research before deciding");
    let mut calls = create_roots(
        &mut kernel,
        input.clone(),
        vec![assignment("parent", &[input.id])],
    );
    let (work_call, parent) = calls.pop().unwrap();
    let effects = work(
        &mut kernel,
        work_call,
        WorkStep::Coordinate("investigate the dependency".into()),
    );
    let coordination = coordinate_call(&effects);
    let effects = kernel.step(
        NOW,
        Event::CoordinateFinished {
            call: coordination,
            result: Ok(KernelDecision::Investigate(assignment(
                "investigation",
                &[],
            ))),
        },
    );
    let (investigation_call, investigation) = work_calls(&effects).pop().unwrap();

    kernel.step(NOW, Event::Pause(parent.job));
    assert!(finished_as(
        &kernel,
        investigation.job,
        OutcomeKind::Cancelled
    ));
    let effects = work(
        &mut kernel,
        investigation_call,
        WorkStep::Finish(Completion::new("late investigation")),
    );
    assert!(starts(&effects).next().is_none());
}

#[test]
fn stop_terminalizes_a_routing_investigation_before_late_tool_truth() {
    let tool = ToolSpec {
        name: "read".into(),
        description: "read evidence".into(),
        parameters: json!({ "type": "object" }),
        effect: ToolEffect::ReadOnly,
    };
    let mut kernel = kernel_with(tool);
    let input = Input::new(InputId(1), "investigate this input");
    let (_, effects) = kernel.accept(NOW, input.clone()).unwrap();
    let effects = kernel.step(
        NOW,
        Event::CoordinateFinished {
            call: coordinate_call(&effects),
            result: Ok(KernelDecision::Investigate(assignment(
                "investigation",
                &[input.id],
            ))),
        },
    );
    let (work_call, investigation) = work_calls(&effects).pop().unwrap();
    let effects = work(
        &mut kernel,
        work_call,
        WorkStep::Tool(ToolCall::new("read", json!({}))),
    );
    let tool_call = tool_call(&effects);

    kernel.step(NOW, Event::Stop);
    assert!(finished_as(
        &kernel,
        investigation.job,
        OutcomeKind::Cancelled
    ));
    let effects = kernel.step(
        NOW,
        Event::ToolFinished {
            call: tool_call,
            result: ToolOutcome::value(json!({ "late": true })),
        },
    );
    assert!(starts(&effects).next().is_none());
    assert_eq!(
        input_status(&kernel, input.id),
        InputStatus::Finished(InputOutcome::Cancelled)
    );
}

#[test]
fn inquiry_to_a_finished_job_returns_its_outcome_without_stale_delivery() {
    let mut kernel = kernel();
    let input = Input::new(InputId(1), "ask a completed child");
    let mut calls = create_roots(
        &mut kernel,
        input.clone(),
        vec![assignment("parent", &[input.id])],
    );
    let (parent_call, parent) = calls.pop().unwrap();
    let calls = calls_by_goal(work_calls(&work(
        &mut kernel,
        parent_call,
        WorkStep::Delegate(vec![assignment("child", &[])]),
    )));
    let (parent_call, _) = &calls["parent"];
    let (child_call, child) = &calls["child"];
    work(
        &mut kernel,
        *child_call,
        WorkStep::Finish(Completion::new("child answer")),
    );
    let outcome = match kernel.job_status(child.job) {
        JobStatus::Finished(outcome) => outcome.as_of,
        status => panic!("expected a finished child, got {status:?}"),
    };

    work(
        &mut kernel,
        *parent_call,
        WorkStep::Inquire {
            job: child.job,
            question: "anything else?".into(),
        },
    );

    let inquiry = kernel
        .records
        .values()
        .rev()
        .find(|record| {
            matches!(record.body, RecordBody::Inquiry { target, .. } if target == child.job)
        })
        .unwrap()
        .seq;
    assert!(kernel.inquiries.is_empty());
    assert!(kernel.records.values().any(|record| {
        matches!(
            record.body,
            RecordBody::InquirySettled {
                inquiry: settled,
                result: crate::InquiryResult::Finished(seq),
            } if settled == inquiry && seq == outcome
        )
    }));
    assert!(!kernel.records.values().any(|record| {
        matches!(
            record.body,
            RecordBody::Delivery {
                to: DeliveryTarget::Job(target),
                source,
                kind: crate::DeliveryKind::Inquiry,
            } if target == child.job && source == inquiry
        )
    }));
    assert!(matches!(kernel.job_status(parent.job), JobStatus::Running));
}

#[test]
fn compaction_replaces_only_an_already_read_prefix() {
    let limits = AgentLimits {
        context_bytes: 3_000,
        item_bytes: 512,
        ..AgentLimits::default()
    };
    let mut kernel = Kernel::new(limits, Vec::new()).unwrap();
    let input = Input::new(InputId(1), "accumulate a long local history");
    let (_, effects) = kernel.accept(NOW, input.clone()).unwrap();
    let mut effects = kernel.step(
        NOW,
        Event::CoordinateFinished {
            call: coordinate_call(&effects),
            result: Ok(KernelDecision::Apply {
                changes: vec![JobChange::Create(assignment("long job", &[input.id]))],
                constraints: None,
            }),
        },
    );

    let (compact, frozen) = loop {
        if let Some(compact) = compact_call(&effects) {
            break compact;
        }
        let (call, _) = work_calls(&effects)
            .pop()
            .expect("work or compaction starts");
        effects = kernel.step(
            NOW,
            Event::WorkFinished {
                call,
                result: Ok(WorkProposal {
                    note: Some("x".repeat(480)),
                    report: None,
                    answers: Vec::new(),
                    step: WorkStep::Continue,
                }),
            },
        );
    };
    let job = frozen.job;
    let newest = *kernel.jobs[&job].context.records.back().unwrap();
    assert!(
        frozen.through < newest,
        "unread suffix must remain outside the checkpoint"
    );

    let effects = kernel.step(
        NOW,
        Event::CompactFinished {
            call: compact,
            result: Ok(CheckpointDraft {
                summary: "earlier local work".into(),
                evidence: Vec::new(),
            }),
        },
    );
    let (_, resumed) = work_calls(&effects)
        .into_iter()
        .find(|(_, input)| input.job == job)
        .expect("job resumes after compaction");
    assert_eq!(resumed.checkpoint.as_ref().unwrap().through, frozen.through);
    assert!(
        resumed
            .records
            .iter()
            .all(|record| record.source > frozen.through)
    );
    assert!(resumed.records.iter().any(|record| record.source == newest));
}

#[test]
fn changing_constraints_discards_a_finish_frozen_under_the_old_constraints() {
    let mut kernel = kernel();
    let original = Input::new(InputId(1), "build under the original constraints");
    let (work_call, work_input) = create_roots(
        &mut kernel,
        original.clone(),
        vec![assignment("builder", &[original.id])],
    )
    .pop()
    .unwrap();
    let job = work_input.job;

    let update = Input::new(InputId(2), "all work must now stay read-only");
    let (_, routing_effects) = kernel.accept(NOW, update).unwrap();
    work(
        &mut kernel,
        work_call,
        WorkStep::Finish(Completion::new("finished under stale constraints")),
    );
    assert!(matches!(
        kernel.jobs[&job].state,
        crate::job::JobState::Waiting(crate::job::WaitState::Commit(_))
    ));

    let effects = kernel.step(
        NOW,
        Event::CoordinateFinished {
            call: coordinate_call(&routing_effects),
            result: Ok(KernelDecision::Apply {
                changes: Vec::new(),
                constraints: Some("read-only work only".into()),
            }),
        },
    );

    assert!(!matches!(kernel.job_status(job), JobStatus::Finished(_)));
    assert!(
        work_calls(&effects)
            .iter()
            .any(|(_, input)| { input.job == job && input.constraints == "read-only work only" })
    );
}

#[test]
fn changing_constraints_cancels_an_in_flight_model_authority() {
    let mut kernel = kernel();
    let original = Input::new(InputId(1), "start work");
    let (stale_call, stale_input) = create_roots(
        &mut kernel,
        original.clone(),
        vec![assignment("worker", &[original.id])],
    )
    .pop()
    .unwrap();

    let (_, routing_effects) = kernel
        .accept(NOW, Input::new(InputId(2), "new global constraint"))
        .unwrap();
    let effects = kernel.step(
        NOW,
        Event::CoordinateFinished {
            call: coordinate_call(&routing_effects),
            result: Ok(KernelDecision::Apply {
                changes: Vec::new(),
                constraints: Some("new constraint".into()),
            }),
        },
    );
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Cancel(call) if *call == stale_call))
    );

    work(
        &mut kernel,
        stale_call,
        WorkStep::Finish(Completion::new("late stale finish")),
    );
    assert!(!matches!(
        kernel.job_status(stale_input.job),
        JobStatus::Finished(_)
    ));
}

#[test]
fn one_coordination_decision_cannot_update_an_owner_and_its_descendant() {
    let mut kernel = kernel();
    let original = Input::new(InputId(1), "create a tree");
    let (parent_call, parent_input) = create_roots(
        &mut kernel,
        original.clone(),
        vec![assignment("parent", &[original.id])],
    )
    .pop()
    .unwrap();
    let calls = calls_by_goal(work_calls(&work(
        &mut kernel,
        parent_call,
        WorkStep::Delegate(vec![assignment("child", &[])]),
    )));
    let child = calls["child"].1.job;

    let update = Input::new(InputId(2), "change the tree");
    let (_, effects) = kernel.accept(NOW, update.clone()).unwrap();
    kernel.step(
        NOW,
        Event::CoordinateFinished {
            call: coordinate_call(&effects),
            result: Ok(KernelDecision::Apply {
                changes: vec![
                    JobChange::Update {
                        job: parent_input.job,
                        spec: None,
                        action: JobAction::Cancel,
                        inputs: Vec::new(),
                        required: false,
                    },
                    JobChange::Update {
                        job: child,
                        spec: Some(spec("changed child")),
                        action: JobAction::Keep,
                        inputs: vec![update.id],
                        required: false,
                    },
                ],
                constraints: None,
            }),
        },
    );

    assert_eq!(kernel.jobs[&parent_input.job].revision, 1);
    assert_eq!(kernel.jobs[&child].revision, 1);
    assert!(!kernel.jobs[&child].inputs.contains(&update.id));
    assert!(!matches!(
        kernel.job_status(parent_input.job),
        JobStatus::Finished(_)
    ));
    assert!(matches!(
        input_status(&kernel, update.id),
        InputStatus::RoutingFailed { .. }
    ));
    assert!(kernel.records.values().any(|record| {
        matches!(&record.body, RecordBody::InputRoutingFailed { inputs, .. } if inputs.contains(&update.id))
    }));

    let effects = kernel.step(NOW, Event::Retry(update.id));
    let retried = starts(&effects)
        .find_map(|(_, call)| match call {
            Call::Coordinate(input) => Some(input),
            _ => None,
        })
        .expect("retry starts coordination");
    assert!(retried.records.iter().any(|view| {
        matches!(
            kernel.records[&view.source].body,
            RecordBody::InputRoutingFailed { .. }
        )
    }));
}

#[test]
fn changing_a_dependency_contract_wakes_waiters_with_the_new_revision() {
    let mut kernel = kernel();
    let original = Input::new(InputId(1), "run parent and child");
    let (parent_call, parent_input) = create_roots(
        &mut kernel,
        original.clone(),
        vec![assignment("consumer", &[original.id])],
    )
    .pop()
    .unwrap();
    let calls = calls_by_goal(work_calls(&work(
        &mut kernel,
        parent_call,
        WorkStep::Delegate(vec![assignment("producer", &[])]),
    )));
    let producer = calls["producer"].1.job;
    let consumer = parent_input.job;
    work(
        &mut kernel,
        calls["consumer"].0,
        WorkStep::Wait(Await::Job(producer)),
    );

    let update = Input::new(InputId(2), "change the producer contract");
    let (_, effects) = kernel.accept(NOW, update.clone()).unwrap();
    let effects = kernel.step(
        NOW,
        Event::CoordinateFinished {
            call: coordinate_call(&effects),
            result: Ok(KernelDecision::Apply {
                changes: vec![JobChange::Update {
                    job: producer,
                    spec: Some(spec("new producer")),
                    action: JobAction::Keep,
                    inputs: vec![update.id],
                    required: false,
                }],
                constraints: None,
            }),
        },
    );

    let (_, resumed) = work_calls(&effects)
        .into_iter()
        .find(|(_, input)| input.job == consumer)
        .expect("dependency waiter resumes");
    assert!(resumed.records.iter().any(|view| {
        matches!(kernel.records[&view.source].body, RecordBody::JobChanged { job, revision: 2, .. } if job == producer)
    }));
    assert!(kernel.records.values().any(|record| {
        matches!(record.body, RecordBody::Delivery { to: DeliveryTarget::Job(job), kind: crate::DeliveryKind::DependencyChanged, .. } if job == consumer)
    }));
}

#[test]
fn an_open_input_routing_preserves_the_existing_user_question() {
    let mut kernel = kernel();
    let input = Input::new(InputId(1), "work needs clarification");
    let (target_call, target_input) = create_roots(
        &mut kernel,
        input.clone(),
        vec![assignment("target", &[input.id])],
    )
    .pop()
    .unwrap();
    let target = target_input.job;
    work(
        &mut kernel,
        target_call,
        WorkStep::AskUser("first question?".into()),
    );
    let (_, effects) = kernel
        .accept(NOW, Input::new(InputId(2), "an unrelated request"))
        .unwrap();
    let effects = kernel.step(
        NOW,
        Event::CoordinateFinished {
            call: coordinate_call(&effects),
            result: Ok(KernelDecision::Inquire {
                job: target,
                question: "can you answer internally?".into(),
            }),
        },
    );
    let (target_call, target_input) = work_calls(&effects)
        .into_iter()
        .find(|(_, work)| work.job == target)
        .expect("inquiry wakes the target");
    let inquiry = target_input.inquiries[0].id;
    kernel.step(
        NOW,
        Event::WorkFinished {
            call: target_call,
            result: Ok(WorkProposal {
                note: None,
                report: None,
                answers: vec![InquiryAnswer {
                    inquiry,
                    response: InquiryResponse::Answer(ReportDraft {
                        summary: "internal answer".into(),
                        evidence: Vec::new(),
                    }),
                }],
                step: WorkStep::AskUser("better question?".into()),
            }),
        },
    );

    assert_eq!(kernel.user_question, Some(target));
    assert_eq!(
        input_status(&kernel, input.id),
        InputStatus::WaitingForUser {
            question: "first question?".into()
        }
    );
    assert!(matches!(
        kernel.jobs[&target].state,
        crate::job::JobState::Waiting(crate::job::WaitState::User { .. })
    ));
}

#[test]
fn a_waiting_job_can_replace_its_own_user_question() {
    let mut kernel = kernel();
    let input = Input::new(InputId(1), "parent work needs clarification");
    let (parent_call, parent_input) = create_roots(
        &mut kernel,
        input.clone(),
        vec![assignment("parent", &[input.id])],
    )
    .pop()
    .unwrap();
    let calls = calls_by_goal(work_calls(&work(
        &mut kernel,
        parent_call,
        WorkStep::Delegate(vec![assignment("child", &[])]),
    )));
    work(
        &mut kernel,
        calls["parent"].0,
        WorkStep::AskUser("first question?".into()),
    );
    let effects = work(
        &mut kernel,
        calls["child"].0,
        WorkStep::Finish(Completion::new("child result")),
    );
    let parent_call = work_calls(&effects)
        .into_iter()
        .find(|(_, work)| work.job == parent_input.job)
        .expect("child outcome wakes the waiting parent")
        .0;
    work(
        &mut kernel,
        parent_call,
        WorkStep::AskUser("better question?".into()),
    );

    assert_eq!(
        input_status(&kernel, input.id),
        InputStatus::WaitingForUser {
            question: "better question?".into()
        }
    );
}

#[test]
fn changing_constraints_requests_cancellation_of_an_in_flight_tool() {
    let tool = ToolSpec {
        name: "write".into(),
        description: "write once".into(),
        parameters: json!({ "type": "object" }),
        effect: ToolEffect::ExternalWrite,
    };
    let mut kernel = kernel_with(tool);
    let input = Input::new(InputId(1), "start a write");
    let (work_call, _) = create_roots(
        &mut kernel,
        input.clone(),
        vec![assignment("writer", &[input.id])],
    )
    .pop()
    .unwrap();
    let effects = work(
        &mut kernel,
        work_call,
        WorkStep::Tool(ToolCall::new("write", json!({}))),
    );
    let tool_call = tool_call(&effects);

    let (_, effects) = kernel
        .accept(NOW, Input::new(InputId(2), "writes are now forbidden"))
        .unwrap();
    let effects = kernel.step(
        NOW,
        Event::CoordinateFinished {
            call: coordinate_call(&effects),
            result: Ok(KernelDecision::Apply {
                changes: Vec::new(),
                constraints: Some("do not write".into()),
            }),
        },
    );
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Cancel(call) if *call == tool_call))
    );

    kernel.step(
        NOW,
        Event::ToolFinished {
            call: tool_call,
            result: ToolOutcome {
                result: Ok(json!({ "written": true })),
                external_effect: ExternalEffect::Applied,
            },
        },
    );
    assert!(kernel.records.values().any(|record| {
        matches!(&record.body, RecordBody::ToolFinished { call, outcome, .. } if *call == tool_call && outcome.external_effect == ExternalEffect::Applied)
    }));
}

#[test]
fn inquiry_to_a_paused_then_cancelled_job_returns_its_outcome() {
    let mut kernel = kernel();
    let input = Input::new(InputId(1), "ask after cancellation");
    let (_, target_input) = create_roots(
        &mut kernel,
        input.clone(),
        vec![assignment("target", &[input.id])],
    )
    .pop()
    .unwrap();
    let target = target_input.job;
    kernel.step(NOW, Event::Pause(target));
    kernel.step(NOW, Event::Cancel(target));
    let (_, effects) = kernel
        .accept(NOW, Input::new(InputId(2), "ask the finished job"))
        .unwrap();
    kernel.step(
        NOW,
        Event::CoordinateFinished {
            call: coordinate_call(&effects),
            result: Ok(KernelDecision::Inquire {
                job: target,
                question: "what happened?".into(),
            }),
        },
    );

    assert!(kernel.records.values().any(|record| {
        matches!(
            record.body,
            RecordBody::InquirySettled {
                result: crate::InquiryResult::Finished(_),
                ..
            }
        )
    }));
}

#[test]
fn a_parent_waiting_for_its_child_receives_one_outcome_delivery() {
    let mut kernel = kernel();
    let input = Input::new(InputId(1), "wait for a child");
    let (parent_call, parent_input) = create_roots(
        &mut kernel,
        input.clone(),
        vec![assignment("parent", &[input.id])],
    )
    .pop()
    .unwrap();
    let calls = calls_by_goal(work_calls(&work(
        &mut kernel,
        parent_call,
        WorkStep::Delegate(vec![assignment("child", &[])]),
    )));
    let child = calls["child"].1.job;
    work(
        &mut kernel,
        calls["parent"].0,
        WorkStep::Wait(Await::Job(child)),
    );
    work(
        &mut kernel,
        calls["child"].0,
        WorkStep::Finish(Completion::new("child complete")),
    );
    let outcome = match kernel.job_status(child) {
        JobStatus::Finished(outcome) => outcome.as_of,
        _ => panic!("child must finish"),
    };

    assert_eq!(
        kernel
            .records
            .values()
            .filter(|record| {
                matches!(record.body, RecordBody::Delivery { to: DeliveryTarget::Job(job), source, kind: crate::DeliveryKind::Outcome } if job == parent_input.job && source == outcome)
            })
            .count(),
        1
    );
}

#[test]
fn coordinator_root_directory_is_bounded_and_uses_an_exclusive_cursor() {
    let mut kernel = kernel();
    let input = Input::new(InputId(1), "create many independent jobs");
    let assignments = (0..18)
        .map(|index| assignment(&format!("root {index}"), &[input.id]))
        .collect();
    create_roots(&mut kernel, input, assignments);

    let (_, effects) = kernel
        .accept(NOW, Input::new(InputId(2), "inspect the directory"))
        .unwrap();
    let (call, page) = starts(&effects)
        .find_map(|(call, item)| match item {
            Call::Coordinate(input) => Some((call, input.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(page.jobs.len(), crate::context::DIRECTORY_PAGE);
    let cursor = page.next_job.expect("more roots remain");
    assert_eq!(cursor, page.jobs.last().unwrap().id);
    let finished = kernel
        .jobs
        .keys()
        .copied()
        .find(|job| *job > cursor)
        .unwrap();
    kernel.step(NOW, Event::Cancel(finished));

    kernel.step(
        NOW,
        Event::CoordinateFinished {
            call,
            result: Ok(KernelDecision::Read(ReadQuery::Jobs {
                parent: None,
                after: Some(cursor),
            })),
        },
    );
    let remaining = kernel
        .records
        .values()
        .rev()
        .find_map(|record| match &record.body {
            RecordBody::ReadResult { jobs, next_job, .. } => Some((jobs, next_job)),
            _ => None,
        })
        .unwrap();
    assert_eq!(remaining.0.len(), 1);
    assert_eq!(*remaining.1, None);
    assert!(
        remaining
            .0
            .iter()
            .all(|job| job.id > cursor && job.id != finished)
    );
}
