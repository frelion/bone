use std::{collections::BTreeMap, time::Duration};

use serde_json::json;

use crate::{
    AdmissionError, AgentLimits, Assignment, Await, BackgroundEntry, BootstrapContext, CallId,
    CheckpointDraft, CompactInput, Completion, ControlOutcome, DeliveryTarget, ExternalEffect,
    Input, InputId, InputOutcome, InputStatus, InquiryAnswer, InquiryResponse, JobAction,
    JobChange, JobSpec, JobStatus, KernelDecision, MonoTime, OutcomeKind, ReadQuery, RecordBody,
    ReportDraft, Seq, ToolCall, ToolEffect, ToolOutcome, ToolSpec, WorkInput, WorkProposal,
    WorkStep,
    context::{self, PreparedWork},
    kernel::{Kernel, KernelControl},
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
fn bootstrap_history_is_read_only_context_for_coordination_and_work() {
    let background = BootstrapContext {
        entries: vec![BackgroundEntry::new(
            "previous outcome",
            "the parser was already migrated",
        )],
        omitted: true,
    };
    let mut kernel =
        Kernel::with_background(AgentLimits::default(), Vec::new(), background.clone()).unwrap();
    let input = Input::new(InputId(1), "continue the migration");
    let (_, effects) = kernel.accept(NOW, input.clone()).unwrap();
    let (call, coordinate) = starts(&effects)
        .find_map(|(call, request)| match request {
            Call::Coordinate(input) => Some((call, input.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(coordinate.background.as_ref(), &background);

    let effects = kernel.step(
        NOW,
        Event::CoordinateFinished {
            call,
            result: Ok(KernelDecision::Apply {
                changes: vec![JobChange::Create(assignment("continue", &[input.id]))],
                constraints: None,
            }),
        },
    );
    let (_, work) = work_calls(&effects).pop().unwrap();
    assert_eq!(work.background.as_ref(), &background);
}

#[test]
fn bootstrap_history_must_fit_the_model_context_budget() {
    let limits = AgentLimits {
        context_bytes: 64,
        item_bytes: 64,
        ..AgentLimits::default()
    };
    let background = BootstrapContext {
        entries: vec![BackgroundEntry::new("history", "x".repeat(128))],
        omitted: false,
    };
    assert!(matches!(
        Kernel::with_background(limits, Vec::new(), background),
        Err(crate::kernel::KernelError::BackgroundTooLarge)
    ));
}

#[test]
fn active_reconfigure_restarts_model_work_without_losing_the_job() {
    let mut kernel = kernel();
    let input = Input::new(InputId(2), "keep this job alive");
    let (call, work_input) = create_roots(
        &mut kernel,
        input.clone(),
        vec![assignment("same job", &[input.id])],
    )
    .pop()
    .unwrap();
    let job = work_input.job;
    let replacement_tool = ToolSpec {
        name: "replacement".into(),
        description: "new tool".into(),
        parameters: json!({ "type": "object" }),
        effect: ToolEffect::ReadOnly,
    };

    let effects = kernel
        .reconfigure(NOW, AgentLimits::default(), vec![replacement_tool.clone()])
        .unwrap();

    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Cancel(id) if *id == call))
    );
    let (replacement, restarted) = work_calls(&effects).pop().unwrap();
    assert_eq!(restarted.job, job);
    assert_eq!(restarted.tools, vec![replacement_tool]);
    assert_ne!(replacement, call);

    let _ = work(
        &mut kernel,
        call,
        WorkStep::Finish(Completion::new("late obsolete result")),
    );
    assert!(matches!(kernel.job_status(job), JobStatus::Running));
    assert_eq!(kernel.jobs[&job].active_call, Some(replacement));
}

#[test]
fn suspended_reconfigure_waits_for_an_explicit_scheduling_resume() {
    let mut kernel = kernel();
    let input = Input::new(InputId(20), "pause execution for configuration");
    let (call, work_input) = create_roots(
        &mut kernel,
        input.clone(),
        vec![assignment("same job", &[input.id])],
    )
    .pop()
    .unwrap();

    let effects = kernel.suspend(NOW);
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Cancel(id) if *id == call))
    );
    assert!(starts(&effects).next().is_none());

    let effects = work(
        &mut kernel,
        call,
        WorkStep::Finish(Completion::new("obsolete")),
    );
    assert!(starts(&effects).next().is_none());
    assert_eq!(kernel.job_status(work_input.job), JobStatus::Ready);

    let effects = kernel
        .reconfigure(NOW, AgentLimits::default(), Vec::new())
        .unwrap();
    assert!(starts(&effects).next().is_none());
    assert_eq!(kernel.job_status(work_input.job), JobStatus::Ready);

    let effects = kernel.resume_scheduling(NOW);
    let (_, restarted) = work_calls(&effects).pop().unwrap();
    assert_eq!(restarted.job, work_input.job);
}

#[test]
fn suspend_allows_a_running_tool_to_finish_without_starting_more_work() {
    let tool = ToolSpec {
        name: "read".into(),
        description: "read data".into(),
        parameters: json!({ "type": "object" }),
        effect: ToolEffect::ReadOnly,
    };
    let mut kernel = Kernel::new(AgentLimits::default(), vec![tool.clone()]).unwrap();
    let input = Input::new(InputId(21), "finish the in-flight read");
    let (work_call, work_input) = create_roots(
        &mut kernel,
        input.clone(),
        vec![assignment("reader", &[input.id])],
    )
    .pop()
    .unwrap();
    let tool_call = tool_call(&work(
        &mut kernel,
        work_call,
        WorkStep::Tool(ToolCall::new("read", json!({}))),
    ));

    let effects = kernel.suspend(NOW);
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::Cancel(id) if *id == tool_call))
    );
    let effects = kernel.step(
        NOW,
        Event::ToolFinished {
            call: tool_call,
            result: ToolOutcome::value(json!({ "value": 1 })),
        },
    );
    assert!(starts(&effects).next().is_none());
    assert_eq!(kernel.job_status(work_input.job), JobStatus::Ready);

    let effects = kernel
        .reconfigure(NOW, AgentLimits::default(), vec![tool])
        .unwrap();
    assert!(starts(&effects).next().is_none());

    let effects = kernel.resume_scheduling(NOW);
    let (_, restarted) = work_calls(&effects).pop().unwrap();
    assert_eq!(restarted.job, work_input.job);
}

#[test]
fn reconfigure_keeps_running_tools_and_discards_pending_model_decisions() {
    let old_tool = ToolSpec {
        name: "old".into(),
        description: "old tool".into(),
        parameters: json!({ "type": "object" }),
        effect: ToolEffect::ExternalWrite,
    };
    let mut limits = AgentLimits {
        tool_slots: 1,
        ..AgentLimits::default()
    };
    let mut kernel = Kernel::new(limits.clone(), vec![old_tool]).unwrap();
    let input = Input::new(InputId(3), "run both jobs");
    let mut calls = calls_by_goal(create_roots(
        &mut kernel,
        input.clone(),
        vec![
            assignment("first", &[input.id]),
            assignment("second", &[input.id]),
        ],
    ));
    let (first_call, first) = calls.remove("first").unwrap();
    let (second_call, second) = calls.remove("second").unwrap();
    let effects = work(
        &mut kernel,
        first_call,
        WorkStep::Tool(ToolCall::new("old", json!({}))),
    );
    let running_tool = tool_call(&effects);
    let _ = work(
        &mut kernel,
        second_call,
        WorkStep::Tool(ToolCall::new("old", json!({}))),
    );
    assert!(matches!(
        kernel.jobs[&second.job].state,
        crate::job::JobState::Waiting(crate::job::WaitState::Commit(_))
    ));

    limits.tool_slots = 2;
    let replacement_tool = ToolSpec {
        name: "new".into(),
        description: "new tool".into(),
        parameters: json!({ "type": "object" }),
        effect: ToolEffect::ReadOnly,
    };
    let effects = kernel
        .reconfigure(NOW, limits, vec![replacement_tool.clone()])
        .unwrap();

    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::Cancel(id) if *id == running_tool))
    );
    assert!(matches!(
        kernel.job_status(first.job),
        JobStatus::Waiting(crate::WaitView::Tool(id)) if id == running_tool
    ));
    let (_, restarted) = work_calls(&effects)
        .into_iter()
        .find(|(_, work)| work.job == second.job)
        .unwrap();
    assert_eq!(restarted.tools, vec![replacement_tool]);
}

#[test]
fn reconfigure_keeps_the_output_limit_of_a_running_tool() {
    let tool = ToolSpec {
        name: "read".into(),
        description: "read data".into(),
        parameters: json!({ "type": "object" }),
        effect: ToolEffect::ReadOnly,
    };
    let mut limits = AgentLimits {
        tool_output_bytes: 1_024,
        ..AgentLimits::default()
    };
    let mut kernel = Kernel::new(limits.clone(), vec![tool.clone()]).unwrap();
    let input = Input::new(InputId(4), "read a value");
    let (work_call, _) = create_roots(
        &mut kernel,
        input.clone(),
        vec![assignment("reader", &[input.id])],
    )
    .pop()
    .unwrap();
    let call = tool_call(&work(
        &mut kernel,
        work_call,
        WorkStep::Tool(ToolCall::new("read", json!({}))),
    ));
    limits.tool_output_bytes = 128;
    kernel.reconfigure(NOW, limits, vec![tool]).unwrap();
    let result = ToolOutcome::value(json!({ "value": "x".repeat(256) }));
    let encoded = serde_json::to_vec(&result).unwrap();
    assert!(encoded.len() > 128 && encoded.len() <= 1_024);

    kernel.step(
        NOW,
        Event::ToolFinished {
            call,
            result: result.clone(),
        },
    );

    let recorded = kernel
        .records
        .values()
        .find_map(|record| match &record.body {
            RecordBody::ToolFinished {
                call: finished,
                outcome,
                ..
            } if *finished == call => Some(outcome.as_ref()),
            _ => None,
        })
        .unwrap();
    assert_eq!(recorded, &result);
}

#[test]
fn reconfigure_keeps_the_output_limit_when_resolving_a_running_write() {
    let tool = ToolSpec {
        name: "write".into(),
        description: "write data".into(),
        parameters: json!({ "type": "object" }),
        effect: ToolEffect::ExternalWrite,
    };
    let mut limits = AgentLimits {
        tool_output_bytes: 1_024,
        ..AgentLimits::default()
    };
    let mut kernel = Kernel::new(limits.clone(), vec![tool.clone()]).unwrap();
    let input = Input::new(InputId(5), "write a value");
    let (work_call, _) = create_roots(
        &mut kernel,
        input.clone(),
        vec![assignment("writer", &[input.id])],
    )
    .pop()
    .unwrap();
    let call = tool_call(&work(
        &mut kernel,
        work_call,
        WorkStep::Tool(ToolCall::new("write", json!({}))),
    ));
    limits.tool_output_bytes = 128;
    kernel.reconfigure(NOW, limits, vec![tool]).unwrap();
    kernel.step(
        NOW,
        Event::ToolFinished {
            call,
            result: ToolOutcome {
                result: Err(crate::CallError::failed("unknown")),
                external_effect: ExternalEffect::Unknown,
            },
        },
    );
    let resolution = ToolOutcome {
        result: Ok(json!({ "value": "x".repeat(256) })),
        external_effect: ExternalEffect::Applied,
    };
    let encoded = serde_json::to_vec(&resolution).unwrap();
    assert!(encoded.len() > 128 && encoded.len() <= 1_024);

    let (outcome, _) = kernel.control(
        NOW,
        KernelControl::ResolveWrite {
            call,
            result: resolution.clone(),
        },
    );

    assert_eq!(outcome, ControlOutcome::Applied);
    let recorded = kernel
        .records
        .values()
        .filter_map(|record| match &record.body {
            RecordBody::ToolFinished {
                call: finished,
                outcome,
                ..
            } if *finished == call => Some(outcome.as_ref()),
            _ => None,
        })
        .next_back()
        .unwrap();
    assert_eq!(recorded, &resolution);
}

#[test]
fn reconfigure_trims_the_oldest_bootstrap_entries_to_the_new_budget() {
    let newest = BackgroundEntry::new("new", "n".repeat(128));
    let kept = BootstrapContext {
        entries: vec![newest.clone()],
        omitted: true,
    };
    let background = BootstrapContext {
        entries: vec![BackgroundEntry::new("old", "o".repeat(128)), newest.clone()],
        omitted: false,
    };
    let mut kernel =
        Kernel::with_background(AgentLimits::default(), Vec::new(), background).unwrap();
    let context_bytes = serde_json::to_vec(&kept).unwrap().len();
    let limits = AgentLimits {
        context_bytes,
        item_bytes: context_bytes,
        ..AgentLimits::default()
    };

    kernel.reconfigure(NOW, limits, Vec::new()).unwrap();

    assert_eq!(kernel.background.as_ref(), &kept);
}

#[test]
fn a_routing_investigation_can_finish_while_its_routing_waits() {
    let mut kernel = kernel();
    let input = Input::new(InputId(100), "locate the work that needs this correction");
    let (_, effects) = kernel.accept(NOW, input.clone()).unwrap();
    let effects = kernel.step(
        NOW,
        Event::CoordinateFinished {
            call: coordinate_call(&effects),
            result: Ok(KernelDecision::Investigate(assignment(
                "read-only routing investigation",
                &[input.id],
            ))),
        },
    );
    let (call, investigation) = work_calls(&effects).pop().unwrap();

    let effects = work(
        &mut kernel,
        call,
        WorkStep::Finish(Completion::new("investigation complete")),
    );

    assert!(finished_as(
        &kernel,
        investigation.job,
        OutcomeKind::Completed
    ));
    assert!(starts(&effects).any(|(_, call)| matches!(call, Call::Coordinate(_))));
}

#[test]
fn parent_finish_waits_for_a_cancelled_child_external_write() {
    let mut kernel = kernel_with(ToolSpec {
        name: "write".into(),
        description: "external write".into(),
        parameters: json!({ "type": "object" }),
        effect: ToolEffect::ExternalWrite,
    });
    let input = Input::new(InputId(101), "finish only after the child write settles");
    let (parent_call, parent) = create_roots(
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
    let (child_call, child) = calls["child"].clone();
    work(
        &mut kernel,
        calls["parent"].0,
        WorkStep::Wait(Await::Job(child.job)),
    );
    let write_call = tool_call(&work(
        &mut kernel,
        child_call,
        WorkStep::Tool(ToolCall::new("write", json!({}))),
    ));

    let (_, effects) = kernel.control(NOW, KernelControl::Cancel(child.job));
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Cancel(call) if *call == write_call))
    );
    let parent_call = work_calls(&effects)
        .into_iter()
        .find(|(_, work)| work.job == parent.job)
        .unwrap()
        .0;
    work(
        &mut kernel,
        parent_call,
        WorkStep::Finish(Completion::new("child was cancelled")),
    );
    assert!(!finished_as(&kernel, parent.job, OutcomeKind::Completed));

    kernel.step(
        NOW,
        Event::ToolFinished {
            call: write_call,
            result: ToolOutcome {
                result: Ok(json!({ "written": true })),
                external_effect: ExternalEffect::Applied,
            },
        },
    );
    assert!(finished_as(&kernel, parent.job, OutcomeKind::Completed));
}

#[test]
fn delegate_capacity_rejection_is_returned_to_the_parent_worker() {
    let mut kernel = Kernel::new(
        AgentLimits {
            active_jobs: 1,
            ..AgentLimits::default()
        },
        Vec::new(),
    )
    .unwrap();
    let input = Input::new(InputId(102), "work locally if delegation is full");
    let (call, parent) = create_roots(
        &mut kernel,
        input.clone(),
        vec![assignment("parent", &[input.id])],
    )
    .pop()
    .unwrap();

    let effects = work(
        &mut kernel,
        call,
        WorkStep::Delegate(vec![assignment("child", &[])]),
    );

    assert!(!finished_as(&kernel, parent.job, OutcomeKind::Failed));
    assert_eq!(kernel.jobs.len(), 1);
    let (retry, context) = work_calls(&effects).pop().unwrap();
    assert!(context.records.iter().any(|view| {
        matches!(
            &kernel.records[&view.source].body,
            RecordBody::Audit { message }
                if message == "delegation was not accepted: job capacity is full"
        )
    }));
    work(
        &mut kernel,
        retry,
        WorkStep::Finish(Completion::new("completed locally")),
    );
    assert!(finished_as(&kernel, parent.job, OutcomeKind::Completed));
}

#[test]
fn a_new_input_supersedes_an_in_flight_routing_and_discards_its_late_result() {
    let mut kernel = kernel();
    let old = Input::new(InputId(201), "use implementation A");
    let (_, old_effects) = kernel.accept(NOW, old.clone()).unwrap();
    let old_call = coordinate_call(&old_effects);

    let new = Input::new(InputId(202), "correction: use implementation B");
    let (_, superseding) = kernel.accept(NOW, new.clone()).unwrap();
    assert!(
        superseding
            .iter()
            .any(|effect| matches!(effect, Effect::Cancel(call) if *call == old_call))
    );
    assert!(!starts(&superseding).any(|(_, call)| matches!(call, Call::Coordinate(_))));

    let effects = kernel.step(
        NOW,
        Event::CoordinateFinished {
            call: old_call,
            result: Ok(KernelDecision::Apply {
                changes: vec![JobChange::Create(assignment("implementation A", &[old.id]))],
                constraints: None,
            }),
        },
    );
    assert!(kernel.jobs.is_empty());
    let (current_call, current) = starts(&effects)
        .find_map(|(call, effect)| match effect {
            Call::Coordinate(input) => Some((call, input)),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        current
            .inputs
            .iter()
            .map(|input| input.id)
            .collect::<Vec<_>>(),
        vec![old.id, new.id]
    );

    kernel.step(
        NOW,
        Event::CoordinateFinished {
            call: current_call,
            result: Ok(KernelDecision::Apply {
                changes: vec![JobChange::Create(assignment(
                    "implementation B",
                    &[old.id, new.id],
                ))],
                constraints: None,
            }),
        },
    );
    assert_eq!(
        kernel.jobs.values().next().unwrap().spec.goal,
        "implementation B"
    );
    let (_, retry) = kernel.control(NOW, KernelControl::Retry(old.id));
    assert!(!starts(&retry).any(|(_, call)| matches!(call, Call::Coordinate(_))));
}

#[test]
fn retrying_an_old_failed_input_uses_the_newest_combined_batch() {
    let mut kernel = kernel();
    let old = Input::new(InputId(203), "use implementation A");
    let (_, effects) = kernel.accept(NOW, old.clone()).unwrap();
    kernel.step(
        NOW,
        Event::CoordinateFinished {
            call: coordinate_call(&effects),
            result: Err(crate::CallError::failed("temporary failure")),
        },
    );
    assert!(matches!(
        input_status(&kernel, old.id),
        InputStatus::RoutingFailed { .. }
    ));

    let new = Input::new(InputId(204), "correction: use implementation B");
    let (_, effects) = kernel.accept(NOW, new.clone()).unwrap();
    let (call, combined) = starts(&effects)
        .find_map(|(call, effect)| match effect {
            Call::Coordinate(input) => Some((call, input)),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        combined
            .inputs
            .iter()
            .map(|input| input.id)
            .collect::<Vec<_>>(),
        vec![old.id, new.id]
    );
    assert!(combined.records.iter().any(|view| matches!(
        kernel.records[&view.source].body,
        RecordBody::InputRoutingFailed { .. }
    )));

    kernel.step(
        NOW,
        Event::CoordinateFinished {
            call,
            result: Ok(KernelDecision::Apply {
                changes: vec![JobChange::Create(assignment(
                    "implementation B",
                    &[old.id, new.id],
                ))],
                constraints: None,
            }),
        },
    );
    let (_, retry) = kernel.control(NOW, KernelControl::Retry(old.id));
    assert!(!starts(&retry).any(|(_, call)| matches!(call, Call::Coordinate(_))));
    assert_eq!(
        kernel.jobs.values().next().unwrap().spec.goal,
        "implementation B"
    );
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
fn oversized_tool_call_fails_without_persisting_or_starting_the_request() {
    let limits = AgentLimits {
        context_bytes: 4_096,
        item_bytes: 128,
        ..AgentLimits::default()
    };
    let tool = ToolSpec {
        name: "write".into(),
        description: "write once".into(),
        parameters: json!({ "type": "object" }),
        effect: ToolEffect::ExternalWrite,
    };
    let mut kernel = Kernel::new(limits, vec![tool]).unwrap();
    let input = Input::new(InputId(1), "write the payload");
    let (call, work_input) = create_roots(
        &mut kernel,
        input.clone(),
        vec![assignment("writer", &[input.id])],
    )
    .pop()
    .unwrap();
    let oversized = "x".repeat(512);

    let effects = work(
        &mut kernel,
        call,
        WorkStep::Tool(ToolCall::new(
            "write",
            json!({ "payload": oversized.clone() }),
        )),
    );

    assert!(finished_as(&kernel, work_input.job, OutcomeKind::Failed));
    assert!(
        starts(&effects).all(|(_, call)| !matches!(call, Call::Tool(_))),
        "an oversized request must never reach the tool port"
    );
    assert!(kernel.records.values().any(|record| matches!(
        &record.body,
        RecordBody::Audit { message } if message == "tool call exceeds item_bytes"
    )));
    assert!(!kernel.records.values().any(|record| matches!(
        record.body,
        RecordBody::CallStarted {
            kind: crate::CallKind::Tool,
            ..
        } | RecordBody::ToolFinished { .. }
    )));
    assert!(kernel.records.values().all(|record| {
        let encoded = serde_json::to_vec(record).unwrap();
        encoded.len() < 4_096
            && !encoded
                .windows(oversized.len())
                .any(|part| part == oversized.as_bytes())
    }));
}

#[test]
fn oversized_model_result_becomes_a_small_call_failure() {
    let limits = AgentLimits {
        context_bytes: 1_024,
        item_bytes: 128,
        ..AgentLimits::default()
    };
    let mut kernel = Kernel::new(limits, Vec::new()).unwrap();
    let input = Input::new(InputId(1), "do one thing");
    let (call, work_input) = create_roots(
        &mut kernel,
        input.clone(),
        vec![assignment("worker", &[input.id])],
    )
    .pop()
    .unwrap();
    let oversized = "z".repeat(2_048);

    kernel.step(
        NOW,
        Event::WorkFinished {
            call,
            result: Ok(WorkProposal {
                note: Some(oversized.clone()),
                report: None,
                answers: Vec::new(),
                step: WorkStep::Continue,
            }),
        },
    );

    assert!(finished_as(&kernel, work_input.job, OutcomeKind::Failed));
    assert!(kernel.records.values().any(|record| matches!(
        &record.body,
        RecordBody::CallFinished {
            error: Some(error),
            ..
        } if error.message == "model result exceeds context_bytes"
    )));
    assert!(kernel.records.values().all(|record| {
        !serde_json::to_vec(record)
            .unwrap()
            .windows(oversized.len())
            .any(|part| part == oversized.as_bytes())
    }));
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
fn automatic_context_keeps_required_input_records_whole() {
    let limits = AgentLimits {
        context_bytes: 8_000,
        item_bytes: 128,
        ..AgentLimits::default()
    };
    let mut kernel = Kernel::new(limits, Vec::new()).unwrap();
    let input = Input::new(InputId(1), "atomic-input-".repeat(20));
    let (work_call, work_input) = create_roots(
        &mut kernel,
        input.clone(),
        vec![assignment("reader", &[input.id])],
    )
    .pop()
    .unwrap();
    let source = kernel.inputs[&input.id].accepted_at;
    let view = work_input
        .records
        .iter()
        .find(|view| view.source == source)
        .expect("input source is expanded from its delivery");

    assert!(
        work_input
            .records
            .iter()
            .all(|view| view.next_offset.is_none())
    );
    assert!(view.content.len() > kernel.limits.item_bytes);
    assert_eq!(view.offset, 0);
    assert_eq!(view.next_offset, None);
    assert_eq!(
        view.content,
        serde_json::to_string(&RecordBody::Input(input)).unwrap()
    );

    let effects = work(
        &mut kernel,
        work_call,
        WorkStep::Read(ReadQuery::Record {
            id: source,
            offset: 0,
        }),
    );
    let (after_read_call, _) = work_calls(&effects)
        .into_iter()
        .find(|(_, input)| input.job == work_input.job)
        .expect("the record read wakes the job");
    work(&mut kernel, after_read_call, WorkStep::Continue);

    let compact = context::prepare_compact(&kernel, work_input.job).unwrap();
    let source_views = compact
        .records
        .iter()
        .filter(|view| view.source == source && view.offset == 0)
        .collect::<Vec<_>>();
    assert_eq!(source_views.len(), 1);
    assert_eq!(source_views[0].next_offset, None);
}

#[test]
fn record_paging_always_advances_across_multibyte_utf8() {
    let record = crate::Record {
        seq: Seq(1),
        origin: crate::Origin::Kernel,
        body: RecordBody::Audit {
            message: "🙂".into(),
        },
    };
    let content = serde_json::to_string(&record.body).unwrap();
    let offset = content.find('🙂').unwrap();

    let page = context::view(&record, offset, 1).unwrap();

    assert_eq!(page.content, "🙂");
    assert_eq!(page.next_offset, Some(offset + "🙂".len()));
}

#[test]
fn an_unread_required_input_that_exceeds_context_fails_before_work_starts() {
    let limits = AgentLimits {
        context_bytes: 2_000,
        item_bytes: 128,
        ..AgentLimits::default()
    };
    let mut kernel = Kernel::new(limits, Vec::new()).unwrap();
    let original = Input::new(InputId(1), "ask before continuing");
    let (work_call, work_input) = create_roots(
        &mut kernel,
        original.clone(),
        vec![assignment("reader", &[original.id])],
    )
    .pop()
    .unwrap();
    let job = work_input.job;
    work(
        &mut kernel,
        work_call,
        WorkStep::AskUser("provide the full material".into()),
    );
    let read_before_answer = kernel.jobs[&job].context.read_through;

    let oversized =
        Input::new(InputId(2), "oversized-required-input-".repeat(200)).replying_to(original.id);
    let (_, effects) = kernel.accept(NOW, oversized).unwrap();
    assert!(
        !work_calls(&effects)
            .iter()
            .any(|(_, input)| input.job == job)
    );
    let (compact_call, _) =
        compact_call(&effects).expect("only the already-read prefix is compacted");

    let effects = kernel.step(
        NOW,
        Event::CompactFinished {
            call: compact_call,
            result: Ok(CheckpointDraft {
                summary: "initial request read".into(),
                evidence: Vec::new(),
            }),
        },
    );

    assert!(!starts(&effects).any(|(_, call)| {
        matches!(call, Call::Work(input) if input.job == job)
            || matches!(call, Call::Compact(input) if input.job == job)
    }));
    let JobStatus::Finished(outcome) = kernel.job_status(job) else {
        panic!("oversized job must fail while preparing its context");
    };
    assert_eq!(outcome.kind, OutcomeKind::Failed);
    assert!(
        outcome
            .completion
            .summary
            .contains("required context exceeds the configured model input budget")
    );
    assert_eq!(kernel.jobs[&job].context.read_through, read_before_answer);
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

    let (outcome, effects) = kernel.control(NOW, KernelControl::Pause(job));
    assert_eq!(outcome, ControlOutcome::Applied);
    assert_eq!(
        kernel.control(NOW, KernelControl::Pause(job)).0,
        ControlOutcome::Unchanged
    );
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Cancel(id) if *id == call))
    );
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::Notify(record)
            if matches!(record.body, RecordBody::JobControlChanged { job: changed, paused: true } if changed == job)
    )));
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

    let (outcome, effects) = kernel.control(NOW, KernelControl::Resume(job));
    assert_eq!(outcome, ControlOutcome::Applied);
    assert_eq!(
        kernel.control(NOW, KernelControl::Resume(job)).0,
        ControlOutcome::Unchanged
    );
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::Notify(record)
            if matches!(record.body, RecordBody::JobControlChanged { job: changed, paused: false } if changed == job)
    )));
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

    let (_, effects) = kernel.control(NOW, KernelControl::Cancel(job));
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
fn oversized_tool_outcomes_become_small_errors_without_losing_effect_truth() {
    let oversized_error = ToolOutcome {
        result: Err(crate::CallError::failed("x".repeat(9 * 1024 * 1024))),
        external_effect: ExternalEffect::Applied,
    };
    let escaping_value = ToolOutcome {
        result: Ok(json!({ "value": "\0".repeat(512) })),
        external_effect: ExternalEffect::Applied,
    };

    for outcome in [oversized_error, escaping_value] {
        let limits = AgentLimits {
            context_bytes: 4_096,
            item_bytes: 256,
            tool_output_bytes: 128,
            ..AgentLimits::default()
        };
        let tool = ToolSpec {
            name: "write".into(),
            description: "write once".into(),
            parameters: json!({ "type": "object" }),
            effect: ToolEffect::ExternalWrite,
        };
        let mut kernel = Kernel::new(limits, vec![tool]).unwrap();
        let input = Input::new(InputId(1), "write something");
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
        let call = tool_call(&effects);

        kernel.step(
            NOW,
            Event::ToolFinished {
                call,
                result: outcome,
            },
        );

        let record = kernel
            .records
            .values()
            .find(|record| {
                matches!(record.body, RecordBody::ToolFinished { call: id, .. } if id == call)
            })
            .unwrap();
        let RecordBody::ToolFinished { outcome, .. } = &record.body else {
            unreachable!();
        };
        assert_eq!(outcome.external_effect, ExternalEffect::Applied);
        assert!(matches!(
            &outcome.result,
            Err(error) if error.message == "tool outcome exceeds tool_output_bytes"
        ));
        assert!(serde_json::to_vec(record).unwrap().len() < 1_024);
    }
}

#[test]
fn a_completed_child_seeds_only_its_explicit_paginated_evidence() {
    let limits = AgentLimits {
        context_bytes: 8_000,
        item_bytes: 96,
        ..AgentLimits::default()
    };
    let mut kernel = Kernel::new(limits, Vec::new()).unwrap();
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
    let evidence_text = "evidence-note-".repeat(6);
    let effects = kernel.step(
        NOW,
        Event::WorkFinished {
            call: *child_call,
            result: Ok(WorkProposal {
                note: Some(evidence_text.clone()),
                report: None,
                answers: Vec::new(),
                step: WorkStep::Continue,
            }),
        },
    );
    let evidence = kernel
        .records
        .values()
        .find(|record| {
            matches!(&record.body, RecordBody::Note { text, .. } if text == &evidence_text)
        })
        .unwrap()
        .seq;
    let child_call = work_calls(&effects)
        .into_iter()
        .find(|(_, input)| input.job == child)
        .expect("child continues after recording evidence")
        .0;
    let unlisted_text = "unlisted-note-".repeat(6);
    kernel.step(
        NOW,
        Event::WorkFinished {
            call: child_call,
            result: Ok(WorkProposal {
                note: Some(unlisted_text.clone()),
                report: None,
                answers: Vec::new(),
                step: WorkStep::Finish(Completion {
                    summary: "facts worth reusing".into(),
                    evidence: vec![evidence],
                    remaining: Vec::new(),
                }),
            }),
        },
    );
    let unlisted = kernel
        .records
        .values()
        .find(|record| {
            matches!(&record.body, RecordBody::Note { text, .. } if text == &unlisted_text)
        })
        .unwrap()
        .seq;
    let source_outcome = match kernel.job_status(child) {
        JobStatus::Finished(outcome) => outcome.as_of,
        _ => panic!("seed source must be finished"),
    };

    let effects = work(&mut kernel, *stale_parent_call, WorkStep::Continue);
    let (fresh_parent_call, _) = work_calls(&effects)
        .into_iter()
        .find(|(_, input)| input.job == parent)
        .unwrap();
    let mut follow_up = assignment("follow-up", &[]);
    follow_up.seed = Some(child);
    let effects = work(
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
                record_refs,
                ..
            } => Some((*source_job, summary.as_str(), record_refs.as_slice())),
            _ => None,
        });
    assert_eq!(
        memory,
        Some((
            child,
            "facts worth reusing",
            [evidence, source_outcome].as_slice()
        ))
    );

    let (follow_up_call, initial) = work_calls(&effects)
        .into_iter()
        .find(|(_, input)| input.job == follow_up)
        .expect("follow-up starts with imported memory");
    assert!(!initial.records.iter().any(|view| view.source == evidence));
    assert!(!initial.records.iter().any(|view| view.source == unlisted));
    let memory_view = initial
        .records
        .iter()
        .find(|view| {
            matches!(
                kernel.records[&view.source].body,
                RecordBody::ImportedMemory { .. }
            )
        })
        .unwrap();
    assert_eq!(memory_view.next_offset, None);
    assert!(memory_view.content.len() > kernel.limits.item_bytes);

    let effects = work(
        &mut kernel,
        follow_up_call,
        WorkStep::Read(ReadQuery::Record {
            id: evidence,
            offset: 0,
        }),
    );
    let (after_read_call, after_read) = work_calls(&effects)
        .into_iter()
        .find(|(_, input)| input.job == follow_up)
        .expect("record read immediately wakes the follow-up");
    let page = after_read
        .records
        .iter()
        .find(|view| view.source == evidence)
        .expect("read result expands the requested evidence page");
    let read_result = after_read
        .records
        .iter()
        .find(|view| {
            matches!(
                kernel.records[&view.source].body,
                RecordBody::ReadResult { .. }
            )
        })
        .expect("the read result itself is included");
    assert_eq!(read_result.next_offset, None);
    assert!(read_result.content.len() > kernel.limits.item_bytes);
    assert_eq!(page.offset, 0);
    assert_eq!(page.content.len(), kernel.limits.item_bytes);
    let next_offset = page.next_offset.expect("the evidence has another page");

    let effects = work(
        &mut kernel,
        after_read_call,
        WorkStep::Read(ReadQuery::Record {
            id: evidence,
            offset: next_offset,
        }),
    );
    let (after_second_read_call, after_second_read) = work_calls(&effects)
        .into_iter()
        .find(|(_, input)| input.job == follow_up)
        .expect("the next evidence page wakes the follow-up");
    let second_page = after_second_read
        .records
        .iter()
        .find(|view| view.source == evidence && view.offset == next_offset)
        .expect("the requested offset selects the next evidence page");
    assert!(!second_page.content.is_empty());

    work(
        &mut kernel,
        after_second_read_call,
        WorkStep::Read(ReadQuery::Record {
            id: unlisted,
            offset: 0,
        }),
    );
    assert!(finished_as(&kernel, follow_up, OutcomeKind::Failed));
    assert!(!kernel.records.values().any(|record| {
        matches!(
            record.body,
            RecordBody::ReadResult {
                query: ReadQuery::Record { id, .. },
                ..
            } if id == unlisted
        )
    }));
    assert!(kernel.records.values().any(|record| {
        matches!(
            &record.body,
            RecordBody::Audit { message } if message == "worker cannot read that record"
        )
    }));
}

#[test]
fn a_parent_can_read_only_evidence_explicitly_shared_by_a_child_outcome() {
    let mut kernel = kernel();
    let input = Input::new(InputId(11), "verify delegated evidence");
    let (parent_call, parent) = create_roots(
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

    let effects = kernel.step(
        NOW,
        Event::WorkFinished {
            call: calls["child"].0,
            result: Ok(WorkProposal {
                note: Some("shared verification output".into()),
                report: None,
                answers: Vec::new(),
                step: WorkStep::Continue,
            }),
        },
    );
    let shared = kernel
        .records
        .values()
        .find(|record| {
            matches!(&record.body, RecordBody::Note { text, .. } if text == "shared verification output")
        })
        .unwrap()
        .seq;
    let child_call = work_calls(&effects)
        .into_iter()
        .find(|(_, work)| work.job == child)
        .unwrap()
        .0;
    let effects = kernel.step(
        NOW,
        Event::WorkFinished {
            call: child_call,
            result: Ok(WorkProposal {
                note: Some("private scratch note".into()),
                report: None,
                answers: Vec::new(),
                step: WorkStep::Finish(Completion {
                    summary: "verified".into(),
                    evidence: vec![shared],
                    remaining: Vec::new(),
                }),
            }),
        },
    );
    let private = kernel
        .records
        .values()
        .find(|record| {
            matches!(&record.body, RecordBody::Note { text, .. } if text == "private scratch note")
        })
        .unwrap()
        .seq;
    let parent_call = work_calls(&effects)
        .into_iter()
        .find(|(_, work)| work.job == parent.job)
        .unwrap()
        .0;

    let effects = work(
        &mut kernel,
        parent_call,
        WorkStep::Read(ReadQuery::Record {
            id: shared,
            offset: 0,
        }),
    );
    let (parent_call, context) = work_calls(&effects)
        .into_iter()
        .find(|(_, work)| work.job == parent.job)
        .expect("explicit completion evidence is readable by the parent");
    assert!(context.records.iter().any(|view| view.source == shared));
    assert!(!finished_as(&kernel, parent.job, OutcomeKind::Failed));

    work(
        &mut kernel,
        parent_call,
        WorkStep::Read(ReadQuery::Record {
            id: private,
            offset: 0,
        }),
    );
    assert!(finished_as(&kernel, parent.job, OutcomeKind::Failed));
}

#[test]
fn seed_uses_the_final_outcome_after_an_older_checkpoint() {
    let mut kernel = Kernel::new(
        AgentLimits {
            context_bytes: 3_000,
            item_bytes: 512,
            ..AgentLimits::default()
        },
        Vec::new(),
    )
    .unwrap();
    let input = Input::new(InputId(12), "correct an earlier research hypothesis");
    let (mut call, original) = create_roots(
        &mut kernel,
        input.clone(),
        vec![assignment("investigator", &[input.id])],
    )
    .pop()
    .unwrap();
    let (compact, _) = loop {
        let effects = kernel.step(
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
        if let Some(compact) = compact_call(&effects) {
            break compact;
        }
        call = work_calls(&effects).pop().unwrap().0;
    };
    let effects = kernel.step(
        NOW,
        Event::CompactFinished {
            call: compact,
            result: Ok(CheckpointDraft {
                summary: "old hypothesis: endpoint A".into(),
                evidence: Vec::new(),
            }),
        },
    );
    let call = work_calls(&effects).pop().unwrap().0;
    work(
        &mut kernel,
        call,
        WorkStep::Finish(Completion::new(
            "final verified conclusion: endpoint B; A was disproven",
        )),
    );
    let source_outcome = match kernel.job_status(original.job) {
        JobStatus::Finished(outcome) => outcome.as_of,
        _ => panic!("source should be complete"),
    };

    let follow_up_input = Input::new(InputId(13), "follow up on the verified conclusion");
    let mut follow_up = assignment("follow-up", &[follow_up_input.id]);
    follow_up.seed = Some(original.job);
    let (call, context) = create_roots(&mut kernel, follow_up_input, vec![follow_up])
        .pop()
        .unwrap();
    let memory =
        context.records.iter().find_map(|view| {
            match serde_json::from_str::<RecordBody>(&view.content).ok()? {
                RecordBody::ImportedMemory {
                    summary,
                    record_refs,
                    ..
                } => Some((summary, record_refs)),
                _ => None,
            }
        });
    assert_eq!(
        memory,
        Some((
            "final verified conclusion: endpoint B; A was disproven".into(),
            vec![source_outcome]
        ))
    );

    let effects = work(
        &mut kernel,
        call,
        WorkStep::Read(ReadQuery::Record {
            id: source_outcome,
            offset: 0,
        }),
    );
    let (_, context) = work_calls(&effects).pop().unwrap();
    assert!(context.records.iter().any(|view| {
        view.source == source_outcome && view.content.contains("final verified conclusion")
    }));
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

    assert_eq!(
        kernel.control(NOW, KernelControl::Stop).0,
        ControlOutcome::Applied
    );
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
    assert_eq!(
        kernel.control(NOW, KernelControl::Stop).0,
        ControlOutcome::Unchanged
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
fn clarification_reply_atomically_checks_the_question_sequence() {
    let mut kernel = kernel();
    let input = Input::new(InputId(1), "ambiguous request");
    let (_, effects) = kernel.accept(NOW, input.clone()).unwrap();
    kernel.step(
        NOW,
        Event::CoordinateFinished {
            call: coordinate_call(&effects),
            result: Ok(KernelDecision::Clarify("which target?".into())),
        },
    );
    let question_seq = match input_status(&kernel, input.id) {
        InputStatus::WaitingForUser { question_seq, .. } => question_seq,
        status => panic!("expected a question, got {status:?}"),
    };
    let before = kernel.view().sequence;

    assert!(matches!(
        kernel.accept(
            NOW,
            Input::new(InputId(2), "stale answer").answering(input.id, Seq(question_seq.0 + 1)),
        ),
        Err(AdmissionError::StaleReply)
    ));
    assert_eq!(kernel.view().sequence, before);
    assert!(!kernel.inputs.contains_key(&InputId(2)));

    let (_, effects) = kernel
        .accept(
            NOW,
            Input::new(InputId(3), "the library target").answering(input.id, question_seq),
        )
        .unwrap();
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

    assert!(matches!(
        input_status(&kernel, original.id),
        InputStatus::WaitingForUser { question, .. } if question == "which public name?"
    ));
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

    let (_, effects) = kernel.control(NOW, KernelControl::Pause(parent.job));
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

    let (_, effects) = kernel.control(NOW, KernelControl::Resume(parent.job));
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

    kernel.control(NOW, KernelControl::Pause(parent.job));
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

    kernel.control(NOW, KernelControl::Stop);
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
fn inquiry_answer_explicitly_grants_its_evidence_to_the_requester() {
    let mut kernel = kernel();
    let input = Input::new(InputId(21), "ask a child for evidence");
    let (parent_call, parent) = create_roots(
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
        WorkStep::Inquire {
            job: child,
            question: "show the verification".into(),
        },
    );
    let effects = kernel.step(
        NOW,
        Event::WorkFinished {
            call: calls["child"].0,
            result: Ok(WorkProposal {
                note: Some("inquiry verification evidence".into()),
                report: None,
                answers: Vec::new(),
                step: WorkStep::Continue,
            }),
        },
    );
    let evidence = kernel
        .records
        .values()
        .find(|record| {
            matches!(&record.body, RecordBody::Note { text, .. } if text == "inquiry verification evidence")
        })
        .unwrap()
        .seq;
    let (child_call, child_context) = work_calls(&effects)
        .into_iter()
        .find(|(_, work)| work.job == child)
        .unwrap();
    let inquiry = child_context.inquiries[0].id;
    let effects = kernel.step(
        NOW,
        Event::WorkFinished {
            call: child_call,
            result: Ok(WorkProposal {
                note: None,
                report: None,
                answers: vec![InquiryAnswer {
                    inquiry,
                    response: InquiryResponse::Answer(ReportDraft {
                        summary: "verified answer".into(),
                        evidence: vec![evidence],
                    }),
                }],
                step: WorkStep::Continue,
            }),
        },
    );
    let parent_call = work_calls(&effects)
        .into_iter()
        .find(|(_, work)| work.job == parent.job)
        .unwrap()
        .0;

    let effects = work(
        &mut kernel,
        parent_call,
        WorkStep::Read(ReadQuery::Record {
            id: evidence,
            offset: 0,
        }),
    );
    let (_, context) = work_calls(&effects)
        .into_iter()
        .find(|(_, work)| work.job == parent.job)
        .expect("the inquiry requester can inspect explicitly cited evidence");
    assert!(context.records.iter().any(|view| view.source == evidence));
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

    let (_, effects) = kernel.control(NOW, KernelControl::Retry(update.id));
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
    assert!(matches!(
        input_status(&kernel, input.id),
        InputStatus::WaitingForUser { question, .. } if question == "first question?"
    ));
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

    assert!(matches!(
        input_status(&kernel, input.id),
        InputStatus::WaitingForUser { question, .. } if question == "better question?"
    ));
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
    kernel.control(NOW, KernelControl::Pause(target));
    kernel.control(NOW, KernelControl::Cancel(target));
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
    kernel.control(NOW, KernelControl::Cancel(finished));

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

#[test]
fn coordinator_directory_pages_obey_context_budget_without_accumulating_old_pages() {
    let limits = AgentLimits {
        context_bytes: 3_000,
        item_bytes: 1_024,
        ..AgentLimits::default()
    };
    let mut setup_limits = limits.clone();
    setup_limits.context_bytes = 64 * 1024;
    let mut kernel = Kernel::new(setup_limits, Vec::new()).unwrap();
    let input = Input::new(InputId(301), "create independent work");
    let assignments = (0..18)
        .map(|index| {
            assignment(
                &format!("root {index} {}", "directory-card-padding-".repeat(10)),
                &[input.id],
            )
        })
        .collect();
    create_roots(&mut kernel, input, assignments);
    kernel.limits.context_bytes = limits.context_bytes;

    let (_, effects) = kernel
        .accept(NOW, Input::new(InputId(302), "inspect the full directory"))
        .unwrap();
    let (mut call, first) = starts(&effects)
        .find_map(|(call, effect)| match effect {
            Call::Coordinate(input) => Some((call, input.clone())),
            _ => None,
        })
        .unwrap();
    assert!(first.jobs.len() < context::DIRECTORY_PAGE);
    assert!(serde_json::to_vec(&first).unwrap().len() <= limits.context_bytes);
    let mut seen = first.jobs.iter().map(|job| job.id).collect::<Vec<_>>();
    let mut cursor = first.next_job;
    let mut pages = 1;

    while let Some(after) = cursor {
        assert!(pages <= 18, "directory cursor did not advance");
        let effects = kernel.step(
            NOW,
            Event::CoordinateFinished {
                call,
                result: Ok(KernelDecision::Read(ReadQuery::Jobs {
                    parent: None,
                    after: Some(after),
                })),
            },
        );
        let (next_call, coordinate) = starts(&effects)
            .find_map(|(call, effect)| match effect {
                Call::Coordinate(input) => Some((call, input.clone())),
                _ => None,
            })
            .expect("a legal directory page starts the next coordination call");
        assert!(serde_json::to_vec(&coordinate).unwrap().len() <= limits.context_bytes);
        let visible_results = coordinate
            .records
            .iter()
            .filter(|view| {
                matches!(
                    kernel.records[&view.source].body,
                    RecordBody::ReadResult { .. }
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            visible_results.len(),
            1,
            "only the newest page is projected"
        );
        let (jobs, next) = match &kernel.records[&visible_results[0].source].body {
            RecordBody::ReadResult { jobs, next_job, .. } => (jobs, *next_job),
            _ => unreachable!(),
        };
        assert!(!jobs.is_empty());
        assert!(jobs.iter().all(|job| job.id > after));
        seen.extend(jobs.iter().map(|job| job.id));
        cursor = next;
        call = next_call;
        pages += 1;
    }

    let mut unique = seen.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(seen, unique);
    assert_eq!(seen.len(), 18);
    assert!(!kernel.records.values().any(|record| matches!(
        &record.body,
        RecordBody::InputRoutingFailed { inputs, .. } if inputs.contains(&InputId(302))
    )));
}

#[test]
fn coordinator_read_preflight_deduplicates_existing_record_views() {
    let mut kernel = Kernel::new(
        AgentLimits {
            item_bytes: 128,
            ..AgentLimits::default()
        },
        Vec::new(),
    )
    .unwrap();
    let (receipt, effects) = kernel
        .accept(
            NOW,
            Input::new(InputId(401), "existing context input ".repeat(6)),
        )
        .unwrap();
    let call = coordinate_call(&effects);
    let effects = kernel.step(
        NOW,
        Event::CoordinateFinished {
            call,
            result: Ok(KernelDecision::Read(ReadQuery::Record {
                id: receipt.accepted_at,
                offset: 0,
            })),
        },
    );
    let (call, coordinate) = starts(&effects)
        .find_map(|(call, effect)| match effect {
            Call::Coordinate(input) => Some((call, input.clone())),
            _ => None,
        })
        .unwrap();
    let full_size = serde_json::to_vec(&coordinate).unwrap().len();
    kernel.limits.context_bytes = full_size + 1;
    assert!(kernel.limits.validate().is_ok());

    let effects = kernel.step(
        NOW,
        Event::CoordinateFinished {
            call,
            result: Ok(KernelDecision::Read(ReadQuery::Record {
                id: receipt.accepted_at,
                offset: 0,
            })),
        },
    );
    let (_, projected) = starts(&effects)
        .find_map(|(call, effect)| match effect {
            Call::Coordinate(input) => Some((call, input)),
            _ => None,
        })
        .expect("the deduplicated projection fits");
    assert!(serde_json::to_vec(projected).unwrap().len() <= kernel.limits.context_bytes);
    assert_eq!(
        projected
            .records
            .iter()
            .filter(|view| view.source == receipt.accepted_at)
            .count(),
        1
    );
    assert!(
        !kernel
            .records
            .values()
            .any(|record| matches!(&record.body, RecordBody::InputRoutingFailed { .. }))
    );
}

#[test]
fn coordinator_last_directory_page_accounts_for_null_cursor_bytes() {
    let mut kernel = Kernel::new(
        AgentLimits {
            item_bytes: 128,
            ..AgentLimits::default()
        },
        Vec::new(),
    )
    .unwrap();
    let input = Input::new(InputId(501), "two roots");
    create_roots(
        &mut kernel,
        input.clone(),
        vec![
            assignment("first", &[input.id]),
            assignment("second", &[input.id]),
        ],
    );
    let (_, effects) = kernel
        .accept(NOW, Input::new(InputId(502), "read all roots"))
        .unwrap();
    let call = coordinate_call(&effects);
    let effects = kernel.step(
        NOW,
        Event::CoordinateFinished {
            call,
            result: Ok(KernelDecision::Read(ReadQuery::Jobs {
                parent: None,
                after: None,
            })),
        },
    );
    let (call, coordinate) = starts(&effects)
        .find_map(|(call, effect)| match effect {
            Call::Coordinate(input) => Some((call, input.clone())),
            _ => None,
        })
        .unwrap();
    let full_size = serde_json::to_vec(&coordinate).unwrap().len();
    kernel.limits.context_bytes = full_size - 1;

    let effects = kernel.step(
        NOW,
        Event::CoordinateFinished {
            call,
            result: Ok(KernelDecision::Read(ReadQuery::Jobs {
                parent: None,
                after: None,
            })),
        },
    );
    let (call, projected) = starts(&effects)
        .find_map(|(call, effect)| match effect {
            Call::Coordinate(input) => Some((call, input)),
            _ => None,
        })
        .expect("the page falls back to one card");
    assert!(serde_json::to_vec(projected).unwrap().len() <= kernel.limits.context_bytes);
    let page = projected
        .records
        .iter()
        .find_map(|view| match &kernel.records[&view.source].body {
            RecordBody::ReadResult { jobs, next_job, .. } => Some((jobs, *next_job)),
            _ => None,
        })
        .unwrap();
    assert_eq!(page.0.len(), 1);
    assert_eq!(page.1, Some(page.0[0].id));

    let effects = kernel.step(
        NOW,
        Event::CoordinateFinished {
            call,
            result: Ok(KernelDecision::Read(ReadQuery::Jobs {
                parent: None,
                after: page.1,
            })),
        },
    );
    let (_, projected) = starts(&effects)
        .find_map(|(call, effect)| match effect {
            Call::Coordinate(input) => Some((call, input)),
            _ => None,
        })
        .expect("the last card fits its final page");
    assert!(serde_json::to_vec(projected).unwrap().len() <= kernel.limits.context_bytes);
    let page = projected
        .records
        .iter()
        .find_map(|view| match &kernel.records[&view.source].body {
            RecordBody::ReadResult { jobs, next_job, .. } => Some((jobs, *next_job)),
            _ => None,
        })
        .unwrap();
    assert_eq!(page.0.len(), 1);
    assert_eq!(page.1, None);
    assert!(
        !kernel
            .records
            .values()
            .any(|record| matches!(&record.body, RecordBody::InputRoutingFailed { .. }))
    );
    assert!(full_size > kernel.limits.item_bytes);
}

#[test]
fn a_new_input_supersedes_a_clarification_and_preserves_its_context() {
    let mut kernel = kernel();
    let old = Input::new(InputId(701), "old ambiguous request");
    let (_, effects) = kernel.accept(NOW, old.clone()).unwrap();
    kernel.step(
        NOW,
        Event::CoordinateFinished {
            call: coordinate_call(&effects),
            result: Ok(KernelDecision::Clarify("which target?".into())),
        },
    );

    let new = Input::new(InputId(702), "new correction");
    let (_, effects) = kernel.accept(NOW, new.clone()).unwrap();
    let (call, coordinate) = starts(&effects)
        .find_map(|(call, effect)| match effect {
            Call::Coordinate(input) => Some((call, input)),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        coordinate
            .inputs
            .iter()
            .map(|input| input.id)
            .collect::<Vec<_>>(),
        vec![old.id, new.id]
    );
    assert!(coordinate.records.iter().any(|view| matches!(
        kernel.records[&view.source].body,
        RecordBody::Clarification { .. }
    )));

    let effects = kernel.step(
        NOW,
        Event::CoordinateFinished {
            call,
            result: Ok(KernelDecision::Apply {
                changes: Vec::new(),
                constraints: None,
            }),
        },
    );
    assert!(starts(&effects).next().is_none());
    assert_eq!(
        input_status(&kernel, old.id),
        InputStatus::Finished(InputOutcome::Completed)
    );
    assert_eq!(
        input_status(&kernel, new.id),
        InputStatus::Finished(InputOutcome::Completed)
    );
    assert!(matches!(
        kernel.accept(
            NOW,
            Input::new(InputId(703), "late reply").replying_to(old.id)
        ),
        Err(AdmissionError::InvalidReply)
    ));
}

#[test]
fn a_new_input_cancels_a_waiting_routing_inquiry_and_ignores_its_late_answer() {
    let mut kernel = kernel();
    let root_input = Input::new(InputId(711), "create target");
    let (target_call, target) = create_roots(
        &mut kernel,
        root_input.clone(),
        vec![assignment("target", &[root_input.id])],
    )
    .pop()
    .unwrap();
    work(
        &mut kernel,
        target_call,
        WorkStep::Wait(Await::After(Duration::from_secs(10))),
    );

    let old = Input::new(InputId(712), "ask target");
    let (_, effects) = kernel.accept(NOW, old.clone()).unwrap();
    let effects = kernel.step(
        NOW,
        Event::CoordinateFinished {
            call: coordinate_call(&effects),
            result: Ok(KernelDecision::Inquire {
                job: target.job,
                question: "status?".into(),
            }),
        },
    );
    let (target_call, target_context) = work_calls(&effects)
        .into_iter()
        .find(|(_, work)| work.job == target.job)
        .unwrap();
    let inquiry = target_context.inquiries[0].id;

    let new = Input::new(InputId(713), "replace inquiry");
    let (_, effects) = kernel.accept(NOW, new.clone()).unwrap();
    assert!(starts(&effects).any(|(_, effect)| matches!(effect, Call::Coordinate(_))));
    assert!(!kernel.inquiries.contains_key(&inquiry));

    let effects = kernel.step(
        NOW,
        Event::WorkFinished {
            call: target_call,
            result: Ok(WorkProposal {
                note: None,
                report: None,
                answers: vec![InquiryAnswer {
                    inquiry,
                    response: InquiryResponse::Answer(ReportDraft {
                        summary: "late".into(),
                        evidence: Vec::new(),
                    }),
                }],
                step: WorkStep::Continue,
            }),
        },
    );
    assert!(kernel.records.values().any(|record| matches!(
        &record.body,
        RecordBody::Audit { message } if message.contains("ignored a settled inquiry answer")
    )));
    assert!(!effects.iter().any(|effect| matches!(
        effect,
        Effect::Notify(record)
            if matches!(
                record.body,
                RecordBody::Delivery {
                    to: DeliveryTarget::Routing(_),
                    kind: crate::DeliveryKind::InquiryResult,
                    ..
                }
            )
    )));
    assert_eq!(input_status(&kernel, old.id), InputStatus::Routing);
    assert_eq!(input_status(&kernel, new.id), InputStatus::Routing);
}

#[test]
fn a_new_input_cancels_a_waiting_routing_investigation_and_its_late_finish() {
    let mut kernel = kernel();
    let old = Input::new(InputId(721), "investigate old");
    let (_, effects) = kernel.accept(NOW, old.clone()).unwrap();
    let effects = kernel.step(
        NOW,
        Event::CoordinateFinished {
            call: coordinate_call(&effects),
            result: Ok(KernelDecision::Investigate(assignment(
                "old investigation",
                &[old.id],
            ))),
        },
    );
    let (old_work, investigation) = work_calls(&effects).pop().unwrap();

    let new = Input::new(InputId(722), "replace investigation");
    let (_, effects) = kernel.accept(NOW, new.clone()).unwrap();
    let coordinate = starts(&effects)
        .find_map(|(_, effect)| match effect {
            Call::Coordinate(input) => Some(input),
            _ => None,
        })
        .unwrap();
    assert!(finished_as(
        &kernel,
        investigation.job,
        OutcomeKind::Cancelled
    ));
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Cancel(call) if *call == old_work))
    );
    assert_eq!(
        coordinate
            .inputs
            .iter()
            .map(|input| input.id)
            .collect::<Vec<_>>(),
        vec![old.id, new.id]
    );

    let effects = work(
        &mut kernel,
        old_work,
        WorkStep::Finish(Completion::new("late stale finding")),
    );
    assert!(starts(&effects).next().is_none());
    assert!(finished_as(
        &kernel,
        investigation.job,
        OutcomeKind::Cancelled
    ));
    assert!(kernel.records.values().any(|record| matches!(
        &record.body,
        RecordBody::Audit { message } if message.contains("discarded a model result")
    )));
}

#[test]
fn parent_finish_waits_until_an_unknown_descendant_write_is_resolved() {
    let mut kernel = kernel_with(ToolSpec {
        name: "write".into(),
        description: "write".into(),
        parameters: json!({ "type": "object" }),
        effect: ToolEffect::ExternalWrite,
    });
    let input = Input::new(InputId(731), "nested write");
    let (parent_call, parent) = create_roots(
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
    let (child_call, child) = calls["child"].clone();
    work(
        &mut kernel,
        calls["parent"].0,
        WorkStep::Wait(Await::Job(child.job)),
    );
    let write_call = tool_call(&work(
        &mut kernel,
        child_call,
        WorkStep::Tool(ToolCall::new("write", json!({}))),
    ));

    let (_, effects) = kernel.control(NOW, KernelControl::Cancel(child.job));
    let parent_call = work_calls(&effects)
        .into_iter()
        .find(|(_, work)| work.job == parent.job)
        .unwrap()
        .0;
    work(
        &mut kernel,
        parent_call,
        WorkStep::Finish(Completion::new("done after truth")),
    );
    kernel.step(
        NOW,
        Event::ToolFinished {
            call: write_call,
            result: ToolOutcome {
                result: Err(crate::CallError::failed("unknown")),
                external_effect: ExternalEffect::Unknown,
            },
        },
    );
    assert!(!finished_as(&kernel, parent.job, OutcomeKind::Completed));

    let (outcome, _) = kernel.control(
        NOW,
        KernelControl::ResolveWrite {
            call: write_call,
            result: ToolOutcome {
                result: Ok(json!({ "resolved": true })),
                external_effect: ExternalEffect::None,
            },
        },
    );
    assert_eq!(outcome, ControlOutcome::Applied);
    assert_eq!(
        kernel
            .control(
                NOW,
                KernelControl::ResolveWrite {
                    call: write_call,
                    result: ToolOutcome {
                        result: Ok(json!({ "resolved": true })),
                        external_effect: ExternalEffect::None,
                    },
                },
            )
            .0,
        ControlOutcome::Unchanged
    );
    assert!(finished_as(&kernel, parent.job, OutcomeKind::Completed));
}

#[test]
fn coordinator_initial_directory_accounts_for_null_cursor_bytes() {
    let mut kernel = Kernel::new(
        AgentLimits {
            item_bytes: 128,
            ..AgentLimits::default()
        },
        Vec::new(),
    )
    .unwrap();
    let input = Input::new(InputId(601), "two roots");
    create_roots(
        &mut kernel,
        input.clone(),
        vec![
            assignment("first", &[input.id]),
            assignment("second", &[input.id]),
        ],
    );
    let (_, effects) = kernel
        .accept(NOW, Input::new(InputId(602), "show the root directory"))
        .unwrap();
    let coordinate = starts(&effects)
        .find_map(|(_, effect)| match effect {
            Call::Coordinate(input) => Some(input.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(coordinate.jobs.len(), 2);
    assert_eq!(coordinate.next_job, None);
    let full_size = serde_json::to_vec(&coordinate).unwrap().len();
    kernel.limits.context_bytes = full_size - 1;

    let projected = context::prepare_coordinate(&kernel, coordinate.routing)
        .expect("the initial directory falls back to one card");
    assert!(serde_json::to_vec(&projected).unwrap().len() <= kernel.limits.context_bytes);
    assert_eq!(projected.jobs.len(), 1);
    assert_eq!(projected.next_job, Some(projected.jobs[0].id));
}
