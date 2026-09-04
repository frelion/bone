mod support;

use std::error::Error as _;

use bone_llm::{
    ErrorKind, FinishReason, InputItem, InputSource, OutputItem, Protocol, Request, StreamEvent,
    ToolChoice, ToolDefinition, ToolOutput,
    protocol::openai_responses::{Options, Reasoning, ReasoningEffort, ReasoningSummary},
    testing::openai_responses_endpoint,
};
use futures_util::StreamExt;
use rig_core::{completion::CompletionError, providers::openai as rig_openai};
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
    let endpoint = openai_responses_endpoint("openai-test", client).expect("endpoint should build");
    let model = endpoint
        .model("openai-test-model")
        .expect("model should build");

    let response = model
        .complete(
            Request::new([user("hello")])
                .instructions("Answer briefly.")
                .max_output_tokens(64),
        )
        .await
        .expect("fixture should parse");

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
    assert_eq!(response.message_id(), Some("msg_test_1"));
    assert!(matches!(
        response.items(),
        [OutputItem::Text(text)] if text == "Hello from OpenAI Responses."
    ));
    assert_eq!(response.usage().input_tokens, 8);
    assert_eq!(response.usage().output_tokens, 5);

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

    let requests = transport.unary_requests();
    assert_eq!(requests.len(), 1);
    let body: Value =
        serde_json::from_slice(&requests[0].body).expect("request body should be JSON");
    assert_eq!(body["model"], "openai-test-model");
    assert_eq!(body["max_output_tokens"], 64);
    assert_eq!(body["instructions"], "Answer briefly.");
    assert_eq!(body["input"][0]["role"], "user");
    assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
    assert_eq!(body["input"][0]["content"][0]["text"], "hello");
}

#[tokio::test]
async fn maps_tools_and_replays_opaque_assistant_state_with_the_result() {
    let transport = ScriptedHttpClient::unary_json(TOOL_RESPONSE);
    let client = rig_openai::Client::builder()
        .api_key("test-only-key")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let model = openai_responses_endpoint("openai-tools", client)
        .expect("endpoint should build")
        .model("openai-test-model")
        .expect("model should build");

    let response = model
        .complete(
            Request::new([user("inspect the path")])
                .max_output_tokens(64)
                .tools([inspect_path()])
                .tool_choice(ToolChoice::Specific(vec!["inspect_path".to_owned()])),
        )
        .await
        .expect("tool fixture should parse");

    assert_eq!(response.finish_reason(), Some(&FinishReason::ToolCalls));
    let call = response
        .tool_calls()
        .next()
        .expect("one tool call should be exposed")
        .clone();
    assert_eq!(call.name(), "inspect_path");
    assert_eq!(call.arguments()["path"], "/tmp/bone");

    let first_requests = transport.unary_requests();
    let first_body: Value =
        serde_json::from_slice(&first_requests[0].body).expect("first request body should be JSON");
    assert_eq!(first_body["tools"][0]["type"], "function");
    assert_eq!(first_body["tools"][0]["name"], "inspect_path");
    assert_eq!(first_body["tools"][0]["parameters"]["type"], "object");
    assert_eq!(first_body["tool_choice"]["type"], "function");
    assert_eq!(first_body["tool_choice"]["name"], "inspect_path");

    let replay = response
        .into_item()
        .expect("tool response should be replayable as one opaque item");
    let second_transport = ScriptedHttpClient::unary_json(TEXT_RESPONSE);
    let second_client = rig_openai::Client::builder()
        .api_key("test-only-key")
        .http_client(second_transport.clone())
        .build()
        .expect("test client should build");
    let second_model = openai_responses_endpoint("openai-tools", second_client)
        .expect("endpoint should build")
        .model("openai-test-model")
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
    let input = second_body["input"]
        .as_array()
        .expect("input should be an array");
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
            .expect("arguments should be JSON text"),
    )
    .expect("arguments should be valid JSON");
    assert_eq!(replayed_arguments["path"], "/tmp/bone");
    assert_eq!(function_result["call_id"], "call_test_1");
    assert_eq!(function_result["output"], "path is a directory");
}

#[tokio::test]
async fn sends_reasoning_controls_and_replays_encrypted_state_opaquely() {
    let transport = ScriptedHttpClient::unary_json(REASONING_RESPONSE);
    let client = rig_openai::Client::builder()
        .api_key("test-only-key")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let model = openai_responses_endpoint("openai-reasoning", client)
        .expect("endpoint should build")
        .model("openai-test-model")
        .expect("model should build");

    let response = model
        .complete(
            Request::new([user("reason about this")])
                .max_output_tokens(64)
                .options(
                    Options::new().reasoning(
                        Reasoning::new()
                            .effort(ReasoningEffort::Low)
                            .summary(ReasoningSummary::Auto),
                    ),
                ),
        )
        .await
        .expect("reasoning fixture should parse");

    let first_requests = transport.unary_requests();
    let first_body: Value = serde_json::from_slice(&first_requests[0].body)
        .expect("reasoning request body should be JSON");
    assert_eq!(first_body["reasoning"]["effort"], "low");
    assert_eq!(first_body["reasoning"]["summary"], "auto");
    assert!(first_body["include"].as_array().is_some_and(|include| {
        include
            .iter()
            .any(|item| item == "reasoning.encrypted_content")
    }));
    assert!(response.items().iter().any(
        |item| matches!(item, OutputItem::ReasoningSummary(text) if text == "Checked the available evidence.")
    ));
    assert_eq!(response.usage().reasoning_tokens, 4);

    let replay = response
        .into_item()
        .expect("reasoning response should be replayable without exposing opaque state");
    let second_transport = ScriptedHttpClient::unary_json(TEXT_RESPONSE);
    let second_client = rig_openai::Client::builder()
        .api_key("test-only-key")
        .http_client(second_transport.clone())
        .build()
        .expect("test client should build");
    let second_model = openai_responses_endpoint("openai-reasoning", second_client)
        .expect("endpoint should build")
        .model("openai-test-model")
        .expect("model should build");

    second_model
        .complete(Request::new([
            user("reason about this"),
            replay,
            user("continue"),
        ]))
        .await
        .expect("reasoning replay fixture should parse");

    let second_requests = second_transport.unary_requests();
    let second_body: Value = serde_json::from_slice(&second_requests[0].body)
        .expect("second request body should be JSON");
    let replay = second_body["input"]
        .as_array()
        .expect("input should be an array")
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
        .stream(Request::new([user("hello")]).max_output_tokens(32))
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
    let transport = ScriptedHttpClient::unary_error("400", ERROR_RESPONSE);
    let client = rig_openai::Client::builder()
        .api_key("test-only-key")
        .http_client(transport)
        .build()
        .expect("test client should build");
    let model = openai_responses_endpoint("openai-error", client)
        .expect("endpoint should build")
        .model("openai-test-model")
        .expect("model should build");

    let error = model
        .complete(Request::new([user("hello")]))
        .await
        .expect_err("400 response must fail");

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
