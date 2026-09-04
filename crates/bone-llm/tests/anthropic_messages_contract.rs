mod support;

use std::error::Error as _;

use bone_llm::{
    ErrorKind, FinishReason, InputItem, InputSource, OutputItem, Protocol, Request, StreamEvent,
    ToolChoice, ToolDefinition, ToolOutput,
    testing::{anthropic_messages_endpoint, model as test_model},
};
use futures_util::StreamExt;
use rig_core::{
    client::CompletionClient, completion::CompletionError, providers::anthropic as rig_anthropic,
};
use serde_json::Value;
use support::transport::ScriptedHttpClient;

const TEXT_RESPONSE: &str = include_str!("fixtures/anthropic_messages/text_response.json");
const TEXT_STREAM: &str = include_str!("fixtures/anthropic_messages/text_stream.sse");
const THINKING_STREAM: &str = include_str!("fixtures/anthropic_messages/thinking_stream.sse");
const TRUNCATED_STREAM: &str = include_str!("fixtures/anthropic_messages/truncated_stream.sse");
const TOOL_RESPONSE: &str = include_str!("fixtures/anthropic_messages/tool_response.json");
const ERROR_RESPONSE: &str = include_str!("fixtures/anthropic_messages/error_response.json");

#[tokio::test]
async fn sends_anthropic_wire_shape_and_preserves_endpoint_identity() {
    let transport = ScriptedHttpClient::unary_json(TEXT_RESPONSE);
    let client = rig_anthropic::Client::builder()
        .api_key("test-only-key")
        .base_url("https://gateway.example/v1/messages")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let endpoint =
        anthropic_messages_endpoint("anthropic-test", client).expect("endpoint should build");
    let model = endpoint.model("claude-test").expect("model should build");

    let response = model
        .complete(
            Request::new([user("hello")])
                .instructions("Answer briefly.")
                .max_output_tokens(64),
        )
        .await
        .expect("fixture should parse");

    assert_eq!(endpoint.id(), "anthropic-test");
    assert_eq!(endpoint.protocol(), Protocol::AnthropicMessages);
    assert_eq!(model.endpoint_id(), "anthropic-test");
    assert_eq!(model.protocol(), Protocol::AnthropicMessages);
    assert_eq!(model.id(), "claude-test");
    assert_eq!(response.origin().provider(), "anthropic");
    assert_eq!(response.message_id(), Some("msg_test_1"));
    assert!(matches!(
        response.items(),
        [OutputItem::Text(text)] if text == "Hello from Anthropic."
    ));
    assert_eq!(response.usage().input_tokens, 8);
    assert_eq!(response.usage().output_tokens, 5);

    let metadata = transport.requests();
    assert_eq!(metadata.len(), 1);
    assert_eq!(metadata[0].method.as_str(), "POST");
    assert_eq!(metadata[0].uri, "https://gateway.example/v1/messages");
    assert_eq!(
        metadata[0]
            .headers
            .get("x-api-key")
            .and_then(|value| value.to_str().ok()),
        Some("test-only-key")
    );
    assert_eq!(
        metadata[0]
            .headers
            .get("anthropic-version")
            .and_then(|value| value.to_str().ok()),
        Some("2023-06-01")
    );

    let requests = transport.unary_requests();
    let body: Value =
        serde_json::from_slice(&requests[0].body).expect("request body should be JSON");
    assert_eq!(body["model"], "claude-test");
    assert_eq!(body["max_tokens"], 64);
    assert_eq!(body["system"][0]["type"], "text");
    assert_eq!(body["system"][0]["text"], "Answer briefly.");
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"][0]["type"], "text");
    assert_eq!(body["messages"][0]["content"][0]["text"], "hello");
}

#[tokio::test]
async fn maps_tools_and_replays_the_opaque_response_and_result() {
    let transport = ScriptedHttpClient::unary_json(TOOL_RESPONSE);
    let client = rig_anthropic::Client::builder()
        .api_key("test-only-key")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let model = anthropic_messages_endpoint("anthropic-tools", client)
        .expect("endpoint should build")
        .model("claude-test")
        .expect("model should build");

    let unsupported = match model
        .stream(Request::new([user("hello")]).tool_choice(ToolChoice::Required))
        .await
    {
        Err(error) => error,
        Ok(_) => panic!("tool_choice without tools unexpectedly opened a stream"),
    };
    assert_eq!(unsupported.kind(), ErrorKind::InvalidRequest);
    assert!(transport.requests().is_empty());

    let response = model
        .complete(
            Request::new([user("inspect the path")])
                .max_output_tokens(64)
                .tools([inspect_path()])
                .tool_choice(ToolChoice::Specific(vec!["inspect_path".to_owned()])),
        )
        .await
        .expect("tool fixture should parse");

    let call = response
        .tool_calls()
        .next()
        .expect("one tool call should be exposed")
        .clone();
    assert_eq!(call.name(), "inspect_path");
    assert_eq!(call.arguments()["path"], "/tmp/bone");

    let first_requests = transport.unary_requests();
    let first_body: Value =
        serde_json::from_slice(&first_requests[0].body).expect("request body should be JSON");
    assert_eq!(first_body["tools"][0]["name"], "inspect_path");
    assert_eq!(first_body["tools"][0]["input_schema"]["type"], "object");
    assert_eq!(first_body["tool_choice"]["type"], "tool");
    assert_eq!(first_body["tool_choice"]["name"], "inspect_path");

    let replay = response
        .into_item()
        .expect("tool response should be replayable as one opaque item");
    let second_transport = ScriptedHttpClient::unary_json(TEXT_RESPONSE);
    let second_client = rig_anthropic::Client::builder()
        .api_key("test-only-key")
        .http_client(second_transport.clone())
        .build()
        .expect("test client should build");
    let second_model = anthropic_messages_endpoint("anthropic-tools", second_client)
        .expect("endpoint should build")
        .model("claude-test")
        .expect("model should build");

    second_model
        .complete(
            Request::new([
                user("inspect the path"),
                replay,
                InputItem::tool_result(&call, ToolOutput::text("path is a directory")),
            ])
            .max_output_tokens(64),
        )
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
async fn a_test_only_concrete_model_preserves_cache_and_strict_tool_options() {
    let transport = ScriptedHttpClient::unary_json(TEXT_RESPONSE);
    let client = rig_anthropic::Client::builder()
        .api_key("test-only-key")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let inner = client
        .completion_model("claude-test")
        .with_automatic_caching()
        .with_strict_tools();
    let model = test_model(
        "anthropic-cached",
        Protocol::AnthropicMessages,
        "claude-test",
        inner,
    )
    .expect("model should build");

    model
        .complete(
            Request::new([user("hello")])
                .max_output_tokens(64)
                .tools([inspect_path()]),
        )
        .await
        .expect("fixture should parse");

    let requests = transport.unary_requests();
    let body: Value = serde_json::from_slice(&requests[0].body).expect("body should be JSON");
    assert_eq!(body["cache_control"]["type"], "ephemeral");
    assert_eq!(body["tools"][0]["strict"], true);
    assert_eq!(
        body["tools"][0]["input_schema"]["additionalProperties"],
        false
    );
}

#[tokio::test]
async fn stream_emits_text_and_exactly_one_completed_response() {
    let transport = ScriptedHttpClient::sse(TEXT_STREAM);
    let client = rig_anthropic::Client::builder()
        .api_key("test-only-key")
        .base_url("https://gateway.example/v1/messages")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let model = anthropic_messages_endpoint("anthropic-stream", client)
        .expect("endpoint should build")
        .model("claude-test")
        .expect("model should build");
    let mut stream = model
        .stream(Request::new([user("hello")]).max_output_tokens(32))
        .await
        .expect("fixture stream should open");
    let mut text = String::new();
    let mut completed = Vec::new();

    while let Some(event) = stream.next().await {
        match event.expect("fixture event should parse") {
            StreamEvent::TextDelta(delta) => text.push_str(&delta),
            StreamEvent::Completed(response) => completed.push(response),
            _ => {}
        }
    }

    assert_eq!(text, "Hello stream");
    assert_eq!(completed.len(), 1);
    let response = completed.pop().unwrap();
    assert_eq!(response.origin().provider(), "anthropic");
    assert_eq!(response.origin().reported_model_id(), Some("claude-test"));
    assert_eq!(response.finish_reason(), Some(&FinishReason::Stop));
    assert_eq!(response.usage().input_tokens, 8);
    assert_eq!(response.usage().output_tokens, 3);
    assert!(stream.next().await.is_none());

    let metadata = transport.requests();
    assert_eq!(metadata.len(), 1);
    assert_eq!(metadata[0].uri, "https://gateway.example/v1/messages");
}

#[tokio::test]
async fn completed_thinking_response_replays_its_opaque_signature() {
    let first_client = rig_anthropic::Client::builder()
        .api_key("test-only-key")
        .http_client(ScriptedHttpClient::sse(THINKING_STREAM))
        .build()
        .expect("test client should build");
    let model = anthropic_messages_endpoint("anthropic-thinking", first_client)
        .expect("endpoint should build")
        .model("claude-test")
        .expect("model should build");
    let mut stream = model
        .stream(Request::new([user("reason briefly")]).max_output_tokens(64))
        .await
        .expect("fixture stream should open");
    let mut completed = Vec::new();

    while let Some(event) = stream.next().await {
        if let StreamEvent::Completed(response) = event.expect("fixture event should parse") {
            completed.push(response);
        }
    }

    assert_eq!(completed.len(), 1);
    let response = completed.pop().unwrap();
    assert_eq!(response.finish_reason(), Some(&FinishReason::Stop));
    assert_eq!(response.usage().input_tokens, 3);
    assert_eq!(response.usage().output_tokens, 7);
    assert_eq!(response.usage().cached_input_tokens, 4);
    assert_eq!(response.usage().cache_creation_input_tokens, 9);
    assert_eq!(response.usage().reasoning_tokens, 5);
    assert_eq!(response.usage().total_tokens, 23);

    let replay = response
        .into_item()
        .expect("thinking response should be replayable without exposing its signature");
    let second_transport = ScriptedHttpClient::unary_json(TEXT_RESPONSE);
    let second_client = rig_anthropic::Client::builder()
        .api_key("test-only-key")
        .http_client(second_transport.clone())
        .build()
        .expect("test client should build");
    let second_model = anthropic_messages_endpoint("anthropic-thinking", second_client)
        .expect("endpoint should build")
        .model("claude-test")
        .expect("model should build");

    second_model
        .complete(
            Request::new([user("reason briefly"), replay, user("continue")]).max_output_tokens(64),
        )
        .await
        .expect("replay fixture should parse");

    let requests = second_transport.unary_requests();
    let body: Value = serde_json::from_slice(&requests[0].body).expect("body should be JSON");
    let thinking = body["messages"][1]["content"]
        .as_array()
        .expect("assistant content should be an array")
        .iter()
        .find(|item| item["type"] == "thinking")
        .expect("opaque thinking block should be replayed");
    assert_eq!(thinking["thinking"], "Check the facts first.");
    assert_eq!(thinking["signature"], "sig_part_1sig_part_2");
}

#[tokio::test]
async fn truncated_stream_ends_with_one_explicit_error_and_no_completed_response() {
    let client = rig_anthropic::Client::builder()
        .api_key("test-only-key")
        .http_client(ScriptedHttpClient::sse(TRUNCATED_STREAM))
        .build()
        .expect("test client should build");
    let model = anthropic_messages_endpoint("anthropic-truncated", client)
        .expect("endpoint should build")
        .model("claude-test")
        .expect("model should build");
    let mut stream = model
        .stream(Request::new([user("hello")]).max_output_tokens(32))
        .await
        .expect("fixture stream should open");
    let mut completed = 0;
    let mut errors = Vec::new();

    while let Some(event) = stream.next().await {
        match event {
            Ok(StreamEvent::Completed(_)) => completed += 1,
            Ok(_) => {}
            Err(error) => errors.push(error),
        }
    }

    assert_eq!(completed, 0);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].kind(), ErrorKind::IncompleteStream);
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn preserves_non_success_status_and_anthropic_error_body() {
    let transport = ScriptedHttpClient::unary_error("429", ERROR_RESPONSE);
    let client = rig_anthropic::Client::builder()
        .api_key("test-only-key")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let model = anthropic_messages_endpoint("anthropic-error", client)
        .expect("endpoint should build")
        .model("claude-test")
        .expect("model should build");

    let error = model
        .complete(Request::new([user("hello")]).max_output_tokens(32))
        .await
        .expect_err("429 response must fail");

    assert_eq!(error.kind(), ErrorKind::Provider);
    let source = error
        .source()
        .and_then(|source| source.downcast_ref::<CompletionError>())
        .expect("provider failure should retain the Rig error as its source");
    assert_eq!(
        source
            .provider_response_status()
            .map(|status| status.as_u16()),
        Some(429)
    );
    assert_eq!(source.provider_response_body(), Some(ERROR_RESPONSE));
    assert_eq!(
        transport.requests()[0].uri,
        "https://api.anthropic.com/v1/messages"
    );
}

fn user(text: &str) -> InputItem {
    InputItem::external(InputSource::User, text)
}

fn inspect_path() -> ToolDefinition {
    ToolDefinition::new(
        "inspect_path",
        "Inspect one filesystem path.",
        serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"]
        }),
    )
}
