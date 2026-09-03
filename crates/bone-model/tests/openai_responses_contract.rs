mod support;

use bone_model::{
    Protocol, StreamedAssistantContent,
    protocol::openai_responses::{
        self, Reasoning, ReasoningEffort, ReasoningSummaryLevel, reasoning_params,
    },
    rig::{
        completion::{CompletionError, FinishReason, ToolDefinition},
        message::{
            AssistantContent, Message, ReasoningContent, ToolChoice, ToolResultContent, UserContent,
        },
        providers::openai as rig_openai,
    },
};
use futures_util::StreamExt;
use serde_json::Value;
use support::transport::ScriptedHttpClient;

const TEXT_RESPONSE: &str = include_str!("fixtures/openai_responses/text_response.json");
const TEXT_STREAM: &str = include_str!("fixtures/openai_responses/text_stream.sse");
const TOOL_RESPONSE: &str = include_str!("fixtures/openai_responses/tool_response.json");
const REASONING_RESPONSE: &str = include_str!("fixtures/openai_responses/reasoning_response.json");
const TRUNCATED_STREAM: &str = include_str!("fixtures/openai_responses/truncated_stream.sse");
const ERROR_RESPONSE: &str = include_str!("fixtures/openai_responses/error_response.json");

#[tokio::test]
async fn sends_responses_wire_shape_and_preserves_endpoint_identity() {
    let transport = ScriptedHttpClient::unary_json(TEXT_RESPONSE);
    let client = rig_openai::Client::builder()
        .api_key("test-only-key")
        .base_url("https://gateway.example/v1")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let endpoint =
        openai_responses::from_client("openai-test", client).expect("endpoint should build");
    let model = endpoint
        .model("openai-test-model")
        .expect("model should build");

    let response = model
        .request(Message::user("hello"))
        .preamble("Answer briefly.".to_owned())
        .max_tokens(64)
        .send()
        .await
        .expect("fixture should parse");

    assert_eq!(endpoint.id(), "openai-test");
    assert_eq!(endpoint.protocol(), Protocol::OpenAiResponses);
    assert_eq!(model.endpoint_id(), "openai-test");
    assert_eq!(model.protocol(), Protocol::OpenAiResponses);
    assert_eq!(model.id(), "openai-test-model");
    assert_eq!(response.provider, "openai");
    assert_eq!(response.message_id.as_deref(), Some("msg_test_1"));
    assert!(matches!(
        response.choice.as_slice(),
        [AssistantContent::Text(text)] if text.text == "Hello from OpenAI Responses."
    ));
    assert_eq!(response.usage.input_tokens, 8);
    assert_eq!(response.usage.output_tokens, 5);

    let metadata = transport.requests();
    let request_metadata = metadata.first().expect("one request should be captured");
    assert_eq!(metadata.len(), 1);
    assert_eq!(request_metadata.method.as_str(), "POST");
    assert_eq!(request_metadata.uri, "https://gateway.example/v1/responses");
    assert!(!request_metadata.uri.ends_with("/chat/completions"));
    assert_eq!(
        request_metadata
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer test-only-key")
    );
    assert_eq!(
        request_metadata
            .headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );

    let requests = transport.unary_requests();
    let request = requests
        .first()
        .expect("one unary request should be captured");
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_slice(&request.body).expect("request body should be JSON");
    assert_eq!(body["model"], "openai-test-model");
    assert_eq!(body["max_output_tokens"], 64);
    assert_eq!(body["instructions"], "Answer briefly.");
    assert_eq!(body["input"][0]["role"], "user");
    assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
    assert_eq!(body["input"][0]["content"][0]["text"], "hello");
}

#[tokio::test]
async fn maps_tools_and_replays_the_provider_call_id_with_the_function_result() {
    let transport = ScriptedHttpClient::unary_json(TOOL_RESPONSE);
    let client = rig_openai::Client::builder()
        .api_key("test-only-key")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let model = openai_responses::from_client("openai-tools", client)
        .expect("endpoint should build")
        .model("openai-test-model")
        .expect("model should build");

    let response = model
        .request(Message::user("inspect the path"))
        .max_tokens(64)
        .tool(ToolDefinition {
            name: "inspect_path".to_owned(),
            description: "Inspect one filesystem path.".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
        })
        .tool_choice(ToolChoice::Specific {
            function_names: vec!["inspect_path".to_owned()],
        })
        .send()
        .await
        .expect("tool fixture should parse");

    assert_eq!(response.finish_reason(), Some(FinishReason::ToolCalls));
    let [AssistantContent::ToolCall(tool_call)] = response.choice.as_slice() else {
        panic!(
            "expected one normalized tool call, got {:?}",
            response.choice
        );
    };
    assert_eq!(tool_call.function.name, "inspect_path");
    assert_eq!(tool_call.function.arguments["path"], "/tmp/bone");
    assert_eq!(
        tool_call
            .provider
            .as_ref()
            .map(|provider| provider.call_id.as_str()),
        Some("call_test_1")
    );
    assert_eq!(
        tool_call
            .provider
            .as_ref()
            .and_then(|provider| provider.item_id.as_deref()),
        Some("fc_test_1")
    );

    let first_requests = transport.unary_requests();
    let first_body: Value =
        serde_json::from_slice(&first_requests[0].body).expect("first request body should be JSON");
    assert_eq!(first_body["tools"][0]["type"], "function");
    assert_eq!(first_body["tools"][0]["name"], "inspect_path");
    assert_eq!(first_body["tools"][0]["parameters"]["type"], "object");
    assert_eq!(first_body["tool_choice"]["type"], "function");
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
    let second_transport = ScriptedHttpClient::unary_json(TEXT_RESPONSE);
    let second_client = rig_openai::Client::builder()
        .api_key("test-only-key")
        .http_client(second_transport.clone())
        .build()
        .expect("test client should build");
    let second_model = openai_responses::from_client("openai-tools", second_client)
        .expect("endpoint should build")
        .model("openai-test-model")
        .expect("model should build");

    second_model
        .request(Message::User {
            content: vec![result],
        })
        .messages([Message::user("inspect the path"), assistant])
        .max_tokens(64)
        .send()
        .await
        .expect("second-turn fixture should parse");

    let second_requests = second_transport.unary_requests();
    let second_body: Value = serde_json::from_slice(&second_requests[0].body)
        .expect("second request body should be JSON");
    let input = second_body["input"]
        .as_array()
        .expect("Responses input should be an array");
    let function_call = input
        .iter()
        .find(|item| item["type"] == "function_call")
        .expect("assistant function call should be replayed");
    let function_result = input
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .expect("function result should be sent");
    assert_eq!(function_call["id"], "fc_test_1");
    assert_eq!(function_call["call_id"], "call_test_1");
    assert_eq!(function_call["name"], "inspect_path");
    let replayed_arguments: Value = serde_json::from_str(
        function_call["arguments"]
            .as_str()
            .expect("replayed function arguments should be JSON text"),
    )
    .expect("replayed function arguments should contain valid JSON");
    assert_eq!(replayed_arguments["path"], "/tmp/bone");
    assert_eq!(function_result["call_id"], "call_test_1");
    assert_eq!(function_result["output"], "path is a directory");
}

#[tokio::test]
async fn sends_reasoning_controls_and_replays_summary_with_encrypted_state() {
    let transport = ScriptedHttpClient::unary_json(REASONING_RESPONSE);
    let client = rig_openai::Client::builder()
        .api_key("test-only-key")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let model = openai_responses::from_client("openai-reasoning", client)
        .expect("endpoint should build")
        .model("openai-test-model")
        .expect("model should build");

    let response = model
        .request(Message::user("reason about this"))
        .max_tokens(64)
        .additional_params(reasoning_params(
            Reasoning::new()
                .with_effort(ReasoningEffort::Low)
                .with_summary_level(ReasoningSummaryLevel::Auto),
        ))
        .send()
        .await
        .expect("reasoning fixture should parse");

    let first_requests = transport.unary_requests();
    let first_body: Value = serde_json::from_slice(&first_requests[0].body)
        .expect("reasoning request body should be JSON");
    assert_eq!(first_body["reasoning"]["effort"], "low");
    assert_eq!(first_body["reasoning"]["summary"], "auto");
    assert!(
        first_body["include"]
            .as_array()
            .is_some_and(|include| include
                .iter()
                .any(|item| item == "reasoning.encrypted_content")),
        "reasoning requests must ask the provider to return replayable encrypted state"
    );

    let reasoning = response
        .choice
        .iter()
        .find_map(|content| match content {
            AssistantContent::Reasoning(reasoning) => Some(reasoning),
            _ => None,
        })
        .expect("response should contain normalized reasoning");
    assert_eq!(reasoning.id.as_deref(), Some("rs_test_1"));
    assert!(reasoning.content.iter().any(
        |content| matches!(content, ReasoningContent::Summary(text) if text == "Checked the available evidence.")
    ));
    assert!(reasoning.content.iter().any(
        |content| matches!(content, ReasoningContent::Encrypted(data) if data == "encrypted-test-state")
    ));
    assert_eq!(response.usage.reasoning_tokens, 4);

    let assistant = Message::Assistant {
        id: response.message_id.clone(),
        content: response.choice.clone(),
    };
    let second_transport = ScriptedHttpClient::unary_json(TEXT_RESPONSE);
    let second_client = rig_openai::Client::builder()
        .api_key("test-only-key")
        .http_client(second_transport.clone())
        .build()
        .expect("test client should build");
    let second_model = openai_responses::from_client("openai-reasoning", second_client)
        .expect("endpoint should build")
        .model("openai-test-model")
        .expect("model should build");

    second_model
        .request(Message::user("continue"))
        .message(assistant)
        .max_tokens(64)
        .send()
        .await
        .expect("reasoning replay fixture should parse");

    let second_requests = second_transport.unary_requests();
    let second_body: Value = serde_json::from_slice(&second_requests[0].body)
        .expect("second request body should be JSON");
    let replay = second_body["input"]
        .as_array()
        .expect("Responses input should be an array")
        .iter()
        .find(|item| item["type"] == "reasoning")
        .expect("provider reasoning state should be replayed");
    assert_eq!(replay["id"], "rs_test_1");
    assert_eq!(replay["summary"][0]["type"], "summary_text");
    assert_eq!(
        replay["summary"][0]["text"],
        "Checked the available evidence."
    );
    assert_eq!(replay["encrypted_content"], "encrypted-test-state");
}

#[tokio::test]
async fn normalizes_responses_sse_and_requires_a_terminal_record() {
    let transport = ScriptedHttpClient::sse(TEXT_STREAM);
    let client = rig_openai::Client::builder()
        .api_key("test-only-key")
        .base_url("https://gateway.example/v1")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let model = openai_responses::from_client("openai-stream", client)
        .expect("endpoint should build")
        .model("openai-test-model")
        .expect("model should build");
    let mut stream = model
        .request(Message::user("hello"))
        .max_tokens(32)
        .stream()
        .await
        .expect("fixture stream should open");
    let mut text = String::new();
    let mut finals = 0;

    while let Some(item) = stream.next().await {
        match item.expect("fixture event should parse") {
            StreamedAssistantContent::Text(delta) => text.push_str(&delta.text),
            StreamedAssistantContent::Final(_) => finals += 1,
            _ => {}
        }
    }

    assert_eq!(text, "Hello stream");
    assert_eq!(finals, 1);
    let terminal = stream
        .response
        .expect("complete stream needs terminal record");
    assert_eq!(terminal.provider, "openai");
    assert_eq!(terminal.model.as_deref(), Some("openai-test-model"));
    assert_eq!(terminal.usage.input_tokens, 8);
    assert_eq!(terminal.usage.output_tokens, 3);

    let requests = transport.requests();
    let request = requests
        .first()
        .expect("one stream request should be captured");
    assert_eq!(requests.len(), 1);
    assert_eq!(request.method.as_str(), "POST");
    assert_eq!(request.uri, "https://gateway.example/v1/responses");
    assert_eq!(
        request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer test-only-key")
    );
}

#[tokio::test]
async fn does_not_mark_a_truncated_responses_stream_as_complete() {
    let client = rig_openai::Client::builder()
        .api_key("test-only-key")
        .http_client(ScriptedHttpClient::sse(TRUNCATED_STREAM))
        .build()
        .expect("test client should build");
    let model = openai_responses::from_client("openai-truncated", client)
        .expect("endpoint should build")
        .model("openai-test-model")
        .expect("model should build");
    let mut stream = model
        .request(Message::user("hello"))
        .max_tokens(32)
        .stream()
        .await
        .expect("fixture stream should open");
    let mut text = String::new();
    let mut finals = 0;

    while let Some(item) = stream.next().await {
        match item {
            Ok(StreamedAssistantContent::Text(delta)) => text.push_str(&delta.text),
            Ok(StreamedAssistantContent::Final(_)) => finals += 1,
            Ok(_) | Err(_) => {}
        }
    }

    assert_eq!(text, "partial");
    assert_eq!(finals, 0);
    assert!(stream.response.is_none());
}

#[tokio::test]
async fn preserves_non_success_status_and_openai_error_body() {
    let transport = ScriptedHttpClient::unary_error("400", ERROR_RESPONSE);
    let client = rig_openai::Client::builder()
        .api_key("test-only-key")
        .http_client(transport)
        .build()
        .expect("test client should build");
    let model = openai_responses::from_client("openai-error", client)
        .expect("endpoint should build")
        .model("openai-test-model")
        .expect("model should build");

    let error = model
        .request(Message::user("hello"))
        .send()
        .await
        .expect_err("400 response must fail");

    assert!(matches!(error, CompletionError::ProviderResponse(_)));
    assert_eq!(
        error
            .provider_response_status()
            .map(|status| status.as_u16()),
        Some(400)
    );
    assert_eq!(error.provider_response_body(), Some(ERROR_RESPONSE));
    assert_eq!(
        error
            .provider_response_json()
            .expect("fixture is valid JSON")
            .expect("provider body should be retained")["error"]["code"],
        "invalid_value"
    );
}
