//! Real Runtime with controlled futures and Tokio's virtual clock.
use bone_agent::*;
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use std::{future::pending, sync::Arc, time::Duration};
use tokio::sync::{Notify, broadcast, mpsc, oneshot};

struct PendingCall {
    input: ModelInput,
    reply: oneshot::Sender<CallOutcome>,
}
struct ControlledModel(mpsc::UnboundedSender<PendingCall>);
impl ModelPort for ControlledModel {
    fn infer(&self, input: ModelInput, _: CallContext) -> BoxFuture<'static, CallOutcome> {
        let (reply, response) = oneshot::channel();
        self.0.send(PendingCall { input, reply }).unwrap();
        Box::pin(async move { response.await.expect("prepared model reply") })
    }
}
struct StuckLookup(Arc<Notify>);
impl ToolPort for StuckLookup {
    fn specification(&self) -> ToolSpec {
        ToolSpec {
            name: "lookup".into(),
            description: "controlled read that never returns".into(),
            parameters: json!({"type":"object"}),
            effect: ToolEffect::ReadOnly,
        }
    }
    fn run(&self, _: Value, context: CallContext) -> BoxFuture<'static, CallOutcome> {
        let started = self.0.clone();
        Box::pin(async move {
            context.report_progress(CallProgress {
                message: "查询仍在运行".into(),
                percent: None,
            });
            started.notify_one();
            pending().await
        })
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    tokio::time::pause();
    let (sender, mut calls) = mpsc::unbounded_channel();
    let started = Arc::new(Notify::new());
    let agent = Runtime::spawn(
        Arc::new(ControlledModel(sender)),
        vec![Arc::new(StuckLookup(started.clone()))],
        KernelConfig::default(),
        RuntimeConfig::default(),
    )
    .unwrap();
    let mut notices = agent.subscribe();
    agent.post(Input::new(InputId(1), "研究 A")).await.unwrap();
    route(next(&mut calls).await, "研究 A", InputId(1));
    let first = next(&mut calls).await;
    let a = job(&first);
    first
        .reply
        .send(CallOutcome::work(WorkProposal {
            operation: Some(ToolCall::new("lookup", json!({}))),
            next: Next::Continue,
            ..Default::default()
        }))
        .unwrap();
    started.notified().await;
    let old = next(&mut calls).await;
    agent
        .post(Input::new(InputId(2), "独立完成 B"))
        .await
        .unwrap();
    route(next(&mut calls).await, "完成 B", InputId(2));
    let b = next(&mut calls).await;
    b.reply.send(answer("B 已完成")).unwrap();
    delivered(&mut notices, InputId(2)).await;
    assert!(!old.reply.is_closed());
    println!("B 已交付；A 的模型和只读查询仍在运行。");

    agent
        .post(Input::new(InputId(3), "不要 A，改为 C，七秒后再看"))
        .await
        .unwrap();
    let routing = next(&mut calls).await;
    old.reply.send(answer("OBSOLETE A")).unwrap();
    routing
        .reply
        .send(CallOutcome::kernel(KernelDecision {
            changes: vec![JobChange::Update {
                job: a,
                goal: Some("C".into()),
                action: JobAction::Keep,
                inputs: vec![InputId(3)],
                required: true,
            }],
            ..Default::default()
        }))
        .unwrap();
    let c = next(&mut calls).await;
    c.reply
        .send(CallOutcome::work(WorkProposal {
            next: Next::Wait {
                reconsider_after: Some(Duration::from_secs(7)),
            },
            ..Default::default()
        }))
        .unwrap();
    // Snapshot commands are ordered with commands; wait for the actual model completion.
    loop {
        if matches!(notices.recv().await.unwrap(), Notice::JobChanged { job } if job.id == a && job.state == JobState::Waiting(WaitReason::Timer))
        {
            break;
        }
    }
    tokio::time::advance(Duration::from_secs(7)).await;
    let c = next(&mut calls).await;
    assert_eq!(job(&c), a);
    c.reply.send(answer("C 的结果")).unwrap();
    delivered(&mut notices, InputId(3)).await;
    assert!(!agent.snapshot().await.unwrap().record.iter().any(|entry| matches!(&entry.kind, RecordKind::Notice(Notice::Reply { text, .. }) if text == "OBSOLETE A")));
    assert!(agent.shutdown().await.unwrap().unresolved_calls.is_empty());
    println!("C 被定时唤醒并交付；旧 A 未发布，遗留只读等待已清理。");
}
fn job(call: &PendingCall) -> JobId {
    match call.input.task {
        ModelTask::Work { job, .. } => job,
        _ => panic!("expected worker"),
    }
}
fn route(call: PendingCall, goal: &str, input: InputId) {
    assert!(matches!(call.input.task, ModelTask::Kernel { .. }));
    call.reply
        .send(CallOutcome::kernel(KernelDecision {
            changes: vec![JobChange::Create(JobSpec {
                goal: goal.into(),
                inputs: vec![input],
                parent: None,
                references: vec![],
            })],
            ..Default::default()
        }))
        .unwrap();
}
fn answer(text: &str) -> CallOutcome {
    CallOutcome::work(WorkProposal {
        reply: Some(text.into()),
        next: Next::Finish,
        ..Default::default()
    })
}
async fn next(calls: &mut mpsc::UnboundedReceiver<PendingCall>) -> PendingCall {
    tokio::time::timeout(Duration::from_secs(2), calls.recv())
        .await
        .expect("next independent call")
        .expect("model open")
}
async fn delivered(notices: &mut broadcast::Receiver<Notice>, id: InputId) {
    loop {
        match notices.recv().await.unwrap() {
            Notice::InputFinished {
                id: finished,
                outcome: InputOutcome::Completed,
            } if finished == id => return,
            Notice::Error { message } | Notice::InputRoutingFailed { message, .. } => {
                panic!("{message}")
            }
            _ => {}
        }
    }
}
