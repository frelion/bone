//! Inspect the real synchronous kernel with prepared events and model results.
//! No I/O runs here. Use --json to export each input, state change, and effect.

use bone_agent::{
    Autonomy, Call, Effect, Event, InputDisposition, InputReview, JobId, JobOutcome, Kernel,
    KernelConfig, MessageId, Next, Notice, RecordKind, WorkResult,
};
use serde_json::{Value, json};

fn main() {
    let mut kernel = Kernel::new(KernelConfig::default(), vec![]).unwrap();
    // These IDs describe this fixed scenario; scheduling is performed by Kernel.
    let events = [
        ("用户要求研究 A；直接启动主力 #1", message(1, "研究 A")),
        (
            "主力仍在推理；状态询问单独交给协调 #2",
            message(2, "进度如何？"),
        ),
        (
            "协调只回复已有状态；主力 #1 不重启",
            finished(
                2,
                JobOutcome::review(InputReview {
                    disposition: InputDisposition::Keep,
                    reply: Some("A 的推理仍在进行".into()),
                    note: "这批消息只询问进度".into(),
                }),
            ),
        ),
        (
            "用户改为 B；协调 #3 解释插话",
            message(3, "不要 A，改研究 B"),
        ),
        (
            "旧主力 #1 先返回；整个答案暂存，不能抢先发布",
            finished(
                1,
                JobOutcome::work(WorkResult {
                    note: "A 的计算产物仍可作为参考材料".into(),
                    reply: Some("这是旧 A 的答案，不能发布".into()),
                    requirement: Some("研究 A".into()),
                    autonomy: Autonomy::Run,
                    next: Next::Finish,
                    ..Default::default()
                }),
            ),
        ),
        (
            "协调只要求重想；主力 #4 拿到 B 的原话",
            finished(
                3,
                JobOutcome::review(InputReview {
                    disposition: InputDisposition::Reconsider,
                    reply: None,
                    note: "用户否定了原方向；B 的方案由主力决定".into(),
                }),
            ),
        ),
        (
            "主力 #4 直接交付 B；无需协调再审批",
            finished(
                4,
                JobOutcome::work(WorkResult {
                    note: "结合原话与已有材料完成 B".into(),
                    reply: Some("B 的结论是……".into()),
                    requirement: Some("研究 B".into()),
                    autonomy: Autonomy::Run,
                    next: Next::Finish,
                    ..Default::default()
                }),
            ),
        ),
    ];

    let mut frames = Vec::new();
    for (title, event) in events {
        let before = kernel.snapshot();
        let received = event.clone();
        let effects = kernel.step(event);
        let after = kernel.snapshot();
        frames.push(json!({
            "title": title,
            "event": received,
            "record_added": &after.record[before.record.len()..],
            "before": before,
            "after": after,
            "effects": effects.iter().map(effect_json).collect::<Vec<_>>(),
        }));
    }
    let final_state = kernel.snapshot();
    assert_eq!(final_state.requirement.as_deref(), Some("研究 B"));
    assert!(!final_state.autonomous);
    assert!(
        final_state
            .record
            .iter()
            .any(|entry| matches!(entry.kind, RecordKind::WorkHeld { job: JobId(1), .. }))
    );
    assert!(!final_state.record.iter().any(|entry| matches!(&entry.kind,
        RecordKind::Notice(Notice::Reply { text, .. }) if text.contains("不能发布"))));

    if std::env::args().any(|arg| arg == "--json") {
        println!("{}", serde_json::to_string_pretty(&frames).unwrap());
    } else {
        for (index, frame) in frames.iter().enumerate() {
            println!("\n{}. {}", index + 1, frame["title"].as_str().unwrap());
            println!(
                "   主力={} 协调={} 暂存={}",
                frame["after"]["work"], frame["after"]["review"], frame["after"]["candidate"]
            );
            for effect in frame["effects"].as_array().unwrap() {
                if let Some(start) = effect.get("Start") {
                    println!("   启动 #{}：{}", start["id"], start["call"]);
                } else {
                    println!("   {effect}");
                }
            }
        }
        println!("\n验收通过：状态询问不重启主力，旧 A 没有发布，B 由主力直接交付。");
    }
}

fn message(id: u64, text: &str) -> Event {
    Event::UserMessage {
        id: MessageId(id),
        text: text.into(),
    }
}

fn finished(id: u64, outcome: JobOutcome) -> Event {
    Event::JobFinished {
        id: JobId(id),
        outcome,
    }
}

fn effect_json(effect: &Effect) -> Value {
    match effect {
        Effect::Start { id, call, timeout } => match call {
            Call::Model(input) => json!({"Start": {
                "id": id, "call": input.task, "input": input, "timeout": timeout,
            }}),
            Call::Tool(call) => json!({"Start": {
                "id": id, "call": {"Tool": call}, "timeout": timeout,
            }}),
        },
        Effect::RequestCancel { id } => json!({"RequestCancel": id}),
        Effect::WakeAfter { id, delay } => json!({"WakeAfter": {"id": id, "delay": delay}}),
        Effect::CancelWake { id } => json!({"CancelWake": id}),
        Effect::Publish(notice) => json!({"Publish": notice}),
    }
}
