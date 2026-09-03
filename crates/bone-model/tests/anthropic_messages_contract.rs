mod support;

use bone_model::{
    Protocol, StreamedAssistantContent,
    protocol::anthropic_messages,
    rig::{
        completion::{CompletionError, FinishReason, ToolDefinition},
        message::{AssistantContent, Message, ToolChoice, ToolResultContent, UserContent},
        providers::anthropic as rig_anthropic,
    },
};
use futures_util::StreamExt;
use serde_json::Value;
use support::transport::ScriptedHttpClient;

const TEXT_RESPONSE: &str = include_str!("fixtures/anthropic_messages/text_response.json");
const TEXT_STREAM: &str = include_str!("fixtures/anthropic_messages/text_stream.sse");
const THINKING_STREAM: &str = include_str!("fixtures/anthropic_messages/thinking_stream.sse");
const TRUNCATED_STREAM: &str = include_str!("fixtures/anthropic_messages/truncated_stream.sse");
const TOOL_RESPONSE: &str = include_str!("fixtures/anthropic_messages/tool_response.json");
const ERROR_RESPONSE: &str = include_str!("fixtures/anthropic_messages/error_response.json");

#[tokio::test]
async fn sends_anthropic_messages_wire_shape_and_preserves_endpoint_identity() {
    let transport = ScriptedHttpClient::unary_json(TEXT_RESPONSE);
    let client = rig_anthropic::Client::builder()
        .api_key("test-only-key")
        .base_url("https://gateway.example/v1/messages")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let endpoint =
        anthropic_messages::from_client("anthropic-test", client).expect("endpoint should build");
    let model = endpoint.model("claude-test").expect("model should build");

    let response = model
        .request(Message::user("hello"))
        .preamble("Answer briefly.".to_owned())
        .max_tokens(64)
        .send()
        .await
        .expect("fixture should parse");

    assert_eq!(endpoint.id(), "anthropic-test");
    assert_eq!(endpoint.protocol(), Protocol::AnthropicMessages);
    assert_eq!(model.endpoint_id(), "anthropic-test");
    assert_eq!(model.protocol(), Protocol::AnthropicMessages);
    assert_eq!(model.id(), "claude-test");
    assert_eq!(response.provider, "anthropic");
    assert_eq!(response.message_id.as_deref(), Some("msg_test_1"));
    assert!(matches!(
        response.choice.as_slice(),
        [AssistantContent::Text(text)] if text.text == "Hello from Anthropic."
    ));
    assert_eq!(response.usage.input_tokens, 8);
    assert_eq!(response.usage.output_tokens, 5);

    let metadata = transport.requests();
    let request_metadata = metadata.first().expect("one request should be captured");
    assert_eq!(metadata.len(), 1);
    assert_eq!(request_metadata.method.as_str(), "POST");
    assert_eq!(request_metadata.uri, "https://gateway.example/v1/messages");
    assert_eq!(
        request_metadata
            .headers
            .get("x-api-key")
            .and_then(|value| value.to_str().ok()),
        Some("test-only-key")
    );
    assert_eq!(
        request_metadata
            .headers
            .get("anthropic-version")
            .and_then(|value| value.to_str().ok()),
        Some("2023-06-01")
    );
    assert_eq!(
        request_metadata
            .headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );

    let requests = transport.unary_requests();
    let request = requests.first().expect("one request should be captured");
    assert_eq!(requests.len(), 1);

    let body: Value = serde_json::from_slice(&request.body).expect("request body should be JSON");
    assert_eq!(body["model"], "claude-test");
    assert_eq!(body["max_tokens"], 64);
    assert_eq!(body["system"][0]["type"], "text");
    assert_eq!(body["system"][0]["text"], "Answer briefly.");
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"][0]["type"], "text");
    assert_eq!(body["messages"][0]["content"][0]["text"], "hello");
}

#[tokio::test]
async fn maps_tools_to_anthropic_wire_and_normalizes_tool_use() {
    let transport = ScriptedHttpClient::unary_json(TOOL_RESPONSE);
    let client = rig_anthropic::Client::builder()
        .api_key("test-only-key")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let model = anthropic_messages::from_client("anthropic-tools", client)
        .expect("endpoint should build")
        .model("claude-test")
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

    let [AssistantContent::ToolCall(tool_call)] = response.choice.as_slice() else {
        panic!(
            "expected one normalized tool call, got {:?}",
            response.choice
        );
    };
    assert_eq!(tool_call.function.name, "inspect_path");
    assert_eq!(tool_call.function.arguments["path"], "/tmp/bone");
    assert_eq!(
        tool_call.provider.as_ref().map(|id| id.call_id.as_str()),
        Some("toolu_test_1")
    );

    let requests = transport.unary_requests();
    let body: Value =
        serde_json::from_slice(&requests[0].body).expect("request body should be JSON");
    assert_eq!(body["tools"][0]["name"], "inspect_path");
    assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
    assert_eq!(body["tool_choice"]["type"], "tool");
    assert_eq!(body["tool_choice"]["name"], "inspect_path");

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
    let second_client = rig_anthropic::Client::builder()
        .api_key("test-only-key")
        .http_client(second_transport.clone())
        .build()
        .expect("test client should build");
    let second_model = anthropic_messages::from_client("anthropic-tools", second_client)
        .expect("endpoint should build")
        .model("claude-test")
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
    assert_eq!(second_body["messages"][1]["role"], "assistant");
    assert_eq!(second_body["messages"][1]["content"][0]["type"], "tool_use");
    assert_eq!(
        second_body["messages"][1]["content"][0]["id"],
        "toolu_test_1"
    );
    assert_eq!(second_body["messages"][2]["role"], "user");
    assert_eq!(
        second_body["messages"][2]["content"][0]["type"],
        "tool_result"
    );
    assert_eq!(
        second_body["messages"][2]["content"][0]["tool_use_id"],
        "toolu_test_1"
    );
}

#[tokio::test]
async fn custom_model_factory_keeps_anthropic_cache_and_strict_tool_options_available() {
    use bone_model::rig::client::CompletionClient;

    let transport = ScriptedHttpClient::unary_json(TEXT_RESPONSE);
    let client = rig_anthropic::Client::builder()
        .api_key("test-only-key")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let model = anthropic_messages::from_model_factory("anthropic-cached", move |model_id| {
        client
            .completion_model(model_id)
            .with_automatic_caching()
            .with_strict_tools()
    })
    .expect("endpoint should build")
    .model("claude-test")
    .expect("model should build");

    model
        .request(Message::user("hello"))
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
        .send()
        .await
        .expect("fixture should parse");

    let requests = transport.unary_requests();
    let body: Value =
        serde_json::from_slice(&requests[0].body).expect("request body should be JSON");
    assert_eq!(body["cache_control"]["type"], "ephemeral");
    assert_eq!(body["tools"][0]["strict"], true);
    assert_eq!(
        body["tools"][0]["input_schema"]["additionalProperties"],
        false
    );
}

#[tokio::test]
async fn normalizes_anthropic_sse_and_requires_a_terminal_record() {
    let transport = ScriptedHttpClient::sse(TEXT_STREAM);
    let client = rig_anthropic::Client::builder()
        .api_key("test-only-key")
        .base_url("https://gateway.example/v1/messages")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let model = anthropic_messages::from_client("anthropic-stream", client)
        .expect("endpoint should build")
        .model("claude-test")
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
    assert_eq!(terminal.provider, "anthropic");
    assert_eq!(terminal.model.as_deref(), Some("claude-test"));
    assert_eq!(terminal.finish_reason, Some(FinishReason::Stop));
    assert_eq!(terminal.usage.input_tokens, 8);
    assert_eq!(terminal.usage.output_tokens, 3);

    let requests = transport.requests();
    let request = requests
        .first()
        .expect("one stream request should be captured");
    assert_eq!(requests.len(), 1);
    assert_eq!(request.method.as_str(), "POST");
    assert_eq!(request.uri, "https://gateway.example/v1/messages");
    assert_eq!(
        request
            .headers
            .get("x-api-key")
            .and_then(|value| value.to_str().ok()),
        Some("test-only-key")
    );
    assert_eq!(
        request
            .headers
            .get("anthropic-version")
            .and_then(|value| value.to_str().ok()),
        Some("2023-06-01")
    );
}

#[tokio::test]
async fn preserves_thinking_deltas_signature_cache_usage_and_terminal() {
    let client = rig_anthropic::Client::builder()
        .api_key("test-only-key")
        .http_client(ScriptedHttpClient::sse(THINKING_STREAM))
        .build()
        .expect("test client should build");
    let model = anthropic_messages::from_client("anthropic-thinking", client)
        .expect("endpoint should build")
        .model("claude-test")
        .expect("model should build");
    let mut stream = model
        .request(Message::user("reason briefly"))
        .max_tokens(64)
        .stream()
        .await
        .expect("fixture stream should open");
    let mut delta_text = String::new();
    let mut delta_ids = Vec::new();
    let mut completed = None;
    let mut completed_id = None;
    let mut finals = 0;

    while let Some(item) = stream.next().await {
        match item.expect("fixture event should parse") {
            StreamedAssistantContent::ReasoningDelta { id, reasoning, .. } => {
                delta_ids.push(id);
                delta_text.push_str(&reasoning);
            }
            StreamedAssistantContent::Reasoning { id, reasoning } => {
                completed_id = Some(id);
                completed = Some(reasoning);
            }
            StreamedAssistantContent::Final(_) => finals += 1,
            _ => {}
        }
    }

    assert_eq!(delta_text, "Check the facts first.");
    assert_eq!(
        delta_ids.first(),
        delta_ids.last(),
        "all deltas for one thinking block need one correlator"
    );
    let completed = completed.expect("thinking block must emit a completed replacement");
    assert_eq!(completed.display_text(), "Check the facts first.");
    assert_eq!(completed.first_signature(), Some("sig_part_1sig_part_2"));
    assert_eq!(completed_id.as_ref(), delta_ids.first());
    assert_eq!(finals, 1);

    let aggregated = stream
        .choice
        .iter()
        .find_map(|content| match content {
            AssistantContent::Reasoning(reasoning) => Some(reasoning),
            _ => None,
        })
        .expect("aggregated choice must retain thinking");
    assert_eq!(aggregated.display_text(), "Check the facts first.");
    assert_eq!(aggregated.first_signature(), Some("sig_part_1sig_part_2"));

    let terminal = stream.response.expect("thinking stream needs a terminal");
    assert_eq!(terminal.finish_reason, Some(FinishReason::Stop));
    assert_eq!(terminal.usage.input_tokens, 3);
    assert_eq!(terminal.usage.output_tokens, 7);
    assert_eq!(terminal.usage.cached_input_tokens, 4);
    assert_eq!(terminal.usage.cache_creation_input_tokens, 9);
    assert_eq!(terminal.usage.reasoning_tokens, 5);
    assert_eq!(terminal.usage.total_tokens, 23);
}

#[tokio::test]
async fn does_not_mark_a_truncated_stream_as_complete_without_message_delta() {
    let client = rig_anthropic::Client::builder()
        .api_key("test-only-key")
        .http_client(ScriptedHttpClient::sse(TRUNCATED_STREAM))
        .build()
        .expect("test client should build");
    let model = anthropic_messages::from_client("anthropic-truncated", client)
        .expect("endpoint should build")
        .model("claude-test")
        .expect("model should build");
    let mut stream = model
        .request(Message::user("hello"))
        .max_tokens(32)
        .stream()
        .await
        .expect("fixture stream should open");
    let mut finals = 0;

    while let Some(item) = stream.next().await {
        match item {
            Ok(StreamedAssistantContent::Final(_)) => finals += 1,
            Ok(_) => {}
            Err(_) => {}
        }
    }

    assert_eq!(finals, 0);
    assert!(stream.response.is_none());
}

#[tokio::test]
async fn preserves_non_success_status_and_anthropic_error_body() {
    let transport = ScriptedHttpClient::unary_error("429", ERROR_RESPONSE);
    let client = rig_anthropic::Client::builder()
        .api_key("test-only-key")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let model = anthropic_messages::from_client("anthropic-error", client)
        .expect("endpoint should build")
        .model("claude-test")
        .expect("model should build");

    let error = model
        .request(Message::user("hello"))
        .max_tokens(32)
        .send()
        .await
        .expect_err("429 response must fail");

    assert!(
        matches!(error, CompletionError::ProviderResponse(_)),
        "unexpected error: {error:?}"
    );
    assert_eq!(
        error
            .provider_response_status()
            .map(|status| status.as_u16()),
        Some(429)
    );
    assert_eq!(error.provider_response_body(), Some(ERROR_RESPONSE));

    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method.as_str(), "POST");
    assert_eq!(requests[0].uri, "https://api.anthropic.com/v1/messages");
}
