use bone_agent::Agent;
use bone_provider::{
    protocol::openai_responses,
    rig::{
        providers::openai as rig_openai,
        test_utils::{MockHttpResponse, SequencedHttpClient},
    },
};
use bone_tools::ToolEnvironment;

const START_ACTION: &str = r#"{
  "id":"resp_start","object":"response","created_at":0,"status":"completed",
  "model":"openai-test-model","usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2},
  "output":[{"type":"function_call","id":"fc_start","call_id":"call_start",
    "name":"start_action","arguments":"{\"intent\":\"Read note.txt and report its exact content\"}",
    "status":"completed"}],"tools":[]
}"#;

const READ_FILE: &str = r#"{
  "id":"resp_read","object":"response","created_at":0,"status":"completed",
  "model":"openai-test-model","usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2},
  "output":[{"type":"function_call","id":"fc_read","call_id":"call_read",
    "name":"read","arguments":"{\"path\":\"note.txt\"}","status":"completed"}],"tools":[]
}"#;

const ACTION_RESULT: &str = r#"{
  "id":"resp_action","object":"response","created_at":0,"status":"completed",
  "model":"openai-test-model","usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2},
  "output":[{"type":"message","id":"msg_action","status":"completed","role":"assistant",
    "content":[{"type":"output_text","annotations":[],"text":"note.txt contains: verified slice"}]}],
  "tools":[]
}"#;

const FINAL_REPLY: &str = r#"{
  "id":"resp_final","object":"response","created_at":0,"status":"completed",
  "model":"openai-test-model","usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2},
  "output":[{"type":"message","id":"msg_final","status":"completed","role":"assistant",
    "content":[{"type":"output_text","annotations":[],"text":"The file says: verified slice"}]}],
  "tools":[]
}"#;

const FOLLOW_UP_REPLY: &str = r#"{
  "id":"resp_followup","object":"response","created_at":0,"status":"completed",
  "model":"openai-test-model","usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2},
  "output":[{"type":"message","id":"msg_followup","status":"completed","role":"assistant",
    "content":[{"type":"output_text","annotations":[],"text":"Yes. It said: verified slice"}]}],
  "tools":[]
}"#;

#[tokio::test]
async fn real_provider_and_read_tool_cross_the_whole_agent_slice() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    std::fs::write(workspace.path().join("note.txt"), "verified slice\n")
        .expect("write test fixture");

    let transport = SequencedHttpClient::new([
        MockHttpResponse::success(START_ACTION),
        MockHttpResponse::success(READ_FILE),
        MockHttpResponse::success(ACTION_RESULT),
        MockHttpResponse::success(FINAL_REPLY),
        MockHttpResponse::success(FOLLOW_UP_REPLY),
    ]);
    let client = rig_openai::Client::builder()
        .api_key("test-only-key")
        .http_client(transport.clone())
        .build()
        .expect("test client");
    let model = openai_responses::from_client("agent-slice", client)
        .expect("test endpoint")
        .model("openai-test-model")
        .expect("test model");
    let tools = ToolEnvironment::new(workspace.path()).expect("tool environment");
    let mut agent = Agent::new(model)
        .tool(tools.read())
        .expect("register read tool");

    let reply = agent
        .chat("Read note.txt and tell me exactly what it says")
        .await
        .expect("complete agent slice");

    assert_eq!(reply.text(), "The file says: verified slice");
    assert_eq!(reply.actions().len(), 1);
    let action = &reply.actions()[0];
    assert_eq!(
        action.intent(),
        "Read note.txt and report its exact content"
    );
    assert_eq!(action.turns().len(), 2);
    let read = action.turns()[0].tools()[0].result().expect("read result");
    assert!(read.is_success());
    assert!(read.output().render().contains("verified slice"));

    let followup = agent
        .chat("Do you remember what it said?")
        .await
        .expect("continue the same conversation");
    assert_eq!(followup.text(), "Yes. It said: verified slice");
    assert!(followup.actions().is_empty());

    let requests = transport.requests();
    assert_eq!(requests.len(), 5);
    let controller: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("controller request JSON");
    let action_turn: serde_json::Value =
        serde_json::from_slice(&requests[1].body).expect("action request JSON");
    let action_followup: serde_json::Value =
        serde_json::from_slice(&requests[2].body).expect("action follow-up JSON");
    let controller_followup: serde_json::Value =
        serde_json::from_slice(&requests[3].body).expect("controller follow-up JSON");
    let next_chat: serde_json::Value =
        serde_json::from_slice(&requests[4].body).expect("next chat JSON");
    assert_eq!(controller["tools"][0]["name"], "start_action");
    assert_eq!(action_turn["tools"][0]["name"], "read");

    let action_input = action_followup["input"]
        .as_array()
        .expect("action input array");
    assert!(has_call(action_input, "function_call", "call_read"));
    assert!(has_call(action_input, "function_call_output", "call_read"));
    assert!(!has_any_call(action_input, "call_start"));

    let controller_input = controller_followup["input"]
        .as_array()
        .expect("controller input array");
    assert!(has_call(controller_input, "function_call", "call_start"));
    let outcome = controller_input
        .iter()
        .find(|item| item["type"] == "function_call_output" && item["call_id"] == "call_start")
        .expect("controller receives the action outcome");
    let outcome: serde_json::Value = serde_json::from_str(
        outcome["output"]
            .as_str()
            .expect("action outcome is JSON text"),
    )
    .expect("action outcome JSON");
    assert_eq!(outcome["status"], "completed");
    assert_eq!(outcome["output"], "note.txt contains: verified slice");
    assert!(!has_any_call(controller_input, "call_read"));

    let next_input = next_chat["input"]
        .as_array()
        .expect("next chat input array");
    assert!(has_call(next_input, "function_call", "call_start"));
    assert!(has_call(next_input, "function_call_output", "call_start"));
    assert!(!has_any_call(next_input, "call_read"));
    assert_eq!(transport.remaining_responses(), 0);
}

fn has_call(input: &[serde_json::Value], kind: &str, call_id: &str) -> bool {
    input
        .iter()
        .any(|item| item["type"] == kind && item["call_id"] == call_id)
}

fn has_any_call(input: &[serde_json::Value], call_id: &str) -> bool {
    input.iter().any(|item| item["call_id"] == call_id)
}
