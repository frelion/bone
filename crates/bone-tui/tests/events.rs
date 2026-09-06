use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use bone_agent::{
    AgentHandle, Autonomy, JobContext, JobOutcome, KernelConfig, ModelInput, ModelPort, Next,
    Notice, Runtime, RuntimeConfig, WorkResult,
};
use bone_tui::write_events;
use serde_json::Value;
use tokio::io::AsyncReadExt;

struct Model {
    calls: AtomicUsize,
    continuations: usize,
}

impl ModelPort for Model {
    fn infer(
        &self,
        _: ModelInput,
        _: JobContext,
    ) -> Pin<Box<dyn Future<Output = JobOutcome> + Send + 'static>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let finished = call == self.continuations;
        Box::pin(async move {
            JobOutcome::work(WorkResult {
                note: format!("step {call}"),
                autonomy: Autonomy::Run,
                next: if finished {
                    Next::Finish
                } else {
                    Next::Continue
                },
                ..Default::default()
            })
        })
    }
}

fn agent(continuations: usize) -> AgentHandle {
    Runtime::spawn(
        Arc::new(Model {
            calls: AtomicUsize::new(0),
            continuations,
        }),
        vec![],
        KernelConfig::default(),
        RuntimeConfig::default(),
    )
    .unwrap()
}

async fn finish(agent: &AgentHandle) {
    let mut notices = agent.subscribe();
    agent.post("think").await.unwrap();
    loop {
        match notices.recv().await.unwrap() {
            Notice::Finished { .. } => break,
            Notice::Error { message } => panic!("{message}"),
            _ => {}
        }
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn event_log_contains_an_ordered_baseline_work_and_shutdown() {
    let agent = agent(0);
    let observation = agent.observe().await.unwrap();
    let (writer, mut reader) = tokio::io::duplex(128);
    let writing = tokio::spawn(write_events(observation, writer));
    finish(&agent).await;
    agent.shutdown().await.unwrap();
    let mut contents = String::new();
    reader.read_to_string(&mut contents).await.unwrap();
    writing.await.unwrap().unwrap();
    let lines: Vec<Value> = contents
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines[0]["type"], "snapshot");
    assert_eq!(lines[0]["sequence"], 0);
    assert_eq!(lines.len(), 4);
    for (index, line) in lines.iter().enumerate().skip(1) {
        assert_eq!(line["type"], "step");
        assert_eq!(line["step"]["sequence"], index);
    }
    assert_eq!(lines[1]["step"]["event"]["UserMessage"]["text"], "think");
    assert!(
        lines[2]["step"]["records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["kind"].get("PlanAccepted").is_some())
    );
    assert_eq!(lines[3]["step"]["event"], "Stop");
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_blocked_writer_does_not_block_work_and_reports_any_gap() {
    let agent = agent(300);
    let observation = agent.observe().await.unwrap();
    let (writer, mut reader) = tokio::io::duplex(16);
    let writing = tokio::spawn(write_events(observation, writer));
    // No reads until the agent finishes: even the baseline blocks this writer.
    finish(&agent).await;
    assert!(!agent.snapshot().await.unwrap().autonomous);
    agent.shutdown().await.unwrap();

    let mut contents = String::new();
    reader.read_to_string(&mut contents).await.unwrap();
    writing.await.unwrap().unwrap();
    let lines: Vec<Value> = contents
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        lines
            .iter()
            .any(|line| line["type"] == "gap" && line["missed_steps"].as_u64().unwrap() > 0)
    );
    assert_eq!(lines.last().unwrap()["step"]["sequence"], 303);
    assert_eq!(lines.last().unwrap()["step"]["event"], "Stop");
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_failed_writer_does_not_terminate_the_agent() {
    let agent = agent(0);
    let (writer, reader) = tokio::io::duplex(16);
    drop(reader);
    let error = write_events(agent.observe().await.unwrap(), writer)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
    finish(&agent).await;
    assert!(agent.shutdown().await.unwrap().unresolved_jobs.is_empty());
}
