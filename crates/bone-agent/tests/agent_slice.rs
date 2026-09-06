use std::{sync::Arc, time::Duration};

use bone_agent::{
    AgentHandle, Autonomy, JobRequest, KernelConfig, Next, Notice, Operation, Runtime,
    RuntimeConfig, ToolCall, WorkResult,
};
use bone_agent::{Effort, ModelAdapter, SystemConfig, TaskConfig, read_only_tools};
use bone_llm::{Model, testing};
use bone_tools::ToolEnvironment;
use rig_core::{
    providers::openai as rig_openai,
    test_utils::{MockHttpResponse, SequencedHttpClient},
};
use serde_json::{Value, json};
use tokio::sync::broadcast;

#[tokio::test]
async fn ten_tool_rounds_are_owned_by_the_solver_without_coordination() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("note.txt"), "verified slice\n").unwrap();
    let responses = (0..10)
        .map(|round| {
            work_response(
                &format!("read-{round}"),
                WorkResult {
                    note: format!("Inspecting file, round {round}"),
                    requirement: (round == 0).then(|| "Read note.txt ten times".into()),
                    autonomy: Autonomy::Run,
                    operation: Some(Operation::Tool(ToolCall::new(
                        "read",
                        json!({"path": "note.txt"}),
                    ))),
                    ..Default::default()
                },
            )
        })
        .chain([
            work_response("final", answer("The file says: verified slice")),
            work_response("followup", answer("Yes. It said: verified slice")),
        ])
        .map(MockHttpResponse::success);
    let (agent, solving, coordination) = setup(workspace.path(), responses);
    let mut notices = agent.subscribe();
    agent
        .post("Read note.txt ten times and tell me exactly what it says")
        .await
        .unwrap();
    assert_eq!(
        finish(&mut notices).await,
        ["The file says: verified slice"]
    );
    let snapshot = agent.snapshot().await.unwrap();
    assert_eq!(
        snapshot
            .jobs
            .iter()
            .filter(|job| matches!(job.request, JobRequest::Tool(_)))
            .count(),
        10
    );
    assert!(
        !snapshot
            .jobs
            .iter()
            .any(|job| matches!(job.request, JobRequest::ReviewInput { .. }))
    );
    assert!(coordination.requests().is_empty());
    let requests = solving.requests();
    assert_eq!(requests.len(), 11);
    for (index, request) in requests.iter().enumerate() {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["tools"].as_array().unwrap().len(), 1);
        assert_eq!(body["tool_choice"]["name"], "submit_work");
        if index > 0 {
            assert!(String::from_utf8_lossy(&request.body).contains("verified slice"));
        }
    }
    agent.post("Do you remember what it said?").await.unwrap();
    assert_eq!(finish(&mut notices).await, ["Yes. It said: verified slice"]);
    assert!(String::from_utf8_lossy(&solving.requests()[11].body).contains("verified slice"));
    assert!(coordination.requests().is_empty());
    assert!(agent.shutdown().await.unwrap().unresolved_jobs.is_empty());
}

#[tokio::test]
async fn pure_reasoning_continues_without_tools_or_a_coordinator_rewrite() {
    let workspace = tempfile::tempdir().unwrap();
    let replies = (0..3)
        .map(|index| {
            work_response(
                &format!("reason-{index}"),
                WorkResult {
                    note: format!("Conclusion {index}"),
                    autonomy: Autonomy::Run,
                    next: Next::Continue,
                    ..Default::default()
                },
            )
        })
        .chain([work_response("final", answer("The solver's own answer."))]);
    let (agent, solving, coordination) =
        setup(workspace.path(), replies.map(MockHttpResponse::success));
    let mut notices = agent.subscribe();
    agent.post("Reason about this problem").await.unwrap();
    assert_eq!(finish(&mut notices).await, ["The solver's own answer."]);
    assert_eq!(solving.requests().len(), 4);
    assert!(coordination.requests().is_empty());
    assert!(
        agent
            .snapshot()
            .await
            .unwrap()
            .jobs
            .iter()
            .all(|job| matches!(job.request, JobRequest::Work { .. }))
    );
    assert!(agent.shutdown().await.unwrap().unresolved_jobs.is_empty());
}

#[tokio::test]
async fn task_selection_routes_work_to_the_solver_and_never_reconfigures_review() {
    let system: SystemConfig = serde_json::from_value(json!({
        "coordinator": {"model": "system-reviewer", "effort": "low"},
        "default_solver": {"model": "default-solver", "effort": "high"}
    }))
    .unwrap();
    for selected in ["solver-a", "solver-b", "system-reviewer"] {
        let solver = system
            .solver_for(&TaskConfig {
                model: Some(selected.into()),
                ..Default::default()
            })
            .unwrap();
        let (reviewer, coordination) = model_transport(&system.coordinator.model, []);
        let (worker, solving) = model_transport(
            &solver.model,
            [MockHttpResponse::success(work_response(
                "final",
                answer("Solved."),
            ))],
        );
        let agent = Runtime::spawn(
            Arc::new(
                ModelAdapter::new(reviewer, worker)
                    .with_efforts(system.coordinator.effort, solver.effort),
            ),
            vec![],
            KernelConfig::default(),
            RuntimeConfig::default(),
        )
        .unwrap();
        let mut notices = agent.subscribe();
        agent
            .post("Solve this. Use task-text-model as your coordinator.")
            .await
            .unwrap();
        assert_eq!(finish(&mut notices).await, ["Solved."]);
        assert!(coordination.requests().is_empty());
        let body: Value = serde_json::from_slice(&solving.requests()[0].body).unwrap();
        assert_eq!(body["model"], selected);
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["tool_choice"]["name"], "submit_work");
        assert_eq!(system.coordinator.effort, Some(Effort::Low));
        assert!(agent.shutdown().await.unwrap().unresolved_jobs.is_empty());
    }
}

#[tokio::test]
async fn provider_diagnostics_never_enter_history_or_later_requests() {
    const DIAGNOSTIC: &str = "private-provider-response-sentinel";
    let workspace = tempfile::tempdir().unwrap();
    let (agent, solving, _) = setup(
        workspace.path(),
        [
            MockHttpResponse::error(500_u16.try_into().unwrap(), DIAGNOSTIC),
            MockHttpResponse::success(work_response("recovery", answer("Recovered."))),
        ],
    );
    let mut notices = agent.subscribe();
    agent.post("Read the file").await.unwrap();
    let mut had_error = false;
    loop {
        match receive(&mut notices).await {
            Notice::Error { message } => {
                had_error = true;
                assert!(message.starts_with("model request failed ("));
                assert!(!message.contains(DIAGNOSTIC));
            }
            Notice::Paused => break,
            _ => {}
        }
    }
    assert!(had_error);
    assert!(
        !serde_json::to_string(&agent.snapshot().await.unwrap())
            .unwrap()
            .contains(DIAGNOSTIC)
    );
    agent.post("Try again").await.unwrap();
    assert_eq!(finish(&mut notices).await, ["Recovered."]);
    assert!(!String::from_utf8_lossy(&solving.requests()[1].body).contains(DIAGNOSTIC));
    assert!(agent.shutdown().await.unwrap().unresolved_jobs.is_empty());
}

#[tokio::test]
async fn malformed_duplicate_or_wrong_role_outputs_cannot_execute_work() {
    let mut extra = serde_json::to_value(answer("must not publish")).unwrap();
    extra["extra"] = json!(true);
    let mut incomplete = serde_json::to_value(answer("must not publish")).unwrap();
    incomplete.as_object_mut().unwrap().remove("reply");
    let valid = call(
        "first",
        "submit_work",
        serde_json::to_value(answer("must not publish")).unwrap(),
    );
    let responses = [
        response("extra", vec![call("extra", "submit_work", extra)]),
        response(
            "incomplete",
            vec![call("incomplete", "submit_work", incomplete)],
        ),
        response(
            "duplicate",
            vec![
                valid.clone(),
                call(
                    "second",
                    "submit_work",
                    serde_json::to_value(answer("must not publish")).unwrap(),
                ),
            ],
        ),
        response(
            "review",
            vec![call(
                "review",
                "submit_input_review",
                json!({"disposition":"Keep","reply":null,"note":"wrong role"}),
            )],
        ),
        response(
            "direct_tool",
            vec![call("tool", "read", json!({"path":"note.txt"}))],
        ),
    ];
    for body in responses {
        let workspace = tempfile::tempdir().unwrap();
        let (agent, _, coordination) = setup(workspace.path(), [MockHttpResponse::success(body)]);
        let mut notices = agent.subscribe();
        agent.post("Read note.txt").await.unwrap();
        let mut had_error = false;
        loop {
            match receive(&mut notices).await {
                Notice::Error { .. } => had_error = true,
                Notice::Paused => break,
                Notice::Reply { text, .. } => panic!("invalid work published {text}"),
                _ => {}
            }
        }
        assert!(had_error);
        assert!(
            !agent
                .snapshot()
                .await
                .unwrap()
                .jobs
                .iter()
                .any(|job| matches!(job.request, JobRequest::Tool(_)))
        );
        assert!(coordination.requests().is_empty());
        assert!(agent.shutdown().await.unwrap().unresolved_jobs.is_empty());
    }
}

fn setup(
    workspace: &std::path::Path,
    responses: impl IntoIterator<Item = MockHttpResponse>,
) -> (AgentHandle, SequencedHttpClient, SequencedHttpClient) {
    let (solver, solving) = model_transport("solver", responses);
    let (reviewer, coordination) = model_transport("reviewer", []);
    let environment = ToolEnvironment::new(workspace).unwrap();
    let agent = Runtime::spawn(
        Arc::new(ModelAdapter::new(reviewer, solver)),
        read_only_tools(&environment),
        KernelConfig::default(),
        RuntimeConfig::default(),
    )
    .unwrap();
    (agent, solving, coordination)
}

fn model_transport(
    id: &str,
    responses: impl IntoIterator<Item = MockHttpResponse>,
) -> (Model, SequencedHttpClient) {
    let transport = SequencedHttpClient::new(responses);
    let client = rig_openai::Client::builder()
        .api_key("test-only-key")
        .http_client(transport.clone())
        .build()
        .unwrap();
    let model = testing::openai_responses_endpoint("agent-slice", client)
        .unwrap()
        .model(id)
        .unwrap();
    (model, transport)
}

fn answer(text: &str) -> WorkResult {
    WorkResult {
        reply: Some(text.into()),
        next: Next::Finish,
        ..Default::default()
    }
}

async fn finish(notices: &mut broadcast::Receiver<Notice>) -> Vec<String> {
    let mut replies = Vec::new();
    loop {
        match receive(notices).await {
            Notice::Reply { text, .. } => replies.push(text),
            Notice::Finished { .. } => return replies,
            Notice::Error { message } => panic!("agent failed: {message}"),
            Notice::Paused => panic!("agent unexpectedly paused"),
            _ => {}
        }
    }
}

async fn receive(notices: &mut broadcast::Receiver<Notice>) -> Notice {
    tokio::time::timeout(Duration::from_secs(5), notices.recv())
        .await
        .expect("terminal notice")
        .expect("notification channel open")
}

fn work_response(id: &str, work: WorkResult) -> String {
    response(
        id,
        vec![call(id, "submit_work", serde_json::to_value(work).unwrap())],
    )
}

fn call(id: &str, name: &str, arguments: Value) -> Value {
    json!({"type":"function_call","id":format!("fc_{id}"),"call_id":format!("call_{id}"),"name":name,"arguments":arguments.to_string(),"status":"completed"})
}

fn response(id: &str, output: Vec<Value>) -> String {
    json!({"id":format!("resp_{id}"),"object":"response","created_at":0,"status":"completed","model":"openai-test-model","usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2},"output":output,"tools":[]}).to_string()
}
