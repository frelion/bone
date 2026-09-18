mod support;

use std::error::Error as _;

use bone_adapters::llm::{
    Error, ErrorKind, FinishReason, InputItem, InputSource, Model, OutputItem, Protocol, Request,
    Response, StreamEvent, ToolDefinition, testing::anthropic_messages_endpoint,
};
use futures_util::StreamExt;
use rig_core::{
    completion::CompletionError, providers::anthropic as rig_anthropic,
    test_utils::HttpErrorStreamingClient,
};
use serde_json::Value;
use support::transport::ScriptedHttpClient;

const TEXT_STREAM: &str = include_str!("fixtures/anthropic_messages/text_stream.sse");
const THINKING_STREAM: &str = include_str!("fixtures/anthropic_messages/thinking_stream.sse");
const TRUNCATED_STREAM: &str = include_str!("fixtures/anthropic_messages/truncated_stream.sse");
const ERROR_RESPONSE: &str = include_str!("fixtures/anthropic_messages/error_response.json");

/// The streamed form of the tool fixture: the same `tool_use` block, delivered
/// the way a streaming Messages turn delivers it.
const TOOL_STREAM: &str = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_tool_1","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-6","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":18,"cache_creation_input_tokens":null,"cache_read_input_tokens":null,"output_tokens":0}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_test_1","name":"inspect_path","input":{}}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"/tmp/bone\"}"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":12}}

event: message_stop
data: {"type":"message_stop"}

"#;

/// Drive one streaming call to the single terminal response it must end with.
///
/// Deltas are display-only, so every behavioural assertion in this file reads
/// the terminal [`Response`]: the one trustworthy record of the call.
async fn streamed_response(model: &Model, request: Request) -> Response {
    let mut stream = model.stream(request).await.expect("stream should open");
    let mut terminal = None;
    while let Some(item) = stream.next().await {
        if let StreamEvent::Completed(response) = item.expect("stream item should be valid") {
            terminal = Some(response);
        }
    }
    terminal.expect("stream must end with one complete response")
}

/// Drive one rejected streaming call to the single failure it must end with.
async fn streamed_error(model: &Model, request: Request) -> Error {
    let mut stream = match model.stream(request).await {
        Ok(stream) => stream,
        Err(error) => return error,
    };
    let mut failure = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(StreamEvent::Completed(_)) => {
                panic!("a rejected call must not report a completed response")
            }
            Ok(_) => {}
            Err(error) => failure = Some(error),
        }
    }
    failure.expect("a rejected stream must end with one explicit error")
}

#[tokio::test]
async fn sends_anthropic_wire_shape_and_preserves_endpoint_identity() {
    let transport = ScriptedHttpClient::sse(TEXT_STREAM);
    let client = rig_anthropic::Client::builder()
        .api_key("test-only-key")
        .base_url("https://gateway.example/v1/messages")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let endpoint =
        anthropic_messages_endpoint("anthropic-test", client).expect("endpoint should build");
    let model = endpoint
        .model("claude-sonnet-4-6")
        .expect("model should build");

    let response = streamed_response(
        &model,
        Request::new([user("hello")]).instructions("Answer briefly."),
    )
    .await;

    assert_eq!(endpoint.id(), "anthropic-test");
    assert_eq!(endpoint.protocol(), Protocol::AnthropicMessages);
    assert_eq!(model.endpoint_id(), "anthropic-test");
    assert_eq!(model.protocol(), Protocol::AnthropicMessages);
    assert_eq!(model.id(), "claude-sonnet-4-6");
    assert_eq!(response.origin().provider(), "anthropic");
    assert_eq!(response.message_id(), Some("msg_stream_1"));
    assert!(matches!(
        response.items(),
        [OutputItem::Text(text)] if text == "Hello stream"
    ));
    assert_eq!(response.usage().input_tokens, 8);
    assert_eq!(response.usage().output_tokens, 3);

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

    let requests = transport.streaming_requests();
    let body: Value =
        serde_json::from_slice(&requests[0].body).expect("request body should be JSON");
    assert_eq!(body["model"], "claude-sonnet-4-6");
    // Anthropic requires `max_tokens` on every request. BONE sends no bound
    // of its own, so the value must be the per-model default Rig supplies.
    assert_eq!(body["max_tokens"], 64_000);
    assert_eq!(body["system"][0]["type"], "text");
    assert_eq!(body["system"][0]["text"], "Answer briefly.");
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"][0]["type"], "text");
    assert_eq!(body["messages"][0]["content"][0]["text"], "hello");
}

#[tokio::test]
async fn rejects_required_tool_without_tools_and_maps_the_tool_wire_shape() {
    let transport = ScriptedHttpClient::sse(TOOL_STREAM);
    let client = rig_anthropic::Client::builder()
        .api_key("test-only-key")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let model = anthropic_messages_endpoint("anthropic-tools", client)
        .expect("endpoint should build")
        .model("claude-sonnet-4-6")
        .expect("model should build");

    let unsupported = match model
        .stream(Request::new([user("hello")]).require_tool("inspect_path"))
        .await
    {
        Err(error) => error,
        Ok(_) => panic!("an undefined required tool unexpectedly opened a stream"),
    };
    assert_eq!(unsupported.kind(), ErrorKind::InvalidRequest);
    assert!(transport.requests().is_empty());

    let response = streamed_response(
        &model,
        Request::new([user("inspect the path")])
            .tools([inspect_path()])
            .require_tool("inspect_path"),
    )
    .await;

    let call = response
        .tool_calls()
        .next()
        .expect("one tool call should be exposed")
        .clone();
    assert_eq!(call.name(), "inspect_path");
    assert_eq!(call.arguments()["path"], "/tmp/bone");
    assert_eq!(response.finish_reason(), Some(&FinishReason::ToolCalls));

    let first_requests = transport.streaming_requests();
    let first_body: Value =
        serde_json::from_slice(&first_requests[0].body).expect("request body should be JSON");
    assert_eq!(first_body["tools"][0]["name"], "inspect_path");
    assert_eq!(first_body["tools"][0]["input_schema"]["type"], "object");
    assert_eq!(first_body["tool_choice"]["type"], "tool");
    assert_eq!(first_body["tool_choice"]["name"], "inspect_path");
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
        .model("claude-sonnet-4-6")
        .expect("model should build");
    let mut stream = model
        .stream(Request::new([user("hello")]))
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
    assert_eq!(
        response.origin().reported_model_id(),
        Some("claude-sonnet-4-6")
    );
    assert_eq!(response.finish_reason(), Some(&FinishReason::Stop));
    assert_eq!(response.usage().input_tokens, 8);
    assert_eq!(response.usage().output_tokens, 3);
    assert!(stream.next().await.is_none());

    let metadata = transport.requests();
    assert_eq!(metadata.len(), 1);
    assert_eq!(metadata[0].uri, "https://gateway.example/v1/messages");
}

#[tokio::test]
async fn reports_finish_reason_usage_and_reasoning_from_a_thinking_response() {
    let transport = ScriptedHttpClient::sse(THINKING_STREAM);
    let client = rig_anthropic::Client::builder()
        .api_key("test-only-key")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let model = anthropic_messages_endpoint("anthropic-thinking", client)
        .expect("endpoint should build")
        .model("claude-sonnet-4-6")
        .expect("model should build");
    let mut stream = model
        .stream(Request::new([user("reason briefly")]))
        .await
        .expect("fixture stream should open");
    let mut reasoning = String::new();
    let mut completed = Vec::new();

    while let Some(event) = stream.next().await {
        match event.expect("fixture event should parse") {
            StreamEvent::ReasoningDelta(delta) => reasoning.push_str(&delta),
            StreamEvent::Completed(response) => completed.push(response),
            _ => {}
        }
    }

    // Thinking text is surfaced to the caller as it arrives; its opaque
    // signature is not, because BONE never sends an assistant turn back to a
    // provider.
    assert_eq!(reasoning, "Check the facts first.");
    assert_eq!(completed.len(), 1);
    let response = completed.pop().unwrap();
    assert_eq!(response.finish_reason(), Some(&FinishReason::Stop));
    assert_eq!(response.usage().input_tokens, 3);
    assert_eq!(response.usage().output_tokens, 7);
    assert_eq!(response.usage().cached_input_tokens, 4);
    assert_eq!(response.usage().cache_creation_input_tokens, 9);
    assert_eq!(response.usage().reasoning_tokens, 5);
    assert_eq!(response.usage().total_tokens, 23);
    assert!(matches!(
        response.items(),
        [OutputItem::Text(text)] if text == "Done."
    ));

    // No base URL is configured, so the provider default endpoint is the one
    // the adapter must have called.
    let metadata = transport.requests();
    assert_eq!(metadata.len(), 1);
    assert_eq!(metadata[0].uri, "https://api.anthropic.com/v1/messages");
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
        .model("claude-sonnet-4-6")
        .expect("model should build");
    let mut stream = model
        .stream(Request::new([user("hello")]))
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
    // A stream opens with the same POST a unary call sends, so a rejected
    // stream-open is where a non-success status and its error body arrive.
    let client = rig_anthropic::Client::builder()
        .api_key("test-only-key")
        .http_client(HttpErrorStreamingClient::new(
            http::StatusCode::TOO_MANY_REQUESTS,
            ERROR_RESPONSE,
        ))
        .build()
        .expect("test client should build");
    let model = anthropic_messages_endpoint("anthropic-error", client)
        .expect("endpoint should build")
        .model("claude-sonnet-4-6")
        .expect("model should build");

    let error = streamed_error(&model, Request::new([user("hello")])).await;

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
