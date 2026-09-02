mod support;

use bone_provider::{
    Protocol, StreamedAssistantContent,
    protocol::openai_chat_completions,
    rig::{
        client::CompletionClient,
        completion::{CompletionError, FinishReason, ToolDefinition},
        message::{AssistantContent, Message, ToolChoice, ToolResultContent, UserContent},
        providers::openai as rig_openai,
    },
};
use futures_util::StreamExt;
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
        openai_chat_completions::from_client("chat-test", client).expect("endpoint should build");
    let model = endpoint
        .model("chat-test-model")
        .expect("model should build");

    let response = model
        .request(Message::user("hello"))
        .preamble("Answer briefly.".to_owned())
        .max_tokens(64)
        .send()
        .await
        .expect("fixture should parse");

    assert_eq!(endpoint.id(), "chat-test");
    assert_eq!(endpoint.protocol(), Protocol::OpenAiChatCompletions);
    assert_eq!(model.endpoint_id(), "chat-test");
    assert_eq!(model.protocol(), Protocol::OpenAiChatCompletions);
    assert_eq!(model.id(), "chat-test-model");
    assert_eq!(response.provider, "openai");
    assert_eq!(response.response_id.as_deref(), Some("chatcmpl_text_1"));
    assert_eq!(response.model.as_deref(), Some("chat-test-model"));
    assert_eq!(response.finish_reason(), Some(FinishReason::Stop));
    assert!(matches!(
        response.choice.as_slice(),
        [AssistantContent::Text(text)] if text.text == "Hello from Chat Completions."
    ));
    assert_eq!(response.usage.input_tokens, 8);
    assert_eq!(response.usage.output_tokens, 5);
    assert_eq!(response.usage.total_tokens, 13);

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
    assert_eq!(body["model"], "chat-test-model");
    assert_eq!(body["max_tokens"], 64);
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][0]["content"][0]["type"], "text");
    assert_eq!(body["messages"][0]["content"][0]["text"], "Answer briefly.");
    assert_eq!(body["messages"][1]["role"], "user");
    assert_eq!(body["messages"][1]["content"], "hello");
}

#[tokio::test]
async fn maps_tool_calls_and_replays_the_complete_call_and_result() {
    let transport = ScriptedHttpClient::unary_json(TOOL_RESPONSE);
    let client = rig_openai::CompletionsClient::builder()
        .api_key("test-only-key")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let model = openai_chat_completions::from_client("chat-tools", client)
        .expect("endpoint should build")
        .model("chat-test-model")
        .expect("model should build");

    let response = model
        .request(Message::user("inspect the path"))
        .max_tokens(64)
        .tool(inspect_path_definition())
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
        Some("call_chat_test_1")
    );
    assert_eq!(
        tool_call
            .provider
            .as_ref()
            .and_then(|provider| provider.item_id.as_deref()),
        None
    );

    let first_requests = transport.unary_requests();
    let first_body: Value =
        serde_json::from_slice(&first_requests[0].body).expect("first request body should be JSON");
    assert_eq!(first_body["tools"][0]["type"], "function");
    assert_eq!(first_body["tools"][0]["function"]["name"], "inspect_path");
    assert_eq!(
        first_body["tools"][0]["function"]["parameters"]["type"],
        "object"
    );
    assert_eq!(first_body["tool_choice"]["type"], "function");
    assert_eq!(
        first_body["tool_choice"]["function"]["name"],
        "inspect_path"
    );

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
    let second_client = rig_openai::CompletionsClient::builder()
        .api_key("test-only-key")
        .http_client(second_transport.clone())
        .build()
        .expect("test client should build");
    let second_model = openai_chat_completions::from_client("chat-tools", second_client)
        .expect("endpoint should build")
        .model("chat-test-model")
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

    let second_metadata = second_transport.requests();
    assert_eq!(second_metadata.len(), 1);
    assert_eq!(second_metadata[0].method.as_str(), "POST");
    assert_eq!(
        second_metadata[0].uri,
        "https://api.openai.com/v1/chat/completions"
    );
    assert!(!second_metadata[0].uri.ends_with("/responses"));
    assert_eq!(
        second_metadata[0]
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer test-only-key")
    );

    let second_requests = second_transport.unary_requests();
    assert_eq!(second_requests.len(), 1);
    let second_body: Value = serde_json::from_slice(&second_requests[0].body)
        .expect("second request body should be JSON");
    let messages = second_body["messages"]
        .as_array()
        .expect("Chat Completions messages should be an array");
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
async fn custom_model_factory_preserves_strict_tools_through_type_erasure() {
    let transport = ScriptedHttpClient::unary_json(TEXT_RESPONSE);
    let client = rig_openai::CompletionsClient::builder()
        .api_key("test-only-key")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let endpoint = openai_chat_completions::from_model_factory("chat-strict", move |model_id| {
        client.completion_model(model_id).with_strict_tools()
    })
    .expect("strict Chat Completions endpoint should build");
    let model = endpoint
        .model("chat-test-model")
        .expect("strict Chat Completions model should build");

    model
        .request(Message::user("inspect the path"))
        .max_tokens(64)
        .tool(inspect_path_definition())
        .send()
        .await
        .expect("fixture should parse after the strict request is captured");

    assert_eq!(endpoint.protocol(), Protocol::OpenAiChatCompletions);
    assert_eq!(model.protocol(), Protocol::OpenAiChatCompletions);
    let requests = transport.unary_requests();
    assert_eq!(requests.len(), 1);
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
async fn normalizes_chat_sse_with_one_terminal_and_usage() {
    let transport = ScriptedHttpClient::sse(TEXT_STREAM);
    let client = rig_openai::CompletionsClient::builder()
        .api_key("test-only-key")
        .base_url("https://gateway.example/v1")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let model = openai_chat_completions::from_client("chat-stream", client)
        .expect("endpoint should build")
        .model("chat-test-model")
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
    assert_eq!(terminal.model.as_deref(), Some("chat-test-model"));
    assert_eq!(terminal.finish_reason, Some(FinishReason::Stop));
    assert_eq!(terminal.usage.input_tokens, 8);
    assert_eq!(terminal.usage.output_tokens, 3);
    assert_eq!(terminal.usage.total_tokens, 11);

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
        serde_json::from_slice(&requests[0].body).expect("stream request body should be JSON");
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
}

#[tokio::test]
async fn does_not_mark_a_truncated_chat_stream_as_complete() {
    let client = rig_openai::CompletionsClient::builder()
        .api_key("test-only-key")
        .http_client(ScriptedHttpClient::sse(TRUNCATED_STREAM))
        .build()
        .expect("test client should build");
    let model = openai_chat_completions::from_client("chat-truncated", client)
        .expect("endpoint should build")
        .model("chat-test-model")
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
        match item.expect("truncated fixture events should still parse") {
            StreamedAssistantContent::Text(delta) => text.push_str(&delta.text),
            StreamedAssistantContent::Final(_) => finals += 1,
            _ => {}
        }
    }

    assert_eq!(text, "partial");
    assert_eq!(finals, 0);
    assert!(stream.response.is_none());
}

#[tokio::test]
async fn preserves_chat_http_error_status_body_and_path() {
    let transport = ScriptedHttpClient::unary_error("429", ERROR_RESPONSE);
    let client = rig_openai::CompletionsClient::builder()
        .api_key("test-only-key")
        .http_client(transport.clone())
        .build()
        .expect("test client should build");
    let model = openai_chat_completions::from_client("chat-error", client)
        .expect("endpoint should build")
        .model("chat-test-model")
        .expect("model should build");

    let error = model
        .request(Message::user("hello"))
        .send()
        .await
        .expect_err("429 response must fail");

    assert!(matches!(error, CompletionError::ProviderResponse(_)));
    assert_eq!(
        error
            .provider_response_status()
            .map(|status| status.as_u16()),
        Some(429)
    );
    assert_eq!(error.provider_response_body(), Some(ERROR_RESPONSE));
    assert_eq!(
        error
            .provider_response_json()
            .expect("fixture is valid JSON")
            .expect("provider body should be retained")["error"]["code"],
        "invalid_value"
    );
    let metadata = transport.requests();
    assert_eq!(metadata.len(), 1);
    assert_eq!(metadata[0].method.as_str(), "POST");
    assert_eq!(
        metadata[0].uri,
        "https://api.openai.com/v1/chat/completions"
    );
    assert!(!metadata[0].uri.ends_with("/responses"));
    assert_eq!(
        metadata[0]
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer test-only-key")
    );
}

fn inspect_path_definition() -> ToolDefinition {
    ToolDefinition {
        name: "inspect_path".to_owned(),
        description: "Inspect one filesystem path.".to_owned(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"]
        }),
    }
}
