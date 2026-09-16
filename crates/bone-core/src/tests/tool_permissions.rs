use super::*;
use crate::{JobId, ToolSelection};
use std::collections::BTreeSet;

fn tools() -> Vec<ToolSpec> {
    [
        ("read", ToolEffect::ReadOnly),
        ("bash", ToolEffect::ExternalWrite),
    ]
    .into_iter()
    .map(|(name, effect)| ToolSpec {
        name: name.into(),
        description: name.into(),
        parameters: json!({"type":"object"}),
        effect,
    })
    .collect()
}

fn names(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|name| (*name).into()).collect()
}

fn restricted_root(kernel: &mut Kernel, selection: Option<ToolSelection>) -> (CallId, WorkInput) {
    let mut root = assignment("analyze", &[InputId(1)]);
    root.tools = selection;
    create_roots(
        kernel,
        Input::new(InputId(1), "只读分析，不修改"),
        vec![root],
    )
    .pop()
    .unwrap()
}

#[test]
fn readonly_is_resolved_from_effect_and_unauthorized_calls_never_execute() {
    let mut kernel = Kernel::new(AgentLimits::default(), tools()).unwrap();
    let (call, root) = restricted_root(&mut kernel, Some(ToolSelection::ReadOnly));
    assert_eq!(
        root.tools
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["read"]
    );
    assert_eq!(kernel.jobs[&root.job].allowed_tools, names(&["read"]));
    assert!(kernel.records.values().any(|record| matches!(&record.body,
        RecordBody::JobCreated { allowed_tools, .. } if *allowed_tools == names(&["read"]))));
    let effects = work(
        &mut kernel,
        call,
        WorkStep::Tool(ToolCall::new("bash", json!({}))),
    );
    assert!(!starts(&effects).any(|(_, call)| matches!(call, Call::Tool(_))));
    assert!(kernel.records.values().any(|record| matches!(&record.body,
        RecordBody::WorkRejected { message, .. } if message.contains("not authorized"))));
    assert_eq!(kernel.jobs[&root.job].allowed_tools, names(&["read"]));
}

#[test]
fn children_inherit_or_narrow_but_cannot_gain_authority() {
    let mut kernel = Kernel::new(AgentLimits::default(), tools()).unwrap();
    let (call, root) = restricted_root(&mut kernel, Some(ToolSelection::ReadOnly));
    let mut forbidden = assignment("write", &[]);
    forbidden.tools = Some(ToolSelection::Only(vec!["bash".into()]));
    let rejected = work(
        &mut kernel,
        call,
        WorkStep::delegate(vec![assignment("valid sibling", &[]), forbidden]),
    );
    assert_eq!(kernel.jobs.len(), 1);
    let (call, _) = work_calls(&rejected).pop().unwrap();
    let mut unknown = assignment("unknown", &[]);
    unknown.tools = Some(ToolSelection::Only(vec!["missing".into()]));
    let rejected = work(&mut kernel, call, WorkStep::delegate(vec![unknown]));
    assert_eq!(kernel.jobs.len(), 1);
    let (call, _) = work_calls(&rejected).pop().unwrap();
    let mut empty = assignment("reason", &[]);
    empty.tools = Some(ToolSelection::Only(Vec::new()));
    let effects = work(
        &mut kernel,
        call,
        WorkStep::delegate_and_wait(vec![assignment("inspect", &[]), empty]),
    );
    let children = work_calls(&effects);
    assert_eq!(children.len(), 2);
    for (_, child) in children {
        let allowed = &kernel.jobs[&child.job].allowed_tools;
        assert!(allowed.is_subset(&kernel.jobs[&root.job].allowed_tools));
        if child.spec.goal == "reason" {
            assert!(child.tools.is_empty());
        } else {
            assert_eq!(*allowed, names(&["read"]));
        }
    }
}

#[test]
fn conversation_rejects_mismatched_existing_authority_without_partial_delivery() {
    for (initial, requested) in [
        (None, ToolSelection::ReadOnly),
        (
            Some(ToolSelection::ReadOnly),
            ToolSelection::Only(vec!["read".into(), "bash".into()]),
        ),
    ] {
        let mut kernel = Kernel::new(AgentLimits::default(), tools()).unwrap();
        let (_, root) = restricted_root(&mut kernel, initial);
        let (_, effects) = kernel
            .accept(NOW, Input::new(InputId(2), "different authority"))
            .unwrap();
        let effects = kernel.step(
            NOW,
            Event::ConverseFinished {
                call: converse_call(&effects),
                result: Ok(ConversationStep::Send {
                    inputs: vec![InputId(2)],
                    job: root.job,
                    message: "new constraint".into(),
                    question: None,
                    tools: Some(requested),
                }),
            },
        );
        assert!(!kernel.jobs[&root.job].inputs.contains(&InputId(2)));
        assert!(!starts(&effects).any(|(_, call)| matches!(call, Call::Work(_))));
        assert!(kernel.records.values().any(|record| matches!(&record.body,
            RecordBody::ConversationRejected { message, .. } if message.contains("different tool authority"))));
    }
}

#[test]
fn empty_authority_and_unknown_tools_have_distinct_meanings() {
    let mut kernel = Kernel::new(AgentLimits::default(), tools()).unwrap();
    let (call, root) = restricted_root(&mut kernel, Some(ToolSelection::Only(Vec::new())));
    assert!(root.tools.is_empty());
    let _ = work(
        &mut kernel,
        call,
        WorkStep::Finish(Completion::new("reasoned without tools")),
    );
    assert!(matches!(
        kernel.job_status(root.job),
        JobStatus::Finished(_)
    ));
    converse_step(
        &mut kernel,
        ConversationStep::Reply {
            inputs: vec![InputId(1)],
            text: "done".into(),
            outcome: InputOutcome::Completed,
        },
    );
    let (_, effects) = kernel
        .accept(NOW, Input::new(InputId(2), "unknown"))
        .unwrap();
    let _ = kernel.step(
        NOW,
        Event::ConverseFinished {
            call: converse_call(&effects),
            result: Ok(ConversationStep::Start(vec![Assignment {
                inputs: vec![InputId(2)],
                tools: Some(ToolSelection::Only(vec!["absent".into()])),
                ..Assignment::new(spec("unknown"))
            }])),
        },
    );
    assert_eq!(kernel.jobs.len(), 1);
}

#[test]
fn reply_reconfigure_and_restore_preserve_job_authority() {
    let mut kernel = Kernel::new(AgentLimits::default(), tools()).unwrap();
    let (call, root) = restricted_root(&mut kernel, Some(ToolSelection::ReadOnly));
    let _ = work(&mut kernel, call, WorkStep::NeedInput("which file?".into()));
    converse_step(
        &mut kernel,
        ConversationStep::Ask {
            inputs: vec![InputId(1)],
            question: "which file?".into(),
        },
    );
    let reply = Input::new(InputId(2), "main.rs; now write it");
    let (_, effects) = kernel.accept(NOW, reply).unwrap();
    let effects = send_existing(&mut kernel, &effects, root.job, &[InputId(2)]);
    let (_, resumed) = work_calls(&effects).pop().unwrap();
    assert_eq!(
        resumed
            .tools
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["read"]
    );
    let mut catalog = tools();
    catalog.push(ToolSpec {
        name: "new_read".into(),
        description: "new".into(),
        parameters: json!({"type":"object"}),
        effect: ToolEffect::ReadOnly,
    });
    kernel
        .reconfigure(NOW, AgentLimits::default(), catalog.clone())
        .unwrap();
    assert_eq!(kernel.jobs[&root.job].allowed_tools, names(&["read"]));
    let (restored, effects) = Kernel::restore(
        kernel.durable_snapshot().unwrap(),
        kernel.records.values().cloned().collect(),
        AgentLimits::default(),
        catalog,
    )
    .unwrap();
    assert_eq!(restored.jobs[&root.job].allowed_tools, names(&["read"]));
    assert!(matches!(
        restored.job_status(JobId(root.job.0)),
        JobStatus::Finished(_)
    ));
    assert!(starts(&effects).next().is_none());
}

#[test]
fn terminal_work_requires_one_explicit_conclusion() {
    let mut kernel = Kernel::new(AgentLimits::default(), tools()).unwrap();
    let (call, _) = restricted_root(&mut kernel, None);
    work(&mut kernel, call, WorkStep::Finish(Completion::new("done")));
    let effects = converse_step(&mut kernel, ConversationStep::Wait);
    assert!(starts(&effects).any(|(_, call)| matches!(call, Call::Converse(_))));
    assert!(kernel.records.values().any(|record| matches!(&record.body, RecordBody::ConversationRejected { message, .. } if message.contains("conclude"))));
    converse_step(
        &mut kernel,
        ConversationStep::Reply {
            inputs: vec![InputId(1)],
            text: "done".into(),
            outcome: InputOutcome::Completed,
        },
    );
    assert_eq!(
        kernel.inputs[&InputId(1)].finished,
        Some(InputOutcome::Completed)
    );
    let before = kernel
        .records
        .values()
        .filter(|record| matches!(record.body, RecordBody::ConversationRejected { .. }))
        .count();
    let effects = converse_step(&mut kernel, ConversationStep::Wait);
    assert!(!starts(&effects).any(|(_, call)| matches!(call, Call::Converse(_))));
    assert_eq!(
        before,
        kernel
            .records
            .values()
            .filter(|record| matches!(record.body, RecordBody::ConversationRejected { .. }))
            .count(),
        "a conversation with no open inputs may idle"
    );
}

#[test]
fn waiting_for_missing_information_requires_a_question_not_an_unwakeable_wait() {
    let mut kernel = Kernel::new(AgentLimits::default(), tools()).unwrap();
    let (call, _) = restricted_root(&mut kernel, None);
    work(&mut kernel, call, WorkStep::NeedInput("which file?".into()));
    let effects = converse_step(&mut kernel, ConversationStep::Wait);
    assert!(starts(&effects).any(|(_, call)| matches!(call, Call::Converse(_))));
    assert!(kernel.records.values().any(|record| matches!(&record.body, RecordBody::ConversationRejected { message, .. } if message.contains("nothing can wake"))));
    let effects = converse_step(
        &mut kernel,
        ConversationStep::Ask {
            inputs: vec![InputId(1)],
            question: "which file?".into(),
        },
    );
    assert!(!starts(&effects).any(|(_, call)| matches!(call, Call::Converse(_))));
}

#[test]
fn a_cancelled_job_with_an_unsettled_write_cannot_be_reported_completed() {
    let mut kernel = Kernel::new(AgentLimits::default(), tools()).unwrap();
    let (call, root) = restricted_root(&mut kernel, None);
    let effects = work(
        &mut kernel,
        call,
        WorkStep::Tool(ToolCall::new("bash", json!({}))),
    );
    let running = tool_call(&effects);
    kernel.control(NOW, KernelControl::Cancel(root.job));
    let effects = converse_step(
        &mut kernel,
        ConversationStep::Reply {
            inputs: vec![InputId(1)],
            text: "done".into(),
            outcome: InputOutcome::Completed,
        },
    );
    assert!(starts(&effects).any(|(_, call)| matches!(call, Call::Converse(_))));
    assert!(kernel.inputs[&InputId(1)].finished.is_none());
    assert!(kernel.calls.contains_key(&running));
    converse_step(
        &mut kernel,
        ConversationStep::Reply {
            inputs: vec![InputId(1)],
            text: "Cancelled; the running write has not settled.".into(),
            outcome: InputOutcome::Cancelled,
        },
    );
    assert_eq!(
        kernel.inputs[&InputId(1)].finished,
        Some(InputOutcome::Cancelled)
    );
}
