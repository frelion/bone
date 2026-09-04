mod support;

use std::error::Error as _;

use bone_llm::{
    ErrorKind, FinishReason, InputItem, InputSource, OutputFormat, OutputItem, Protocol, Request,
    StreamEvent, ToolChoice, ToolDefinition, ToolOutput,
    testing::{model as test_model, openai_chat_completions_endpoint},
};
use futures_util::StreamExt;
use rig_core::{
    client::CompletionClient, completion::CompletionError, providers::openai as rig_openai,
};
use serde_json::Value;
use support::transport::ScriptedHttpClient;

const TEXT_RESPONSE: &str = include_str!("fixtures/openai_chat_completions/text_response.json");
const TOOL_RESPONSE: &str = include_str!("fixtures/openai_chat_completions/tool_response.json");
const TEXT_STREAM: &str = include_str!("fixtures/openai_chat_completions/text_stream.sse");
const TRUNCATED_STREAM: &str =
    include_str!("fixtures/openai_chat_completions/truncated_stream.sse");
const ERROR_RESPONSE: &str = include_str!("fixtures/openai_chat_completions/error_response.json");

#[tokio::test]
async fn sends_chat_completions_wire_and_preserves_all_identities() {
    let transport = ScriptedHttpClient::unary_json(TEXT_RESPONSE);
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

    let response = model
        .complete(
            Request::new([user("hello")])
                .instructions("Answer briefly.")
                .max_output_tokens(64),
        )
        .await
        .expect("fixture should parse");

    assert_eq!(endpoint.id(), "chat-test");
    assert_eq!(endpoint.protocol(), Protocol::OpenAiChatCompletions);
    assert_eq!(model.endpoint_id(), "chat-test");
    assert_eq!(model.protocol(), Protocol::OpenAiChatCompletions);
    assert_eq!(model.id(), "chat-test-model");
    assert_eq!(response.origin().provider(), "openai");
    assert_eq!(response.response_id(), Some("chatcmpl_text_1"));
    assert_eq!(
        response.origin().reported_model_id(),
        Some("chat-test-model")
    );
    assert_eq!(response.finish_reason(), Some(&FinishReason::Stop));
    assert!(matches!(
        response.items(),
        [OutputItem::Text(text)] if text == "Hello from Chat Completions."
    ));
    assert_eq!(response.usage().input_tokens, 8);
    assert_eq!(response.usage().output_tokens, 5);
    assert_eq!(response.usage().total_tokens, 13);

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

    let requests = transport.unary_requests();
    assert_eq!(requests.len(), 1);
    let body: Value =
        serde_json::from_slice(&requests[0].body).expect("request body should be JSON");
    assert_eq!(body["model"], "chat-test-model");
    assert_eq!(body["max_tokens"], 64);
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][0]["content"][0]["type"], "text");
    assert_eq!(body["messages"][0]["content"][0]["text"], "Answer briefly.");
    assert_eq!(body["messages"][1]["role"], "user");
    assert_eq!(body["messages"][1]["content"], "hello");
}

#[tokio::test]
async fn maps_tool_calls_and_replays_the_opaque_response_and_result() {
    let transport = ScriptedHttpClient::unary_json(TOOL_RESPONSE);
    let client = rig_openai::CompletionsClient::builder()
        .api_key("test-only-key")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let model = openai_chat_completions_endpoint("chat-tools", client)
        .expect("endpoint should build")
        .model("chat-test-model")
        .expect("model should build");

    let unsupported = model
        .complete(
            Request::new([user("inspect and return JSON")])
                .tools([inspect_path()])
                .output(OutputFormat::JsonSchema(serde_json::json!({
                    "type": "object"
                }))),
        )
        .await
        .expect_err("the initial tool turn cannot enforce a response schema");
    assert_eq!(unsupported.kind(), ErrorKind::UnsupportedOption);
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
    assert_eq!(first_body["tools"][0]["function"]["name"], "inspect_path");
    assert_eq!(first_body["tool_choice"]["type"], "function");
    assert_eq!(
        first_body["tool_choice"]["function"]["name"],
        "inspect_path"
    );

    let replay = response
        .into_item()
        .expect("tool response should be replayable as one opaque item");
    let second_transport = ScriptedHttpClient::unary_json(TEXT_RESPONSE);
    let second_client = rig_openai::CompletionsClient::builder()
        .api_key("test-only-key")
        .http_client(second_transport.clone())
        .build()
        .expect("test client should build");
    let second_model = openai_chat_completions_endpoint("chat-tools", second_client)
        .expect("endpoint should build")
        .model("chat-test-model")
        .expect("model should build");

    second_model
        .complete(
            Request::new([
                user("inspect the path"),
                replay,
                InputItem::tool_result(&call, ToolOutput::text("path is a directory")),
            ])
            .tools([inspect_path()])
            .output(OutputFormat::JsonSchema(serde_json::json!({
                "type": "object"
            })))
            .max_output_tokens(64),
        )
        .await
        .expect("second-turn fixture should parse");

    let second_metadata = second_transport.requests();
    assert_eq!(second_metadata.len(), 1);
    assert_eq!(
        second_metadata[0].uri,
        "https://api.openai.com/v1/chat/completions"
    );

    let second_requests = second_transport.unary_requests();
    let second_body: Value = serde_json::from_slice(&second_requests[0].body)
        .expect("second request body should be JSON");
    assert_eq!(second_body["response_format"]["type"], "json_schema");
    let messages = second_body["messages"]
        .as_array()
        .expect("messages should be an array");
    let assistant_call = messages
        .iter()
        .find(|message| message["role"] == "assistant")
        .and_then(|message| message["tool_calls"].as_array())
        .and_then(|calls| calls.first())
        .expect("assistant tool call should be replayed");
    let tool_result = messages
        .iter()
        .find(|message| message["role"] == "tool")
        .expect("tool result should be sent");
    assert_eq!(assistant_call["id"], "call_chat_test_1");
    assert_eq!(assistant_call["type"], "function");
    assert_eq!(assistant_call["function"]["name"], "inspect_path");
    assert_eq!(
        assistant_call["function"]["arguments"],
        r#"{"path":"/tmp/bone"}"#
    );
    assert_eq!(tool_result["tool_call_id"], "call_chat_test_1");
    assert_eq!(tool_result["content"], "path is a directory");
}

#[tokio::test]
async fn a_test_only_concrete_model_preserves_strict_tool_configuration() {
    let transport = ScriptedHttpClient::unary_json(TEXT_RESPONSE);
    let client = rig_openai::CompletionsClient::builder()
        .api_key("test-only-key")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let inner = client
        .completion_model("chat-test-model")
        .with_strict_tools();
    let model = test_model(
        "chat-strict",
        Protocol::OpenAiChatCompletions,
        "chat-test-model",
        inner,
    )
    .expect("strict model should build");

    model
        .complete(
            Request::new([user("inspect the path")])
                .max_output_tokens(64)
                .tools([inspect_path()]),
        )
        .await
        .expect("fixture should parse after request capture");

    let requests = transport.unary_requests();
    let body: Value = serde_json::from_slice(&requests[0].body)
        .expect("strict request body should be valid JSON");
    let function = &body["tools"][0]["function"];
    assert_eq!(function["name"], "inspect_path");
    assert_eq!(function["strict"], true);
    assert_eq!(function["parameters"]["additionalProperties"], false);
    assert_eq!(
        function["parameters"]["required"],
        serde_json::json!(["path"])
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
async fn preserves_chat_http_error_status_body_and_path() {
    let transport = ScriptedHttpClient::unary_error("429", ERROR_RESPONSE);
    let client = rig_openai::CompletionsClient::builder()
        .api_key("test-only-key")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let model = openai_chat_completions_endpoint("chat-error", client)
        .expect("endpoint should build")
        .model("chat-test-model")
        .expect("model should build");

    let error = model
        .complete(Request::new([user("hello")]))
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
        source
            .provider_response_json()
            .expect("fixture is valid JSON")
            .expect("provider body should be retained")["error"]["code"],
        "invalid_value"
    );
    assert_eq!(
        transport.requests()[0].uri,
        "https://api.openai.com/v1/chat/completions"
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
