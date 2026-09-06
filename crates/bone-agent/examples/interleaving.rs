//! Run the real Runtime with controlled ports and Tokio's virtual clock.
//! The model responses below are prepared data; the kernel decides what runs.

use std::{future::pending, sync::Arc, time::Duration};

use bone_agent::{
    Autonomy, EffectSummary, InputDisposition, InputReview, JobContext, JobOutcome, JobProgress,
    JobRequest, KernelConfig, ModelInput, ModelPort, ModelTask, Next, Notice, Operation,
    RecordKind, Runtime, RuntimeConfig, StepEvent, ToolCall, ToolEffect, ToolPort, ToolSpec,
    WorkResult,
};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::{Notify, broadcast, mpsc, oneshot};

struct PendingCall {
    input: ModelInput,
    respond: oneshot::Sender<JobOutcome>,
}

struct ControlledModel(mpsc::UnboundedSender<PendingCall>);

impl ModelPort for ControlledModel {
    fn infer(&self, input: ModelInput, _: JobContext) -> BoxFuture<'static, JobOutcome> {
        let (respond, result) = oneshot::channel();
        self.0.send(PendingCall { input, respond }).unwrap();
        Box::pin(async move { result.await.expect("a prepared model response") })
    }
}

/// This read never returns and does not check cancellation. Runtime must end
/// the local wait itself; that does not prove a remote service stopped working.
struct StuckLookup(Arc<Notify>);

impl ToolPort for StuckLookup {
    fn specification(&self) -> ToolSpec {
        ToolSpec {
            name: "lookup".into(),
            description: "A controlled read that never returns".into(),
            parameters: json!({"type": "object"}),
            effect: ToolEffect::ReadOnly,
        }
    }

    fn run(&self, _: Value, context: JobContext) -> BoxFuture<'static, JobOutcome> {
        let started = self.0.clone();
        Box::pin(async move {
            context.report_progress(JobProgress {
                message: "A 的查询仍未返回".into(),
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
    let (calls, mut requests) = mpsc::unbounded_channel();
    let tool_started = Arc::new(Notify::new());
    let agent = Runtime::spawn(
        Arc::new(ControlledModel(calls)),
        vec![Arc::new(StuckLookup(tool_started.clone()))],
        KernelConfig::default(),
        RuntimeConfig::default(),
    )
    .unwrap();
    let mut events = agent.observe().await.unwrap().events;
    let mut trace = agent.observe().await.unwrap().events;
    let printer = tokio::spawn(async move {
        while let Ok(step) = trace.recv().await {
            print_step(&step);
        }
    });

    agent.post("研究 A，需要时查询资料").await.unwrap();
    let initial = next_call(&mut requests, false).await;
    initial
        .respond
        .send(JobOutcome::work(WorkResult {
            requirement: Some("研究 A".into()),
            autonomy: Autonomy::Run,
            operation: Some(Operation::Tool(ToolCall::new(
                "lookup",
                json!({"topic": "A"}),
            ))),
            ..Default::default()
        }))
        .unwrap();
    tool_started.notified().await;

    println!("\n时钟推进 30 秒：工具仍不返回，主力获得重新考虑的机会。");
    tokio::time::advance(Duration::from_secs(30)).await;
    let original = next_call(&mut requests, false).await;
    assert!(
        original
            .input
            .snapshot
            .record
            .iter()
            .any(|entry| matches!(entry.kind, RecordKind::Reminder { .. }))
    );
    let original_id = agent.snapshot().await.unwrap().work.unwrap();

    agent.post("进度如何？保持当前工作").await.unwrap();
    next_call(&mut requests, true)
        .await
        .respond
        .send(JobOutcome::review(InputReview {
            disposition: InputDisposition::Keep,
            reply: Some("查询尚未返回，主力正在重新评估；原推理继续".into()),
            note: "只询问已有状态".into(),
        }))
        .unwrap();
    until(&mut events, |step| {
        step.records.iter().any(|entry| {
            matches!(
                entry.kind,
                RecordKind::InputReviewed {
                    disposition: InputDisposition::Keep,
                    ..
                }
            )
        })
    })
    .await;
    assert_eq!(agent.snapshot().await.unwrap().work, Some(original_id));

    agent
        .post("不要 A，改为 B。现有信息已经足够回答 B")
        .await
        .unwrap();
    let review = next_call(&mut requests, true).await;
    // Return the old proposal while the change is still being interpreted.
    original
        .respond
        .send(JobOutcome::work(WorkResult {
            note: "这是旧 A 的计算材料".into(),
            reply: Some("旧 A 答案：不应发布".into()),
            operation: Some(Operation::Tool(ToolCall::new(
                "lookup",
                json!({"topic": "obsolete A"}),
            ))),
            next: Next::Continue,
            ..Default::default()
        }))
        .unwrap();
    until(&mut events, |step| {
        step.records.iter().any(|entry| {
            matches!(entry.kind,
        RecordKind::WorkHeld { job, .. } if job == original_id)
        })
    })
    .await;
    assert_eq!(agent.snapshot().await.unwrap().candidate, Some(original_id));

    review
        .respond
        .send(JobOutcome::review(InputReview {
            disposition: InputDisposition::Reconsider,
            reply: None,
            note: "用户否定了 A；把原话交回主力，不替它制定 B".into(),
        }))
        .unwrap();
    let replacement = next_call(&mut requests, false).await;
    assert!(
        matches!(&replacement.input.task, ModelTask::Work { messages }
        if messages.iter().any(|message| message.text.contains("改为 B")))
    );
    replacement
        .respond
        .send(JobOutcome::work(WorkResult {
            note: "主力根据新要求选择 B，旧查询可以放弃".into(),
            requirement: Some("研究 B".into()),
            reply: Some("B 已完成；不再等待旧 A 查询".into()),
            autonomy: Autonomy::Run,
            next: Next::Finish,
            ..Default::default()
        }))
        .unwrap();
    until(&mut events, |step| {
        step.effects
            .iter()
            .any(|effect| matches!(effect, EffectSummary::Publish(Notice::Finished { .. })))
    })
    .await;

    let snapshot = agent.snapshot().await.unwrap();
    assert_eq!(snapshot.requirement.as_deref(), Some("研究 B"));
    assert_eq!(
        snapshot
            .jobs
            .iter()
            .filter(|job| matches!(job.request, JobRequest::Work { .. }))
            .count(),
        3
    );
    assert_eq!(
        snapshot
            .jobs
            .iter()
            .filter(|job| matches!(job.request, JobRequest::ReviewInput { .. }))
            .count(),
        2
    );
    assert_eq!(
        snapshot
            .jobs
            .iter()
            .filter(|job| matches!(job.request, JobRequest::Tool(_)))
            .count(),
        1
    );
    assert!(!snapshot.record.iter().any(|entry| matches!(&entry.kind,
        RecordKind::Notice(Notice::Reply { text, .. }) if text.contains("不应发布"))));
    assert!(!snapshot.autonomous);
    assert!(agent.shutdown().await.unwrap().unresolved_jobs.is_empty());
    printer.await.unwrap();
    println!(
        "\n验收通过：软提醒直接唤醒主力，询问进度不重启，旧建议被撤销，B 由主力交付。\n卡住的只读查询已结束本地等待；全程没有真实等待 30 秒。"
    );
}

async fn next_call(
    requests: &mut mpsc::UnboundedReceiver<PendingCall>,
    review: bool,
) -> PendingCall {
    let call = tokio::time::timeout(Duration::from_secs(5), requests.recv())
        .await
        .expect("the kernel should start a model call")
        .unwrap();
    assert_eq!(
        matches!(call.input.task, ModelTask::ReviewInput { .. }),
        review
    );
    call
}

async fn until(
    events: &mut broadcast::Receiver<Arc<StepEvent>>,
    predicate: impl Fn(&StepEvent) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = events.recv().await.expect("the observed runtime is open");
            if predicate(&event) {
                return;
            }
        }
    })
    .await
    .expect("the expected kernel step should occur");
}

fn print_step(step: &StepEvent) {
    let time = step.elapsed.as_secs();
    for entry in &step.records {
        match &entry.kind {
            RecordKind::UserMessage(message) => println!("[{time:>2}s] 用户：{}", message.text),
            RecordKind::WorkHeld { job, .. } => {
                println!("[{time:>2}s] 主力 #{} 结果暂存：插话尚未解释", job.0)
            }
            RecordKind::PlanDiscarded { job, reason } => {
                println!("[{time:>2}s] 撤销 #{}：{reason}", job.0)
            }
            RecordKind::InputReviewed {
                disposition, note, ..
            } => println!("[{time:>2}s] 协调：{disposition:?}；{note}"),
            _ => {}
        }
    }
    for effect in &step.effects {
        match effect {
            EffectSummary::Start { id, request, .. } => {
                let purpose = match request {
                    JobRequest::Work { .. } => "主力",
                    JobRequest::ReviewInput { .. } => "协调",
                    JobRequest::Tool(_) => "工具",
                };
                println!("[{time:>2}s] 启动 {purpose} #{}", id.0);
            }
            EffectSummary::RequestCancel { id } => println!("[{time:>2}s] 请求取消 #{}", id.0),
            EffectSummary::Publish(Notice::Reply { text, .. }) => {
                println!("[{time:>2}s] 回复：{text}")
            }
            EffectSummary::Publish(Notice::Finished { cleanup }) => {
                println!("[{time:>2}s] 任务交付；待清理：{cleanup:?}")
            }
            EffectSummary::Publish(Notice::Error { message }) => {
                println!("[{time:>2}s] 错误：{message}")
            }
            _ => {}
        }
    }
}
