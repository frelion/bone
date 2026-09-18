mod support;

use std::error::Error as _;

use bone_adapters::llm::{
    Error, ErrorKind, FinishReason, InputItem, InputSource, Model, ModelOptions, OutputItem,
    Protocol, Request, Response, StreamEvent, ToolDefinition,
    protocol::openai_responses::{Reasoning, ReasoningEffort, ReasoningSummary},
    testing::openai_responses_endpoint,
};
use futures_util::StreamExt;
use rig_core::{
    completion::CompletionError, providers::openai as rig_openai,
    test_utils::HttpErrorStreamingClient,
};
use serde_json::Value;
use support::transport::ScriptedHttpClient;

const TEXT_STREAM: &str = include_str!("fixtures/openai_responses/text_stream.sse");
const TRUNCATED_STREAM: &str = include_str!("fixtures/openai_responses/truncated_stream.sse");
const ERROR_RESPONSE: &str = include_str!("fixtures/openai_responses/error_response.json");

/// The streamed form of the tool fixture: the same function call, delivered the
/// way a streaming Responses turn delivers it, plus the terminal record.
const TOOL_STREAM: &str = r#"data: {"type":"response.output_item.added","output_index":0,"sequence_number":1,"item":{"type":"function_call","id":"fc_test_1","call_id":"call_test_1","name":"inspect_path","arguments":"","status":"in_progress"}}

data: {"type":"response.function_call_arguments.delta","output_index":0,"sequence_number":2,"delta":"{\"path\":\"/tmp/bone\"}"}

data: {"type":"response.output_item.done","output_index":0,"sequence_number":3,"item":{"type":"function_call","id":"fc_test_1","call_id":"call_test_1","name":"inspect_path","arguments":"{\"path\":\"/tmp/bone\"}","status":"completed"}}

data: {"type":"response.completed","sequence_number":4,"response":{"id":"resp_tool_1","object":"response","created_at":0,"status":"completed","model":"openai-test-model","usage":{"input_tokens":18,"output_tokens":12,"total_tokens":30},"output":[{"type":"function_call","id":"fc_test_1","call_id":"call_test_1","name":"inspect_path","arguments":"{\"path\":\"/tmp/bone\"}","status":"completed"}],"tools":[]}}

"#;

/// The streamed form of the reasoning fixture: one visible reasoning summary,
/// one answer, and the terminal record that reports the usage.
const REASONING_STREAM: &str = r#"data: {"type":"response.reasoning_summary_text.delta","item_id":"rs_test_1","output_index":0,"summary_index":0,"sequence_number":1,"delta":"Checked the available evidence."}

data: {"type":"response.output_item.done","output_index":0,"sequence_number":2,"item":{"type":"reasoning","id":"rs_test_1","summary":[{"type":"summary_text","text":"Checked the available evidence."}],"encrypted_content":"encrypted-test-state","status":"completed"}}

data: {"type":"response.output_text.delta","item_id":"msg_reasoning_1","output_index":1,"content_index":0,"sequence_number":3,"delta":"Reasoned answer."}

data: {"type":"response.completed","sequence_number":4,"response":{"id":"resp_reasoning_1","object":"response","created_at":0,"status":"completed","model":"openai-test-model","usage":{"input_tokens":12,"input_tokens_details":{"cached_tokens":0},"output_tokens":9,"output_tokens_details":{"reasoning_tokens":4},"total_tokens":21},"output":[{"type":"reasoning","id":"rs_test_1","summary":[{"type":"summary_text","text":"Checked the available evidence."}],"encrypted_content":"encrypted-test-state","status":"completed"},{"type":"message","id":"msg_reasoning_1","status":"completed","role":"assistant","content":[{"type":"output_text","annotations":[],"text":"Reasoned answer."}]}],"tools":[]}}

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
async fn sends_responses_wire_shape_and_preserves_endpoint_identity() {
    let transport = ScriptedHttpClient::sse(TEXT_STREAM);
    let client = rig_openai::Client::builder()
        .api_key("test-only-key")
        .base_url("https://gateway.example/v1")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let endpoint = openai_responses_endpoint("openai-test", client).expect("endpoint should build");
    let model = endpoint
        .model("openai-test-model")
        .expect("model should build");

    let response = streamed_response(
        &model,
        Request::new([user("hello")]).instructions("Answer briefly."),
    )
    .await;

    assert_eq!(endpoint.id(), "openai-test");
    assert_eq!(endpoint.protocol(), Protocol::OpenAiResponses);
    assert_eq!(model.endpoint_id(), "openai-test");
    assert_eq!(model.protocol(), Protocol::OpenAiResponses);
    assert_eq!(model.id(), "openai-test-model");
    assert_eq!(response.origin().provider(), "openai");
    assert_eq!(
        response.origin().reported_model_id(),
        Some("openai-test-model")
    );
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
    assert_eq!(metadata[0].uri, "https://gateway.example/v1/responses");
    assert!(!metadata[0].uri.ends_with("/chat/completions"));
    assert_eq!(
        metadata[0]
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer test-only-key")
    );
    assert_eq!(
        metadata[0]
            .headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );

    let requests = transport.streaming_requests();
    assert_eq!(requests.len(), 1);
    let body: Value =
        serde_json::from_slice(&requests[0].body).expect("request body should be JSON");
    assert_eq!(body["model"], "openai-test-model");
    // BONE sends no output-token bound of its own.
    assert!(body["max_output_tokens"].is_null());
    assert_eq!(body["instructions"], "Answer briefly.");
    assert_eq!(body["input"][0]["role"], "user");
    assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
    assert_eq!(body["input"][0]["content"][0]["text"], "hello");
}

#[tokio::test]
async fn maps_tools_to_the_responses_wire_shape() {
    let transport = ScriptedHttpClient::sse(TOOL_STREAM);
    let client = rig_openai::Client::builder()
        .api_key("test-only-key")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let model = openai_responses_endpoint("openai-tools", client)
        .expect("endpoint should build")
        .model("openai-test-model")
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
    assert_eq!(first_body["tools"][0]["name"], "inspect_path");
    assert_eq!(first_body["tools"][0]["parameters"]["type"], "object");
    assert_eq!(first_body["tool_choice"]["type"], "function");
    assert_eq!(first_body["tool_choice"]["name"], "inspect_path");
}

#[tokio::test]
async fn sends_reasoning_controls_and_reads_the_reported_reasoning_output() {
    let transport = ScriptedHttpClient::sse(REASONING_STREAM);
    let client = rig_openai::Client::builder()
        .api_key("test-only-key")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let model = openai_responses_endpoint("openai-reasoning", client)
        .expect("endpoint should build")
        .model("openai-test-model")
        .expect("model should build");

    let response = streamed_response(
        &model,
        Request::new([user("reason about this")]).options(ModelOptions::OpenAiResponses {
            reasoning: Reasoning::new()
                .effort(ReasoningEffort::Low)
                .summary(ReasoningSummary::Auto),
        }),
    )
    .await;

    let requests = transport.streaming_requests();
    let body: Value =
        serde_json::from_slice(&requests[0].body).expect("reasoning request body should be JSON");
    assert_eq!(body["reasoning"]["effort"], "low");
    assert_eq!(body["reasoning"]["summary"], "auto");
    assert!(body["include"].as_array().is_some_and(|include| {
        include
            .iter()
            .any(|item| item == "reasoning.encrypted_content")
    }));
    assert!(response.items().iter().any(
        |item| matches!(item, OutputItem::ReasoningSummary(text) if text == "Checked the available evidence.")
    ));
    assert_eq!(response.usage().reasoning_tokens, 4);
}

#[tokio::test]
async fn stream_emits_text_and_exactly_one_completed_response() {
    let transport = ScriptedHttpClient::sse(TEXT_STREAM);
    let client = rig_openai::Client::builder()
        .api_key("test-only-key")
        .base_url("https://gateway.example/v1")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let model = openai_responses_endpoint("openai-stream", client)
        .expect("endpoint should build")
        .model("openai-test-model")
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
        Some("openai-test-model")
    );
    assert_eq!(response.usage().input_tokens, 8);
    assert_eq!(response.usage().output_tokens, 3);
    assert!(stream.next().await.is_none());

    let metadata = transport.requests();
    assert_eq!(metadata.len(), 1);
    assert_eq!(metadata[0].uri, "https://gateway.example/v1/responses");
}

#[tokio::test]
async fn truncated_stream_ends_with_one_explicit_error_and_no_completed_response() {
    let client = rig_openai::Client::builder()
        .api_key("test-only-key")
        .http_client(ScriptedHttpClient::sse(TRUNCATED_STREAM))
        .build()
        .expect("test client should build");
    let model = openai_responses_endpoint("openai-truncated", client)
        .expect("endpoint should build")
        .model("openai-test-model")
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
async fn preserves_non_success_status_and_openai_error_body() {
    // A stream opens with the same POST a unary call sends, so a rejected
    // stream-open is where a non-success status and its error body arrive.
    let client = rig_openai::Client::builder()
        .api_key("test-only-key")
        .http_client(HttpErrorStreamingClient::new(
            http::StatusCode::BAD_REQUEST,
            ERROR_RESPONSE,
        ))
        .build()
        .expect("test client should build");
    let model = openai_responses_endpoint("openai-error", client)
        .expect("endpoint should build")
        .model("openai-test-model")
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
        Some(400)
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
