mod support;

use std::error::Error as _;

use bone_adapters::llm::{
    Error, ErrorKind, FinishReason, InputItem, InputSource, Model, OutputItem, Protocol, Request,
    Response, StreamEvent, ToolDefinition, testing::openai_chat_completions_endpoint,
};
use futures_util::StreamExt;
use rig_core::{
    completion::CompletionError, providers::openai as rig_openai,
    test_utils::HttpErrorStreamingClient,
};
use serde_json::Value;
use support::transport::ScriptedHttpClient;

const TEXT_STREAM: &str = include_str!("fixtures/openai_chat_completions/text_stream.sse");
const TRUNCATED_STREAM: &str =
    include_str!("fixtures/openai_chat_completions/truncated_stream.sse");
const ERROR_RESPONSE: &str = include_str!("fixtures/openai_chat_completions/error_response.json");

/// The streamed form of the tool fixture: the same function call, delivered the
/// way a streaming Chat Completions turn delivers it, plus the terminal record.
const TOOL_STREAM: &str = r#"data: {"id":"chatcmpl_tool_1","object":"chat.completion.chunk","created":1700000001,"model":"chat-test-model","choices":[{"index":0,"delta":{"role":"assistant","content":null,"tool_calls":[{"index":0,"id":"call_chat_test_1","type":"function","function":{"name":"inspect_path","arguments":""}}]},"finish_reason":null}],"usage":null}

data: {"id":"chatcmpl_tool_1","object":"chat.completion.chunk","created":1700000001,"model":"chat-test-model","choices":[{"index":0,"delta":{"content":null,"tool_calls":[{"index":0,"id":null,"type":"function","function":{"name":null,"arguments":"{\"path\":\"/tmp/bone\"}"}}]},"finish_reason":null}],"usage":null}

data: {"id":"chatcmpl_tool_1","object":"chat.completion.chunk","created":1700000001,"model":"chat-test-model","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],"usage":null}

data: {"id":"chatcmpl_tool_1","object":"chat.completion.chunk","created":1700000001,"model":"chat-test-model","choices":[],"usage":{"prompt_tokens":18,"completion_tokens":12,"total_tokens":30}}

data: [DONE]

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
async fn sends_chat_completions_wire_and_preserves_all_identities() {
    let transport = ScriptedHttpClient::sse(TEXT_STREAM);
    let client = rig_openai::CompletionsClient::builder()
        .api_key("test-only-key")
        .base_url("https://gateway.example/v1")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let endpoint =
        openai_chat_completions_endpoint("chat-test", client).expect("endpoint should build");
    let model = endpoint
        .model("chat-test-model")
        .expect("model should build");

    let response = streamed_response(
        &model,
        Request::new([user("hello")]).instructions("Answer briefly."),
    )
    .await;

    assert_eq!(endpoint.id(), "chat-test");
    assert_eq!(endpoint.protocol(), Protocol::OpenAiChatCompletions);
    assert_eq!(model.endpoint_id(), "chat-test");
    assert_eq!(model.protocol(), Protocol::OpenAiChatCompletions);
    assert_eq!(model.id(), "chat-test-model");
    assert_eq!(response.origin().provider(), "openai");
    assert_eq!(response.response_id(), Some("chatcmpl_stream_1"));
    assert_eq!(
        response.origin().reported_model_id(),
        Some("chat-test-model")
    );
    assert_eq!(response.finish_reason(), Some(&FinishReason::Stop));
    assert!(matches!(
        response.items(),
        [OutputItem::Text(text)] if text == "Hello stream"
    ));
    assert_eq!(response.usage().input_tokens, 8);
    assert_eq!(response.usage().output_tokens, 3);
    assert_eq!(response.usage().total_tokens, 11);

    let metadata = transport.requests();
    assert_eq!(metadata.len(), 1);
    assert_eq!(metadata[0].method.as_str(), "POST");
    assert_eq!(
        metadata[0].uri,
        "https://gateway.example/v1/chat/completions"
    );
    assert!(!metadata[0].uri.ends_with("/responses"));
    assert_eq!(
        metadata[0]
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer test-only-key")
    );

    let requests = transport.streaming_requests();
    assert_eq!(requests.len(), 1);
    let body: Value =
        serde_json::from_slice(&requests[0].body).expect("request body should be JSON");
    assert_eq!(body["model"], "chat-test-model");
    // BONE sends no output-token bound of its own.
    assert!(body["max_tokens"].is_null());
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][0]["content"][0]["type"], "text");
    assert_eq!(body["messages"][0]["content"][0]["text"], "Answer briefly.");
    assert_eq!(body["messages"][1]["role"], "user");
    assert_eq!(body["messages"][1]["content"], "hello");
}

#[tokio::test]
async fn maps_tools_to_the_chat_completions_wire_shape() {
    let transport = ScriptedHttpClient::sse(TOOL_STREAM);
    let client = rig_openai::CompletionsClient::builder()
        .api_key("test-only-key")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let model = openai_chat_completions_endpoint("chat-tools", client)
        .expect("endpoint should build")
        .model("chat-test-model")
        .expect("model should build");

    let response = streamed_response(
        &model,
        Request::new([user("inspect the path")])
            .tools([inspect_path()])
            .require_tool("inspect_path"),
    )
    .await;

    assert_eq!(response.finish_reason(), Some(&FinishReason::ToolCalls));
    let call = response
        .tool_calls()
        .next()
        .expect("one tool call should be exposed")
        .clone();
    assert_eq!(call.name(), "inspect_path");
    assert_eq!(call.arguments()["path"], "/tmp/bone");

    let first_requests = transport.streaming_requests();
    let first_body: Value =
        serde_json::from_slice(&first_requests[0].body).expect("first request body should be JSON");
    assert_eq!(first_body["tools"][0]["type"], "function");
    assert_eq!(first_body["tools"][0]["function"]["name"], "inspect_path");
    assert_eq!(first_body["tool_choice"]["type"], "function");
    assert_eq!(
        first_body["tool_choice"]["function"]["name"],
        "inspect_path"
    );
}

#[tokio::test]
async fn stream_emits_text_and_exactly_one_completed_response() {
    let transport = ScriptedHttpClient::sse(TEXT_STREAM);
    let client = rig_openai::CompletionsClient::builder()
        .api_key("test-only-key")
        .base_url("https://gateway.example/v1")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let model = openai_chat_completions_endpoint("chat-stream", client)
        .expect("endpoint should build")
        .model("chat-test-model")
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
    assert_eq!(response.origin().provider(), "openai");
    assert_eq!(
        response.origin().reported_model_id(),
        Some("chat-test-model")
    );
    assert_eq!(response.finish_reason(), Some(&FinishReason::Stop));
    assert_eq!(response.usage().input_tokens, 8);
    assert_eq!(response.usage().output_tokens, 3);
    assert_eq!(response.usage().total_tokens, 11);
    assert!(stream.next().await.is_none());

    let requests = transport.streaming_requests();
    let body: Value =
        serde_json::from_slice(&requests[0].body).expect("stream request body should be JSON");
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
}

#[tokio::test]
async fn truncated_stream_ends_with_one_explicit_error_and_no_completed_response() {
    let client = rig_openai::CompletionsClient::builder()
        .api_key("test-only-key")
        .http_client(ScriptedHttpClient::sse(TRUNCATED_STREAM))
        .build()
        .expect("test client should build");
    let model = openai_chat_completions_endpoint("chat-truncated", client)
        .expect("endpoint should build")
        .model("chat-test-model")
        .expect("model should build");
    let mut stream = model
        .stream(Request::new([user("hello")]))
        .await
        .expect("fixture stream should open");
    let mut text = String::new();
    let mut completed = 0;
    let mut errors = Vec::new();

    while let Some(event) = stream.next().await {
        match event {
            Ok(StreamEvent::TextDelta(delta)) => text.push_str(&delta),
            Ok(StreamEvent::Completed(_)) => completed += 1,
            Ok(_) => {}
            Err(error) => errors.push(error),
        }
    }

    assert_eq!(text, "partial");
    assert_eq!(completed, 0);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].kind(), ErrorKind::IncompleteStream);
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn preserves_chat_http_error_status_body_and_path() {
    // A stream opens with the same POST a unary call sends, so a rejected
    // stream-open is where a non-success status and its error body arrive.
    let transport =
        HttpErrorStreamingClient::new(http::StatusCode::TOO_MANY_REQUESTS, ERROR_RESPONSE);
    let client = rig_openai::CompletionsClient::builder()
        .api_key("test-only-key")
        .http_client(transport)
        .build()
        .expect("test client should build");
    let model = openai_chat_completions_endpoint("chat-error", client)
        .expect("endpoint should build")
        .model("chat-test-model")
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
    assert_eq!(
        source
            .provider_response_json()
            .expect("fixture is valid JSON")
            .expect("provider body should be retained")["error"]["code"],
        "invalid_value"
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
