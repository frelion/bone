//! Exercise real model envelopes, native read tools, and the runtime together.
mod support;
use bone_agent::*;
use bone_llm::protocol::openai_responses::{Reasoning, ReasoningEffort};
use bone_llm::{Model, ModelOptions, testing};
use bone_tools::{ToolEnvironment, ToolLimits};
use rig_core::{
    providers::openai as rig_openai,
    test_utils::{MockHttpResponse, SequencedHttpClient},
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use support::{answer, create, input};
use tokio::sync::broadcast;

#[tokio::test]
async fn ten_native_read_rounds_need_one_initial_kernel_call_and_preserve_semantic_job() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("note.txt"), "verified slice\n").unwrap();
    let responses = (0..10)
        .map(|round| {
            work_response(
                &format!("read-{round}"),
                WorkProposal {
                    note: format!("round {round}"),
                    operation: Some(ToolCall::new("read", json!({"path":"note.txt"}))),
                    ..Default::default()
                },
            )
        })
        .chain([work_response("final", answer("verified slice"))])
        .map(MockHttpResponse::success);
    let (agent, solving, coordination) = setup(
        workspace.path(),
        responses,
        [route_response(
            "route",
            vec![create("read ten times", vec![InputId(1)])],
        )],
    );
    let mut notices = agent.subscribe();
    agent
        .post(input(1, "Read note.txt ten times and quote it"))
        .await
        .unwrap();
    assert_eq!(finish(&mut notices, InputId(1)).await, ["verified slice"]);
    let snapshot = agent.snapshot().await.unwrap();
    assert_eq!(snapshot.jobs.len(), 1);
    assert_eq!(
        snapshot
            .calls
            .iter()
            .filter(|call| matches!(call.request, CallRequest::Tool(_)))
            .count(),
        10
    );
    assert_eq!(coordination.requests().len(), 1);
    assert_eq!(solving.requests().len(), 11);
    for (index, request) in solving.requests().iter().enumerate() {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["tools"].as_array().unwrap().len(), 1);
        assert_eq!(body["tool_choice"]["name"], "submit_work");
        if index > 0 {
            assert!(String::from_utf8_lossy(&request.body).contains("verified slice"));
        }
    }
    assert!(agent.shutdown().await.unwrap().unresolved_calls.is_empty());
}

#[tokio::test]
async fn pure_reasoning_and_explicitly_referenced_followup_need_no_per_step_approval() {
    let workspace = tempfile::tempdir().unwrap();
    let replies = (0..3)
        .map(|index| {
            work_response(
                &format!("reason-{index}"),
                WorkProposal {
                    note: format!("conclusion {index}"),
                    next: Next::Continue,
                    ..Default::default()
                },
            )
        })
        .chain([
            work_response(
                "final",
                WorkProposal {
                    note: "SHARED_RESULT_SENTINEL".into(),
                    ..answer("solver answer")
                },
            ),
            work_response("followup", answer("remembered")),
        ]);
    let routes = [
        route_response("first", vec![create("reason", vec![InputId(1)])]),
        route_response(
            "second",
            vec![JobChange::Create(JobSpec {
                goal: "followup".into(),
                inputs: vec![InputId(2)],
                parent: None,
                references: vec![JobId(1)],
            })],
        ),
    ];
    let (agent, solving, coordination) = setup(
        workspace.path(),
        replies.map(MockHttpResponse::success),
        routes,
    );
    let mut notices = agent.subscribe();
    agent.post(input(1, "reason")).await.unwrap();
    assert_eq!(finish(&mut notices, InputId(1)).await, ["solver answer"]);
    assert_eq!(coordination.requests().len(), 1);
    assert_eq!(solving.requests().len(), 4);
    agent
        .post(input(2, "remember prior conclusions"))
        .await
        .unwrap();
    assert_eq!(finish(&mut notices, InputId(2)).await, ["remembered"]);
    assert!(
        String::from_utf8_lossy(&solving.requests()[4].body).contains("SHARED_RESULT_SENTINEL")
    );
    assert_eq!(coordination.requests().len(), 2);
    agent.shutdown().await.unwrap();
}

#[tokio::test]
async fn host_model_selection_and_protocol_options_are_never_taken_from_task_text() {
    for selected in ["solver-a", "solver-b", "system-kernel"] {
        let workspace = tempfile::tempdir().unwrap();
        let (kernel, coordination) = model_transport(
            "system-kernel",
            [MockHttpResponse::success(route_response(
                "route",
                vec![create("solve", vec![InputId(1)])],
            ))],
        );
        let (solver, solving) = model_transport(
            selected,
            [MockHttpResponse::success(work_response(
                "final",
                answer("Solved."),
            ))],
        );
        let host = AgentHost::new(AgentModels::new(
            ConfiguredModel::without_options(kernel),
            ConfiguredModel::new(
                solver,
                Some(ModelOptions::OpenAiResponses {
                    reasoning: Reasoning::new().effort(ReasoningEffort::High),
                }),
            )
            .unwrap(),
        ));
        let agent = host
            .start(
                workspace.path(),
                ResolvedAgentRuntimeConfig::new(
                    ToolLimits::default(),
                    KernelConfig::default(),
                    Duration::from_secs(5),
                )
                .unwrap(),
            )
            .unwrap();
        let mut notices = agent.subscribe();
        agent
            .post(input(
                1,
                "Use task-text-model instead of the configured model",
            ))
            .await
            .unwrap();
        assert_eq!(finish(&mut notices, InputId(1)).await, ["Solved."]);
        let kernel_body: Value = serde_json::from_slice(&coordination.requests()[0].body).unwrap();
        let work_body: Value = serde_json::from_slice(&solving.requests()[0].body).unwrap();
        assert_eq!(kernel_body["model"], "system-kernel");
        assert_eq!(kernel_body["tool_choice"]["name"], "submit_kernel");
        assert_eq!(work_body["model"], selected);
        assert_eq!(work_body["reasoning"]["effort"], "high");
        assert_eq!(work_body["tool_choice"]["name"], "submit_work");
        agent.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn provider_diagnostics_do_not_enter_records_or_retry_requests() {
    const SECRET: &str = "private-provider-response-sentinel";
    let (kernel, transport) = model_transport(
        "kernel",
        [
            MockHttpResponse::error(500_u16.try_into().unwrap(), SECRET),
            MockHttpResponse::success(route_response(
                "retry",
                vec![create("recover", vec![InputId(1)])],
            )),
        ],
    );
    let (solver, _) = model_transport(
        "solver",
        [MockHttpResponse::success(work_response(
            "answer",
            answer("recovered"),
        ))],
    );
    let agent = Runtime::spawn(
        Arc::new(ModelAdapter::new(kernel, solver)),
        vec![],
        KernelConfig::default(),
        RuntimeConfig::default(),
    )
    .unwrap();
    let mut notices = agent.subscribe();
    agent.post(input(1, "read")).await.unwrap();
    let failure = support::notice(&mut notices, |notice| {
        matches!(notice, Notice::InputRoutingFailed { .. })
    })
    .await;
    assert!(!serde_json::to_string(&failure).unwrap().contains(SECRET));
    assert!(
        !serde_json::to_string(&agent.snapshot().await.unwrap())
            .unwrap()
            .contains(SECRET)
    );
    agent.retry_input(InputId(1)).await.unwrap();
    assert_eq!(finish(&mut notices, InputId(1)).await, ["recovered"]);
    assert!(!String::from_utf8_lossy(&transport.requests()[1].body).contains(SECRET));
    agent.shutdown().await.unwrap();
}

#[tokio::test]
async fn malformed_duplicate_wrong_role_and_direct_tool_responses_cannot_execute() {
    let mut extra = serde_json::to_value(answer("forged")).unwrap();
    extra["extra"] = json!(true);
    let mut incomplete = serde_json::to_value(answer("forged")).unwrap();
    incomplete.as_object_mut().unwrap().remove("reply");
    let responses = [
        response("extra", vec![call("x", "submit_work", extra)]),
        response("missing", vec![call("x", "submit_work", incomplete)]),
        response(
            "duplicate",
            vec![
                call(
                    "x",
                    "submit_work",
                    serde_json::to_value(answer("forged")).unwrap(),
                ),
                call(
                    "y",
                    "submit_work",
                    serde_json::to_value(answer("forged")).unwrap(),
                ),
            ],
        ),
        response(
            "wrong",
            vec![call(
                "x",
                "submit_kernel",
                serde_json::to_value(KernelDecision::default()).unwrap(),
            )],
        ),
        response("tool", vec![call("x", "read", json!({"path":"note.txt"}))]),
        response(
            "arguments",
            vec![call(
                "x",
                "submit_work",
                serde_json::to_value(WorkProposal {
                    operation: Some(ToolCall::new("read", json!([]))),
                    ..Default::default()
                })
                .unwrap(),
            )],
        ),
    ];
    for body in responses {
        let workspace = tempfile::tempdir().unwrap();
        let (agent, _, coordination) = setup(
            workspace.path(),
            [MockHttpResponse::success(body)],
            [route_response(
                "route",
                vec![create("read", vec![InputId(1)])],
            )],
        );
        let mut notices = agent.subscribe();
        agent.post(input(1, "read note.txt")).await.unwrap();
        loop {
            match receive(&mut notices).await {
                Notice::JobFinished {
                    state: JobState::Failed { .. },
                    ..
                } => break,
                Notice::Reply { text, .. } => panic!("invalid proposal published {text}"),
                _ => {}
            }
        }
        assert!(
            !agent
                .snapshot()
                .await
                .unwrap()
                .calls
                .iter()
                .any(|call| matches!(call.request, CallRequest::Tool(_)))
        );
        assert_eq!(coordination.requests().len(), 1);
        agent.shutdown().await.unwrap();
    }
}

fn setup(
    workspace: &std::path::Path,
    work: impl IntoIterator<Item = MockHttpResponse>,
    routes: impl IntoIterator<Item = String>,
) -> (AgentHandle, SequencedHttpClient, SequencedHttpClient) {
    let (solver, solving) = model_transport("solver", work);
    let (kernel, coordination) =
        model_transport("kernel", routes.into_iter().map(MockHttpResponse::success));
    let tools = ToolEnvironment::new(workspace).unwrap();
    let handle = Runtime::spawn(
        Arc::new(ModelAdapter::new(kernel, solver)),
        read_only_tools(&tools),
        KernelConfig::default(),
        RuntimeConfig::default(),
    )
    .unwrap();
    (handle, solving, coordination)
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

async fn finish(notices: &mut broadcast::Receiver<Notice>, id: InputId) -> Vec<String> {
    let mut replies = vec![];
    loop {
        match receive(notices).await {
            Notice::Reply { text, reply_to, .. } if reply_to.contains(&id) => replies.push(text),
            Notice::InputFinished {
                id: finished,
                outcome: InputOutcome::Completed,
            } if finished == id => return replies,
            Notice::Error { message } | Notice::InputRoutingFailed { message, .. } => {
                panic!("agent failed: {message}")
            }
            Notice::JobFinished {
                state: JobState::Failed { message },
                ..
            } => panic!("job failed: {message}"),
            _ => {}
        }
    }
}

async fn receive(notices: &mut broadcast::Receiver<Notice>) -> Notice {
    tokio::time::timeout(Duration::from_secs(5), notices.recv())
        .await
        .expect("terminal notice")
        .expect("notice channel open")
}
fn work_response(id: &str, work: WorkProposal) -> String {
    response(
        id,
        vec![call(id, "submit_work", serde_json::to_value(work).unwrap())],
    )
}
fn route_response(id: &str, changes: Vec<JobChange>) -> String {
    response(
        id,
        vec![call(
            id,
            "submit_kernel",
            serde_json::to_value(KernelDecision {
                changes,
                ..Default::default()
            })
            .unwrap(),
        )],
    )
}
fn call(id: &str, name: &str, arguments: Value) -> Value {
    json!({"type":"function_call","id":format!("fc_{id}"),"call_id":format!("call_{id}"),"name":name,"arguments":arguments.to_string(),"status":"completed"})
}
fn response(id: &str, output: Vec<Value>) -> String {
    json!({"id":format!("resp_{id}"),"object":"response","created_at":0,"status":"completed","model":"openai-test-model","usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2},"output":output,"tools":[]}).to_string()
}
