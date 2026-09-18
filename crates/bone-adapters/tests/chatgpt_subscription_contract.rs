mod support;

use bone_adapters::llm::{
    FinishReason, InputItem, InputSource, Model, OutputItem, Protocol, Request, Response,
    StreamEvent, ToolDefinition, testing::chatgpt_subscription_endpoint,
};
use futures_util::StreamExt;
use http::StatusCode;
use rig_core::{
    providers::chatgpt::{self as rig_chatgpt, ChatGPTAuth},
    test_utils::{CapturedHttpRequest, HttpErrorStreamingClient},
};
use serde_json::{Value, json};
use support::transport::ScriptedHttpClient;

/// The text turn: the delta that builds the aggregated output item and the
/// terminal record that reports it.
///
/// The ChatGPT subscription streams its answer as SSE, so a body carrying only
/// the terminal record aggregates to zero output items; both events are part of
/// the contract.
const TEXT_SSE: &str = r#"data: {"type":"response.output_text.delta","item_id":"msg_stream_1","output_index":0,"content_index":0,"sequence_number":1,"delta":"ok"}

data: {"type":"response.completed","sequence_number":2,"response":{"id":"resp_text_1","object":"response","created_at":1,"status":"completed","error":null,"incomplete_details":null,"instructions":null,"max_output_tokens":null,"model":"gpt-test","usage":{"input_tokens":2,"input_tokens_details":{"cached_tokens":0},"output_tokens":1,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":3},"output":[{"type":"message","id":"msg_stream_1","status":"completed","role":"assistant","content":[{"type":"output_text","annotations":[],"text":"ok"}]}],"tools":[]}}

data: [DONE]"#;

/// The tool turn: the finished function-call item reaches the aggregate through
/// its own event, so the terminal record alone is not enough here either.
const TOOL_SSE: &str = r#"data: {"type":"response.output_item.done","output_index":0,"sequence_number":1,"item":{"type":"function_call","id":"fc_test_1","call_id":"call_test_1","name":"inspect_path","arguments":"{\"path\":\"/tmp/bone\"}","status":"completed"}}

data: {"type":"response.completed","sequence_number":2,"response":{"id":"resp_tool_1","object":"response","created_at":1,"status":"completed","error":null,"incomplete_details":null,"instructions":null,"max_output_tokens":null,"model":"gpt-test","usage":{"input_tokens":8,"input_tokens_details":{"cached_tokens":0},"output_tokens":4,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":12},"output":[{"type":"function_call","id":"fc_test_1","call_id":"call_test_1","name":"inspect_path","arguments":"{\"path\":\"/tmp/bone\"}","status":"completed"}],"tools":[]}}

data: [DONE]"#;

const STREAM_SSE: &str = include_str!("fixtures/openai_responses/text_stream.sse");

/// Drive a call to its single terminal response.
///
/// Streaming is the only model-call mode, so a test that cares about the final
/// result still consumes the stream. Deltas are display-only: the terminal
/// `Completed` record is the one trustworthy record of what the model produced.
async fn complete(model: &Model, request: Request) -> Response {
    let mut stream = model.stream(request).await.expect("stream should open");
    let mut terminal = None;
    while let Some(item) = stream.next().await {
        if let StreamEvent::Completed(response) = item.expect("stream item should be valid") {
            terminal = Some(response);
        }
    }
    terminal.expect("a stream must end with one complete response")
}

fn test_client(
    body: &'static str,
) -> (rig_chatgpt::Client<ScriptedHttpClient>, ScriptedHttpClient) {
    let transport = ScriptedHttpClient::sse(body);
    let client = rig_chatgpt::Client::builder()
        .api_key(ChatGPTAuth::AccessToken {
            access_token: "sentinel-secret-token".to_owned(),
            account_id: Some("acct_test".to_owned()),
        })
        .base_url("https://chatgpt.example/backend-api/codex")
        .http_client(transport.clone())
        .default_instructions("")
        .originator("bone")
        .user_agent("bone-adapters/test")
        .build()
        .expect("test ChatGPT client should build");
    (client, transport)
}

fn default_url_test_client(
    body: &'static str,
) -> (rig_chatgpt::Client<ScriptedHttpClient>, ScriptedHttpClient) {
    let transport = ScriptedHttpClient::sse(body);
    let client = rig_chatgpt::Client::builder()
        .api_key(ChatGPTAuth::AccessToken {
            access_token: "sentinel-secret-token".to_owned(),
            account_id: Some("acct_test".to_owned()),
        })
        .http_client(transport.clone())
        .default_instructions("")
        .originator("bone")
        .user_agent("bone-adapters/test")
        .build()
        .expect("test ChatGPT client should build with its production base URL");
    (client, transport)
}

#[tokio::test]
async fn keeps_rig_chatgpt_production_url_as_an_offline_contract() {
    let (client, transport) = default_url_test_client(TEXT_SSE);
    let model = chatgpt_subscription_endpoint("chatgpt-default-url", client)
        .expect("subscription endpoint should build")
        .model("gpt-test")
        .expect("model should build");

    complete(&model, Request::new([user("hello")])).await;

    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].uri,
        "https://chatgpt.com/backend-api/codex/responses"
    );
}

#[tokio::test]
async fn redacts_authentication_and_stream_provider_bodies() {
    // Streaming is the only model-call mode, so a rejected call is a rejected
    // stream, and the rejection is scripted on the streaming path.
    let handshake_secret = "sentinel-secret-stream-401";
    let stream_client = rig_chatgpt::Client::builder()
        .api_key(ChatGPTAuth::AccessToken {
            access_token: "rejected-test-token".to_owned(),
            account_id: Some("acct_test".to_owned()),
        })
        .http_client(HttpErrorStreamingClient::new(
            StatusCode::UNAUTHORIZED,
            handshake_secret,
        ))
        .build()
        .unwrap();
    let stream_model = chatgpt_subscription_endpoint("chatgpt-stream-error", stream_client)
        .unwrap()
        .model("gpt-test")
        .unwrap();
    let mut stream = stream_model
        .stream(Request::new([user("hello")]))
        .await
        .unwrap();
    let stream_error = stream.next().await.unwrap().unwrap_err();
    let rendered = format!("{stream_error:?}: {stream_error}");
    assert!(rendered.contains("reconnect"));
    assert!(!rendered.contains(handshake_secret));
    assert!(stream.next().await.is_none());

    let envelope_secret = "sentinel-secret-sse-envelope";
    let body = format!(
        "data: {{\"type\":\"error\",\"error\":{{\"message\":\"{envelope_secret}\",\"code\":\"server_error\",\"type\":\"server_error\"}}}}\n\n"
    );
    let envelope_transport = ScriptedHttpClient::sse(body);
    let envelope_client = rig_chatgpt::Client::builder()
        .api_key(ChatGPTAuth::AccessToken {
            access_token: "test-token".to_owned(),
            account_id: Some("acct_test".to_owned()),
        })
        .http_client(envelope_transport)
        .build()
        .unwrap();
    let envelope_model = chatgpt_subscription_endpoint("chatgpt-envelope-error", envelope_client)
        .unwrap()
        .model("gpt-test")
        .unwrap();
    let mut stream = envelope_model
        .stream(Request::new([user("hello")]))
        .await
        .unwrap();
    let envelope_error = stream.next().await.unwrap().unwrap_err();
    let rendered = format!("{envelope_error:?}: {envelope_error}");
    assert!(!rendered.contains(envelope_secret));
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn maps_the_subscription_to_the_codex_responses_wire_shape() {
    let (client, transport) = test_client(TEXT_SSE);
    let debug = format!("{client:?}");
    assert!(!debug.contains("sentinel-secret-token"));

    let endpoint =
        chatgpt_subscription_endpoint("chatgpt-test", client).expect("endpoint should build");
    let model = endpoint.model("gpt-test").expect("model should build");

    let response = complete(
        &model,
        Request::new([user("hello")]).instructions("Answer briefly."),
    )
    .await;

    assert_eq!(endpoint.protocol(), Protocol::OpenAiResponses);
    assert_eq!(model.protocol(), Protocol::OpenAiResponses);
    assert_eq!(model.endpoint_id(), "chatgpt-test");
    assert_eq!(response.origin().provider(), "chatgpt");
    assert!(matches!(response.items(), [OutputItem::Text(text)] if text == "ok"));

    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    let streamed = transport.streaming_requests();
    assert_eq!(streamed.len(), 1);
    let request = &streamed[0];
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
    // BONE sends no output bound of its own.
    assert!(body.get("max_output_tokens").is_none());
    assert!(body.get("temperature").is_none());
    assert!(body["include"].as_array().is_some_and(|values| {
        values
            .iter()
            .any(|value| value == "reasoning.encrypted_content")
    }));
}

#[tokio::test]
async fn stream_emits_text_and_exactly_one_completed_response() {
    let transport = ScriptedHttpClient::sse(STREAM_SSE);
    let client = rig_chatgpt::Client::builder()
        .api_key(ChatGPTAuth::AccessToken {
            access_token: "test-token".to_owned(),
            account_id: Some("acct_test".to_owned()),
        })
        .http_client(transport)
        .default_instructions("")
        .build()
        .expect("test ChatGPT client should build");
    let model = chatgpt_subscription_endpoint("chatgpt-stream", client)
        .expect("endpoint should build")
        .model("gpt-test")
        .expect("model should build");
    let mut stream = model
        .stream(Request::new([user("hello")]))
        .await
        .expect("stream should open");
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
    assert_eq!(completed[0].text().as_deref(), Some("Hello stream"));
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn preserves_tool_calls_and_their_wire_shape() {
    let (client, transport) = test_client(TOOL_SSE);
    let model = chatgpt_subscription_endpoint("chatgpt-tools", client)
        .expect("subscription endpoint should build")
        .model("gpt-test")
        .expect("model should build");

    let response = complete(
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
    assert_eq!(first_requests.len(), 1);
    let first_body: Value =
        serde_json::from_slice(&first_requests[0].body).expect("tool request body should be JSON");
    assert_eq!(first_body["tools"][0]["name"], "inspect_path");
    assert_eq!(first_body["tools"][0]["strict"], true);
    assert_eq!(first_body["tool_choice"]["name"], "inspect_path");
}

fn user(text: &str) -> InputItem {
    InputItem::external(InputSource::User, text)
}

fn header<'a>(request: &'a CapturedHttpRequest, name: &str) -> Option<&'a str> {
    request
        .headers
        .get(name)
        .and_then(|value| value.to_str().ok())
}

fn inspect_path() -> ToolDefinition {
    ToolDefinition::new(
        "inspect_path",
        "Inspect one path.",
        json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"],
            "additionalProperties": false
        }),
    )
}
