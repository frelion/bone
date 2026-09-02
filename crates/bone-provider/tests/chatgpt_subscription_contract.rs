use bone_provider::{
    Protocol,
    rig::{
        completion::{FinishReason, ToolDefinition},
        message::{AssistantContent, Message, ToolChoice, ToolResultContent, UserContent},
        providers::chatgpt::{self as rig_chatgpt, ChatGPTAuth},
        test_utils::RecordingHttpClient,
    },
    service::chatgpt_subscription,
};
use serde_json::{Value, json};

const TEXT_SSE: &str = r#"data: {"type":"response.output_text.delta","delta":"ok"}
data: {"type":"response.completed","response":{"id":"resp_text_1","object":"response","created_at":1,"status":"completed","error":null,"incomplete_details":null,"instructions":null,"max_output_tokens":null,"model":"gpt-test","usage":{"input_tokens":2,"input_tokens_details":{"cached_tokens":0},"output_tokens":1,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":3},"output":[{"type":"message","id":"msg_text_1","status":"completed","role":"assistant","content":[{"type":"output_text","annotations":[],"text":"ok"}]}],"tools":[]}}
data: [DONE]"#;

const TOOL_SSE: &str = r#"data: {"type":"response.completed","response":{"id":"resp_tool_1","object":"response","created_at":1,"status":"completed","error":null,"incomplete_details":null,"instructions":null,"max_output_tokens":null,"model":"gpt-test","usage":{"input_tokens":8,"input_tokens_details":{"cached_tokens":0},"output_tokens":4,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":12},"output":[{"type":"function_call","id":"fc_test_1","call_id":"call_test_1","name":"inspect_path","arguments":"{\"path\":\"/tmp/bone\"}","status":"completed"}],"tools":[]}}
data: [DONE]"#;

fn test_client(
    body: &'static str,
) -> (
    rig_chatgpt::Client<RecordingHttpClient>,
    RecordingHttpClient,
) {
    let transport = RecordingHttpClient::new(body);
    let client = rig_chatgpt::Client::builder()
        .api_key(ChatGPTAuth::AccessToken {
            access_token: "sentinel-secret-token".to_owned(),
            account_id: Some("acct_test".to_owned()),
        })
        .base_url("https://chatgpt.example/backend-api/codex")
        .http_client(transport.clone())
        .default_instructions("")
        .originator("bone")
        .user_agent("bone-provider/test")
        .build()
        .expect("test ChatGPT client should build");
    (client, transport)
}

#[tokio::test]
async fn maps_subscription_service_to_responses_without_hiding_backend_rules() {
    let (client, transport) = test_client(TEXT_SSE);
    let debug = format!("{client:?}");
    assert!(!debug.contains("sentinel-secret-token"));

    let endpoint = chatgpt_subscription::from_client("chatgpt-test", client)
        .expect("subscription endpoint should build");
    let model = endpoint.model("gpt-test").expect("model should build");
    let response = model
        .request(Message::user("hello"))
        .preamble("Answer briefly.".to_owned())
        .max_tokens(64)
        .send()
        .await
        .expect("ChatGPT SSE should normalize");

    assert_eq!(endpoint.protocol(), Protocol::OpenAiResponses);
    assert_eq!(model.protocol(), Protocol::OpenAiResponses);
    assert_eq!(model.endpoint_id(), "chatgpt-test");
    assert_eq!(response.provider, "chatgpt");
    assert!(matches!(
        response.choice.as_slice(),
        [AssistantContent::Text(text)] if text.text == "ok"
    ));

    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(
        request.uri,
        "https://chatgpt.example/backend-api/codex/responses"
    );
    assert_eq!(
        header(request, "authorization"),
        Some("Bearer sentinel-secret-token")
    );
    assert_eq!(header(request, "chatgpt-account-id"), Some("acct_test"));
    assert_eq!(header(request, "originator"), Some("bone"));
    assert_eq!(header(request, "accept"), Some("text/event-stream"));
    assert!(header(request, "session_id").is_some());

    let body: Value = serde_json::from_slice(&request.body).expect("request body should be JSON");
    assert_eq!(body["model"], "gpt-test");
    assert_eq!(body["instructions"], "Answer briefly.");
    assert_eq!(body["stream"], true);
    assert_eq!(body["store"], false);
    assert!(body.get("max_output_tokens").is_none());
    assert!(body.get("temperature").is_none());
    assert!(body["include"].as_array().is_some_and(|values| {
        values
            .iter()
            .any(|value| value == "reasoning.encrypted_content")
    }));
}

#[tokio::test]
async fn preserves_tool_calls_and_replays_their_provider_ids() {
    let (client, transport) = test_client(TOOL_SSE);
    let model = chatgpt_subscription::from_client("chatgpt-tools", client)
        .expect("subscription endpoint should build")
        .model("gpt-test")
        .expect("model should build");

    let response = model
        .request(Message::user("inspect the path"))
        .tool(inspect_path())
        .tool_choice(ToolChoice::Specific {
            function_names: vec!["inspect_path".to_owned()],
        })
        .send()
        .await
        .expect("tool SSE should normalize");

    assert_eq!(response.finish_reason(), Some(FinishReason::ToolCalls));
    let [AssistantContent::ToolCall(tool_call)] = response.choice.as_slice() else {
        panic!("expected one tool call, got {:?}", response.choice);
    };
    assert_eq!(tool_call.function.name, "inspect_path");
    assert_eq!(tool_call.function.arguments["path"], "/tmp/bone");
    assert_eq!(
        tool_call.provider.as_ref().map(|id| id.call_id.as_str()),
        Some("call_test_1")
    );

    let first_body: Value = serde_json::from_slice(&transport.requests()[0].body)
        .expect("tool request body should be JSON");
    assert_eq!(first_body["tools"][0]["name"], "inspect_path");
    assert_eq!(first_body["tool_choice"]["name"], "inspect_path");

    let assistant = Message::Assistant {
        id: response.message_id.clone(),
        content: response.choice.clone(),
    };
    let result = UserContent::tool_result_for(
        tool_call.id.clone(),
        tool_call.provider.clone(),
        tool_call.function.name.clone(),
        vec![ToolResultContent::text("path is a directory")],
    );
    let (second_client, second_transport) = test_client(TEXT_SSE);
    let second_model = chatgpt_subscription::from_client("chatgpt-tools", second_client)
        .expect("subscription endpoint should build")
        .model("gpt-test")
        .expect("model should build");

    second_model
        .request(Message::User {
            content: vec![result],
        })
        .messages([Message::user("inspect the path"), assistant])
        .send()
        .await
        .expect("tool result replay should normalize");

    let second_body: Value = serde_json::from_slice(&second_transport.requests()[0].body)
        .expect("replay request body should be JSON");
    let input = second_body["input"]
        .as_array()
        .expect("input should be an array");
    let call = input
        .iter()
        .find(|item| item["type"] == "function_call")
        .expect("function call should be replayed");
    let output = input
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .expect("function result should be replayed");
    assert_eq!(call["id"], "fc_test_1");
    assert_eq!(call["call_id"], "call_test_1");
    assert_eq!(output["call_id"], "call_test_1");
    assert_eq!(output["output"], "path is a directory");
}

fn header<'a>(
    request: &'a bone_provider::rig::test_utils::CapturedHttpRequest,
    name: &str,
) -> Option<&'a str> {
    request
        .headers
        .get(name)
        .and_then(|value| value.to_str().ok())
}

fn inspect_path() -> ToolDefinition {
    ToolDefinition {
        name: "inspect_path".to_owned(),
        description: "Inspect one path.".to_owned(),
        parameters: json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"],
            "additionalProperties": false
        }),
    }
}
