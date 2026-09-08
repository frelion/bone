//! A deterministic walkthrough of routing, parallel work, and stale-result rejection.
//! Use --json to print the transitions as structured data.
use bone_agent::*;
use serde_json::{Value, json};

fn main() {
    let mut kernel = Kernel::new(KernelConfig::default(), vec![]).unwrap();
    let mut frames = vec![];
    let effects = step(
        &mut kernel,
        &mut frames,
        "接收 A，先短路由",
        Event::Input(Input::new(InputId(1), "研究 A")),
    );
    let route = model(&effects, true);
    let effects = step(
        &mut kernel,
        &mut frames,
        "Kernel 分派 A，主力拿原文",
        done(route, create("研究 A", 1)),
    );
    let first = model(&effects, false);
    let a = kernel.snapshot().jobs[0].id;
    let effects = step(
        &mut kernel,
        &mut frames,
        "A 进入后台继续深入求解",
        done(
            first,
            CallOutcome::work(WorkProposal {
                next: Next::Continue,
                ..Default::default()
            }),
        ),
    );
    let old_a = model(&effects, false);
    let effects = step(
        &mut kernel,
        &mut frames,
        "A 继续计算，B 输入交给 Kernel",
        Event::Input(Input::new(InputId(2), "独立分析 B")),
    );
    let route = model(&effects, true);
    let effects = step(
        &mut kernel,
        &mut frames,
        "B 获得完整主力，A 不取消",
        done(route, create("分析 B", 2)),
    );
    let b = model(&effects, false);
    step(
        &mut kernel,
        &mut frames,
        "B 局部交付，A 仍在运行",
        done(b, answer("B 的分析完成")),
    );
    assert_eq!(
        kernel
            .snapshot()
            .jobs
            .iter()
            .find(|job| job.id == a)
            .unwrap()
            .active_call,
        Some(old_a)
    );
    let effects = step(
        &mut kernel,
        &mut frames,
        "用户改 A 为 C",
        Event::Input(Input::new(InputId(3), "不要 A，改研究 C")),
    );
    let route = model(&effects, true);
    let held = step(
        &mut kernel,
        &mut frames,
        "旧 A 答案暂扣",
        done(old_a, answer("OBSOLETE A")),
    );
    assert!(
        !held
            .iter()
            .any(|effect| matches!(effect, Effect::Publish(Notice::Reply { .. })))
    );
    let effects = step(
        &mut kernel,
        &mut frames,
        "更新目标，撤销旧候选，分派 C",
        done(
            route,
            CallOutcome::kernel(KernelDecision {
                changes: vec![JobChange::Update {
                    job: a,
                    goal: Some("研究 C".into()),
                    action: JobAction::Keep,
                    inputs: vec![InputId(3)],
                    required: true,
                }],
                ..Default::default()
            }),
        ),
    );
    let c = model(&effects, false);
    step(
        &mut kernel,
        &mut frames,
        "C 由主力直接交付",
        done(c, answer("C 的结论是……")),
    );
    let snapshot = kernel.snapshot();
    assert!(
        snapshot
            .jobs
            .iter()
            .all(|job| job.state == JobState::Completed)
    );
    assert!(!snapshot.record.iter().any(|entry| matches!(&entry.kind, RecordKind::Notice(Notice::Reply { text, .. }) if text == "OBSOLETE A")));
    if std::env::args().any(|argument| argument == "--json") {
        println!("{}", serde_json::to_string_pretty(&frames).unwrap());
    } else {
        for (index, frame) in frames.iter().enumerate() {
            println!(
                "{}. {}；Jobs={} Calls={}",
                index + 1,
                frame["title"].as_str().unwrap(),
                frame["jobs"],
                frame["calls"]
            );
        }
        println!("验收通过：B 独立交付，A 的旧答案未发布，C 由完整主力交付。");
    }
}

fn create(goal: &str, input: u64) -> CallOutcome {
    CallOutcome::kernel(KernelDecision {
        changes: vec![JobChange::Create(JobSpec {
            goal: goal.into(),
            inputs: vec![InputId(input)],
            parent: None,
            references: vec![],
        })],
        ..Default::default()
    })
}
fn answer(text: &str) -> CallOutcome {
    CallOutcome::work(WorkProposal {
        reply: Some(text.into()),
        next: Next::Finish,
        ..Default::default()
    })
}
fn done(id: CallId, outcome: CallOutcome) -> Event {
    Event::CallFinished { id, outcome }
}
fn model(effects: &[Effect], kernel: bool) -> CallId {
    effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Start {
                id,
                call: Call::Model(input),
                ..
            } if matches!(input.task, ModelTask::Kernel { .. }) == kernel => Some(*id),
            _ => None,
        })
        .expect("the expected model call")
}
fn step(kernel: &mut Kernel, frames: &mut Vec<Value>, title: &str, event: Event) -> Vec<Effect> {
    let before = kernel.record_cursor();
    let effects = kernel.step(event.clone());
    let after = kernel.snapshot();
    frames.push(json!({"title":title,"event":event,"jobs":after.jobs.len(),"calls":after.calls.len(),"records":after.record.iter().filter(|entry| entry.cursor > before).collect::<Vec<_>>()}));
    effects
}
