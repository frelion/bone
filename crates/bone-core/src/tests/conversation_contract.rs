use std::time::Duration;

use serde_json::json;

use crate::{
    AgentLimits, Assignment, CallId, Completion, ConversationStep, Input, InputId, InputOutcome,
    JobSpec, RecordBody, ToolCall, ToolEffect, ToolOutcome, ToolSelection, ToolSpec, WorkProposal,
    WorkStep,
    kernel::Kernel,
    ports::{Call, Effect, Event},
};

const NOW: crate::MonoTime = crate::MonoTime(Duration::ZERO);

fn kernel() -> Kernel {
    let tools = [
        ("read", ToolEffect::ReadOnly),
        ("bash", ToolEffect::ExternalWrite),
    ]
    .into_iter()
    .map(|(name, effect)| ToolSpec {
        name: name.into(),
        description: name.into(),
        parameters: json!({"type": "object"}),
        effect,
    })
    .collect();
    Kernel::new(AgentLimits::default(), tools).unwrap()
}

fn calls(effects: &[Effect]) -> impl Iterator<Item = (CallId, &Call)> {
    effects.iter().filter_map(|effect| match effect {
        Effect::Start { id, call, .. } => Some((*id, call.as_ref())),
        _ => None,
    })
}

fn converse(kernel: &mut Kernel, effects: &[Effect], step: ConversationStep) -> Vec<Effect> {
    let call = calls(effects)
        .find_map(|(id, call)| matches!(call, Call::Converse(_)).then_some(id))
        .expect("conversation must be scheduled");
    kernel.step(
        NOW,
        Event::ConverseFinished {
            call,
            result: Ok(step),
        },
    )
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

fn assignment(goal: &str, input: u64) -> Assignment {
    let mut assignment = Assignment::new(JobSpec::new(goal, "workspace", "report verified result"));
    assignment.inputs = vec![InputId(input)];
    assignment
}

fn reply(inputs: &[u64], text: &str, outcome: InputOutcome) -> ConversationStep {
    ConversationStep::Reply {
        inputs: inputs.iter().map(|id| InputId(*id)).collect(),
        text: text.into(),
        outcome,
    }
}

#[test]
fn greeting_is_one_conversation_call_with_no_job_or_worker() {
    let mut kernel = kernel();
    let (_, effects) = kernel
        .accept(NOW, Input::new(InputId(1), "你好呀"))
        .unwrap();
    assert_eq!(calls(&effects).count(), 1);
    let effects = converse(
        &mut kernel,
        &effects,
        reply(&[1], "你好！", InputOutcome::Completed),
    );
    assert!(kernel.view().jobs.is_empty());
    assert!(calls(&effects).next().is_none());
    let replies = kernel
        .view()
        .records
        .iter()
        .filter_map(|record| match &record.body {
            RecordBody::Reply { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(replies, ["你好！"]);
    assert!(kernel.view().records.iter().any(|record| matches!(
        record.body,
        RecordBody::InputFinished {
            input: InputId(1),
            outcome: InputOutcome::Completed
        }
    )));
}

#[test]
fn finished_work_returns_to_conversation_before_input_can_complete() {
    let mut kernel = kernel();
    let (_, effects) = kernel
        .accept(NOW, Input::new(InputId(1), "检查文件"))
        .unwrap();
    let effects = converse(
        &mut kernel,
        &effects,
        ConversationStep::Start(vec![assignment("检查文件", 1)]),
    );
    let worker = calls(&effects)
        .find_map(|(id, call)| matches!(call, Call::Work(_)).then_some(id))
        .unwrap();
    let effects = work(
        &mut kernel,
        worker,
        WorkStep::Finish(Completion::new("internal evidence summary")),
    );
    assert!(!kernel.view().records.iter().any(|record| matches!(
        record.body,
        RecordBody::Reply { .. } | RecordBody::InputFinished { .. }
    )));
    let effects = converse(
        &mut kernel,
        &effects,
        reply(
            &[1],
            "文件检查完成，内容符合要求。",
            InputOutcome::Completed,
        ),
    );
    assert!(calls(&effects).next().is_none());
    assert_eq!(
        kernel
            .view()
            .records
            .iter()
            .filter(|record| matches!(record.body, RecordBody::Reply { .. }))
            .count(),
        1
    );
}

#[test]
fn followup_after_failure_keeps_original_goal_and_creates_new_authorized_work() {
    let mut kernel = kernel();
    let (_, effects) = kernel
        .accept(NOW, Input::new(InputId(1), "查南京天气"))
        .unwrap();
    let mut read_only = assignment("查南京天气", 1);
    read_only.tools = Some(ToolSelection::ReadOnly);
    let effects = converse(
        &mut kernel,
        &effects,
        ConversationStep::Start(vec![read_only]),
    );
    let (worker, original_job) = calls(&effects)
        .find_map(|(id, call)| match call {
            Call::Work(input) => Some((id, input.job)),
            _ => None,
        })
        .unwrap();
    let effects = work(
        &mut kernel,
        worker,
        WorkStep::Fail(Completion::new("query needs a command")),
    );
    converse(
        &mut kernel,
        &effects,
        reply(&[1], "这次没有查到天气。", InputOutcome::Failed),
    );

    let (_, effects) = kernel
        .accept(NOW, Input::new(InputId(2), "用 bash 啊"))
        .unwrap();
    let context = calls(&effects)
        .find_map(|(_, call)| match call {
            Call::Converse(input) => Some(input),
            _ => None,
        })
        .unwrap();
    let serialized = serde_json::to_string(context).unwrap();
    assert!(serialized.contains("查南京天气"));
    assert!(serialized.contains("用 bash 啊"));
    let mut continued = assignment("用 bash 查询南京天气", 2);
    continued.tools = Some(ToolSelection::Only(vec!["bash".into()]));
    let effects = converse(
        &mut kernel,
        &effects,
        ConversationStep::Start(vec![continued]),
    );
    let (worker, new_job) = calls(&effects)
        .find_map(|(id, call)| match call {
            Call::Work(input) => {
                assert_eq!(
                    input
                        .tools
                        .iter()
                        .map(|tool| tool.name.as_str())
                        .collect::<Vec<_>>(),
                    ["bash"]
                );
                Some((id, input.job))
            }
            _ => None,
        })
        .unwrap();
    assert_ne!(original_job, new_job);
    assert!(
        !kernel
            .view()
            .jobs
            .iter()
            .find(|job| job.id == original_job)
            .unwrap()
            .allowed_tools
            .contains("bash")
    );
    let effects = work(
        &mut kernel,
        worker,
        WorkStep::Tool(ToolCall::new("bash", json!({}))),
    );
    let tool = calls(&effects)
        .find_map(|(id, call)| matches!(call, Call::Tool(_)).then_some(id))
        .unwrap();
    let effects = kernel.step(
        NOW,
        Event::ToolFinished {
            call: tool,
            result: ToolOutcome::value(json!({"weather":"sunny"})),
        },
    );
    let worker = calls(&effects)
        .find_map(|(id, call)| matches!(call, Call::Work(_)).then_some(id))
        .unwrap();
    let effects = work(
        &mut kernel,
        worker,
        WorkStep::Finish(Completion::new("南京晴")),
    );
    converse(
        &mut kernel,
        &effects,
        reply(&[2], "查询结果：南京晴。", InputOutcome::Completed),
    );
    assert!(kernel.view().records.iter().any(|record| matches!(
        record.body,
        RecordBody::InputFinished {
            input: InputId(2),
            outcome: InputOutcome::Completed
        }
    )));
}

#[test]
fn newer_user_input_prevents_stale_conversation_reply() {
    let mut kernel = kernel();
    let (_, original) = kernel
        .accept(NOW, Input::new(InputId(1), "修改文件"))
        .unwrap();
    let (_, newer) = kernel
        .accept(NOW, Input::new(InputId(2), "等等，只解释，不修改"))
        .unwrap();
    let late = converse(
        &mut kernel,
        &original,
        reply(&[1], "过期回答", InputOutcome::Completed),
    );
    assert!(
        !kernel
            .view()
            .records
            .iter()
            .any(|record| matches!(&record.body,
        RecordBody::Reply { text, .. } if text == "过期回答"))
    );
    let current = if calls(&newer).any(|(_, call)| matches!(call, Call::Converse(_))) {
        newer
    } else {
        late
    };
    converse(
        &mut kernel,
        &current,
        reply(&[1, 2], "我会只解释。", InputOutcome::Completed),
    );
    assert!(kernel.view().jobs.is_empty());
}

#[test]
fn one_finished_job_cannot_conclude_input_with_other_work_running() {
    let mut kernel = kernel();
    let (_, effects) = kernel
        .accept(NOW, Input::new(InputId(1), "检查两个模块"))
        .unwrap();
    let effects = converse(
        &mut kernel,
        &effects,
        ConversationStep::Start(vec![assignment("模块一", 1), assignment("模块二", 1)]),
    );
    let workers = calls(&effects)
        .filter_map(|(id, call)| matches!(call, Call::Work(_)).then_some(id))
        .collect::<Vec<_>>();
    assert_eq!(workers.len(), 2);
    let effects = work(
        &mut kernel,
        workers[0],
        WorkStep::Finish(Completion::new("模块一完成")),
    );
    converse(
        &mut kernel,
        &effects,
        reply(&[1], "全部完成", InputOutcome::Completed),
    );
    assert!(
        !kernel
            .view()
            .records
            .iter()
            .any(|record| matches!(record.body, RecordBody::InputFinished { .. }))
    );
    assert!(
        !kernel
            .view()
            .records
            .iter()
            .any(|record| matches!(&record.body,
        RecordBody::Reply { text, .. } if text == "全部完成"))
    );
}

#[test]
fn job_result_arriving_during_conversation_is_not_lost_by_stale_wait() {
    let mut kernel = kernel();
    let (_, effects) = kernel
        .accept(NOW, Input::new(InputId(1), "检查两个模块"))
        .unwrap();
    let effects = converse(
        &mut kernel,
        &effects,
        ConversationStep::Start(vec![assignment("模块一", 1), assignment("模块二", 1)]),
    );
    let workers = calls(&effects)
        .filter_map(|(id, call)| matches!(call, Call::Work(_)).then_some(id))
        .collect::<Vec<_>>();
    let first = work(
        &mut kernel,
        workers[0],
        WorkStep::Finish(Completion::new("第一个完成")),
    );
    let second = work(
        &mut kernel,
        workers[1],
        WorkStep::Finish(Completion::new("第二个完成")),
    );
    assert!(!calls(&second).any(|(_, call)| matches!(call, Call::Converse(_))));
    let effects = converse(&mut kernel, &first, ConversationStep::Wait);
    assert!(kernel.view().records.iter().any(|record| matches!(
        &record.body,
        RecordBody::ConversationRejected { message, .. } if message.contains("conclude")
    )));
    let context = calls(&effects)
        .find_map(|(_, call)| match call {
            Call::Converse(input) => Some(input),
            _ => None,
        })
        .unwrap();
    assert!(
        serde_json::to_string(context)
            .unwrap()
            .contains("第二个完成")
    );
    converse(
        &mut kernel,
        &effects,
        reply(&[1], "两个模块均已完成。", InputOutcome::Completed),
    );
}

#[test]
fn pending_user_question_survives_background_result_and_resumes_exact_job() {
    let mut kernel = kernel();
    let (_, effects) = kernel
        .accept(NOW, Input::new(InputId(1), "检查两个模块"))
        .unwrap();
    let effects = converse(
        &mut kernel,
        &effects,
        ConversationStep::Start(vec![assignment("需要文件名", 1), assignment("独立检查", 1)]),
    );
    let workers = calls(&effects)
        .filter_map(|(id, call)| match call {
            Call::Work(input) => Some((id, input.job)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let effects = work(
        &mut kernel,
        workers[0].0,
        WorkStep::NeedInput("文件名是什么？".into()),
    );
    assert!(
        !kernel
            .view()
            .records
            .iter()
            .any(|record| matches!(record.body, RecordBody::Clarification { .. }))
    );
    let job_question = kernel
        .view()
        .records
        .iter()
        .find_map(|record| {
            matches!(record.body, RecordBody::JobNeedsInput { .. }).then_some(record.seq)
        })
        .unwrap();
    converse(
        &mut kernel,
        &effects,
        ConversationStep::Ask {
            inputs: vec![InputId(1)],
            question: "要检查哪个文件？".into(),
        },
    );
    let question = kernel
        .view()
        .records
        .iter()
        .find_map(|record| {
            matches!(record.body, RecordBody::Clarification { .. }).then_some(record.seq)
        })
        .unwrap();
    let effects = work(
        &mut kernel,
        workers[1].0,
        WorkStep::Finish(Completion::new("独立检查完成")),
    );
    assert!(!calls(&effects).any(|(_, call)| matches!(call, Call::Converse(_))));

    let answer = Input::new(InputId(2), "main.rs").answering(InputId(1), question);
    let (_, effects) = kernel.accept(NOW, answer).unwrap();
    let effects = converse(
        &mut kernel,
        &effects,
        ConversationStep::Send {
            job: workers[0].1,
            inputs: vec![InputId(1), InputId(2)],
            message: "检查 main.rs".into(),
            question: Some(job_question),
            tools: None,
        },
    );
    let worker = calls(&effects)
        .find_map(|(id, call)| match call {
            Call::Work(input) => {
                assert_eq!(input.job, workers[0].1);
                Some(id)
            }
            _ => None,
        })
        .unwrap();
    let effects = work(
        &mut kernel,
        worker,
        WorkStep::Finish(Completion::new("main.rs 已检查")),
    );
    converse(
        &mut kernel,
        &effects,
        reply(&[1, 2], "检查完成。", InputOutcome::Completed),
    );
    assert!(
        kernel
            .accept(
                NOW,
                Input::new(InputId(3), "过期答案").answering(InputId(1), question)
            )
            .is_err()
    );
}

#[test]
fn parent_answers_child_question_without_publishing_user_question() {
    let mut kernel = kernel();
    let (_, effects) = kernel
        .accept(NOW, Input::new(InputId(1), "检查模块"))
        .unwrap();
    let effects = converse(
        &mut kernel,
        &effects,
        ConversationStep::Start(vec![assignment("父任务", 1)]),
    );
    let (parent_call, parent) = calls(&effects)
        .find_map(|(id, call)| match call {
            Call::Work(input) => Some((id, input.job)),
            _ => None,
        })
        .unwrap();
    let effects = work(
        &mut kernel,
        parent_call,
        WorkStep::delegate_and_wait(vec![assignment("子任务", 1)]),
    );
    let (child_call, child) = calls(&effects)
        .find_map(|(id, call)| match call {
            Call::Work(input) => Some((id, input.job)),
            _ => None,
        })
        .unwrap();
    let effects = work(
        &mut kernel,
        child_call,
        WorkStep::NeedInput("哪个文件？".into()),
    );
    let question = kernel
        .view()
        .records
        .iter()
        .find_map(|record| {
            matches!(record.body, RecordBody::JobNeedsInput { job, .. } if job == child)
                .then_some(record.seq)
        })
        .unwrap();
    let parent_call = calls(&effects)
        .find_map(|(id, call)| match call {
            Call::Work(input) if input.job == parent => Some(id),
            _ => None,
        })
        .unwrap();
    let effects = work(
        &mut kernel,
        parent_call,
        WorkStep::Respond {
            job: child,
            question,
            message: "main.rs".into(),
        },
    );
    assert!(
        calls(&effects).any(|(_, call)| matches!(call, Call::Work(input) if input.job == child))
    );
    assert!(!kernel.view().records.iter().any(|record| matches!(
        record.body,
        RecordBody::Clarification { .. } | RecordBody::Reply { .. }
    )));
}

#[test]
fn invalid_conversation_proposals_retry_with_bounded_feedback_and_reset_on_new_input() {
    let mut kernel = kernel();
    let (_, mut effects) = kernel.accept(NOW, Input::new(InputId(1), "你好")).unwrap();
    for _ in 0..crate::WorkRejections::CONSECUTIVE_LIMIT {
        effects = converse(
            &mut kernel,
            &effects,
            reply(&[1, 1], "重复结案", InputOutcome::Completed),
        );
    }
    assert!(calls(&effects).next().is_none());
    assert!(!kernel.view().records.iter().any(|record| matches!(
        record.body,
        RecordBody::Reply { .. } | RecordBody::InputFinished { .. }
    )));
    assert_eq!(
        kernel
            .view()
            .records
            .iter()
            .filter(|record| matches!(record.body, RecordBody::ConversationRejected { .. }))
            .count(),
        3
    );
    assert!(calls(&kernel.step(NOW, Event::Tick)).next().is_none());
    let (_, effects) = kernel
        .accept(NOW, Input::new(InputId(2), "请重新回答"))
        .unwrap();
    converse(
        &mut kernel,
        &effects,
        reply(&[1, 2], "你好！", InputOutcome::Completed),
    );
    assert_eq!(
        kernel
            .view()
            .records
            .iter()
            .filter(|record| matches!(record.body, RecordBody::InputFinished { .. }))
            .count(),
        2
    );
}
