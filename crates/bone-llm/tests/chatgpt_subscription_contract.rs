use bone_llm::{
    ErrorKind, FinishReason, InputItem, InputSource, OutputFormat, OutputItem, Protocol, Request,
    StreamEvent, ToolChoice, ToolDefinition, ToolOutput, testing::chatgpt_subscription_endpoint,
};
use futures_util::StreamExt;
use http::StatusCode;
use rig_core::{
    providers::chatgpt::{self as rig_chatgpt, ChatGPTAuth},
    test_utils::{
        CapturedHttpRequest, HttpErrorStreamingClient, RecordingHttpClient,
        SequencedStreamingHttpClient,
    },
};
use serde_json::{Value, json};

const TEXT_SSE: &str = r#"data: {"type":"response.output_text.delta","delta":"ok"}

data: {"type":"response.completed","response":{"id":"resp_text_1","object":"response","created_at":1,"status":"completed","error":null,"incomplete_details":null,"instructions":null,"max_output_tokens":null,"model":"gpt-test","usage":{"input_tokens":2,"input_tokens_details":{"cached_tokens":0},"output_tokens":1,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":3},"output":[{"type":"message","id":"msg_text_1","status":"completed","role":"assistant","content":[{"type":"output_text","annotations":[],"text":"ok"}]}],"tools":[]}}

data: [DONE]"#;

const TOOL_SSE: &str = r#"data: {"type":"response.completed","response":{"id":"resp_tool_1","object":"response","created_at":1,"status":"completed","error":null,"incomplete_details":null,"instructions":null,"max_output_tokens":null,"model":"gpt-test","usage":{"input_tokens":8,"input_tokens_details":{"cached_tokens":0},"output_tokens":4,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":12},"output":[{"type":"function_call","id":"fc_test_1","call_id":"call_test_1","name":"inspect_path","arguments":"{\"path\":\"/tmp/bone\"}","status":"completed"}],"tools":[]}}

data: [DONE]"#;

const STREAM_SSE: &str = include_str!("fixtures/openai_responses/text_stream.sse");

fn test_client(
    body: &'static str,
) -> (
    rig_chatgpt::Client<RecordingHttpClient>,
    RecordingHttpClient,
) {
    let transport = RecordingHttpClient::new(body);
    let client = rig_chatgpt::Client::builder()
        .api_key(ChatGPTAuth::AccessToken {
            access_token: "sentinel-secret-token".to_owned(),
            account_id: Some("acct_test".to_owned()),
        })
        .base_url("https://chatgpt.example/backend-api/codex")
        .http_client(transport.clone())
        .default_instructions("")
        .originator("bone")
        .user_agent("bone-llm/test")
        .build()
        .expect("test ChatGPT client should build");
    (client, transport)
}

fn default_url_test_client(
    body: &'static str,
) -> (
    rig_chatgpt::Client<RecordingHttpClient>,
    RecordingHttpClient,
) {
    let transport = RecordingHttpClient::new(body);
    let client = rig_chatgpt::Client::builder()
        .api_key(ChatGPTAuth::AccessToken {
            access_token: "sentinel-secret-token".to_owned(),
            account_id: Some("acct_test".to_owned()),
        })
        .http_client(transport.clone())
        .default_instructions("")
        .originator("bone")
        .user_agent("bone-llm/test")
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

    model
        .complete(Request::new([user("hello")]))
        .await
        .expect("recorded ChatGPT request should normalize");

    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].uri,
        "https://chatgpt.com/backend-api/codex/responses"
    );
}

#[tokio::test]
async fn redacts_authentication_and_stream_provider_bodies() {
    let unary_secret = "sentinel-secret-unary-401";
    let unary_transport =
        RecordingHttpClient::with_error_response(StatusCode::UNAUTHORIZED, unary_secret);
    let unary_client = rig_chatgpt::Client::builder()
        .api_key(ChatGPTAuth::AccessToken {
            access_token: "rejected-test-token".to_owned(),
            account_id: Some("acct_test".to_owned()),
        })
        .http_client(unary_transport)
        .build()
        .unwrap();
    let unary_model = chatgpt_subscription_endpoint("chatgpt-unary-error", unary_client)
        .unwrap()
        .model("gpt-test")
        .unwrap();
    let unary_error = unary_model
        .complete(Request::new([user("hello")]))
        .await
        .unwrap_err();
    let rendered = format!("{unary_error:?}: {unary_error}");
    assert!(rendered.contains("reconnect"));
    assert!(!rendered.contains(unary_secret));

    let handshake_secret = "sentinel-secret-stream-401";
    let stream_transport =
        HttpErrorStreamingClient::new(StatusCode::UNAUTHORIZED, handshake_secret);
    let stream_client = rig_chatgpt::Client::builder()
        .api_key(ChatGPTAuth::AccessToken {
            access_token: "rejected-test-token".to_owned(),
            account_id: Some("acct_test".to_owned()),
        })
        .http_client(stream_transport)
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
    let envelope_transport = SequencedStreamingHttpClient::new(vec![Ok(bytes::Bytes::from(body))]);
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
async fn maps_subscription_to_responses_and_rejects_unsupported_controls() {
    let (client, transport) = test_client(TEXT_SSE);
    let debug = format!("{client:?}");
    assert!(!debug.contains("sentinel-secret-token"));

    let endpoint =
        chatgpt_subscription_endpoint("chatgpt-test", client).expect("endpoint should build");
    let model = endpoint.model("gpt-test").expect("model should build");

    let unsupported = model
        .complete(Request::new([user("hello")]).max_output_tokens(64))
        .await
        .expect_err("subscription endpoint must reject unsupported max_output_tokens");
    assert_eq!(unsupported.kind(), ErrorKind::UnsupportedOption);
    assert!(transport.requests().is_empty());

    let unsupported = model
        .complete(
            Request::new([user("hello")]).output(OutputFormat::JsonSchema(json!({
                "type": "object"
            }))),
        )
        .await
        .expect_err("subscription endpoint must reject unsupported structured output");
    assert_eq!(unsupported.kind(), ErrorKind::UnsupportedOption);
    assert!(transport.requests().is_empty());

    let response = model
        .complete(Request::new([user("hello")]).instructions("Answer briefly."))
        .await
        .expect("ChatGPT SSE should normalize");

    assert_eq!(endpoint.protocol(), Protocol::OpenAiResponses);
    assert_eq!(model.protocol(), Protocol::OpenAiResponses);
    assert_eq!(model.endpoint_id(), "chatgpt-test");
    assert_eq!(response.origin().provider(), "chatgpt");
    assert!(matches!(response.items(), [OutputItem::Text(text)] if text == "ok"));

    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
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
    let transport = SequencedStreamingHttpClient::new(vec![Ok(bytes::Bytes::from_static(
        STREAM_SSE.as_bytes(),
    ))]);
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
async fn preserves_tool_calls_and_replays_their_provider_ids_opaquely() {
    let (client, transport) = test_client(TOOL_SSE);
    let model = chatgpt_subscription_endpoint("chatgpt-tools", client)
        .expect("subscription endpoint should build")
        .model("gpt-test")
        .expect("model should build");

    let response = model
        .complete(
            Request::new([user("inspect the path")])
                .tools([inspect_path()])
                .tool_choice(ToolChoice::Specific(vec!["inspect_path".to_owned()])),
        )
        .await
        .expect("tool SSE should normalize");

    assert_eq!(response.finish_reason(), Some(&FinishReason::ToolCalls));
    let call = response
        .tool_calls()
        .next()
        .expect("one tool call should be exposed")
        .clone();
    assert_eq!(call.name(), "inspect_path");
    assert_eq!(call.arguments()["path"], "/tmp/bone");

    let first_body: Value = serde_json::from_slice(&transport.requests()[0].body)
        .expect("tool request body should be JSON");
    assert_eq!(first_body["tools"][0]["name"], "inspect_path");
    assert_eq!(first_body["tool_choice"]["name"], "inspect_path");

    let replay = response
        .into_item()
        .expect("tool response should be replayable as one opaque item");
    let (second_client, second_transport) = test_client(TEXT_SSE);
    let second_model = chatgpt_subscription_endpoint("chatgpt-tools", second_client)
        .expect("subscription endpoint should build")
        .model("gpt-test")
        .expect("model should build");

    second_model
        .complete(Request::new([
            user("inspect the path"),
            replay,
            InputItem::tool_result(&call, ToolOutput::text("path is a directory")),
        ]))
        .await
        .expect("tool result replay should normalize");

    let second_body: Value = serde_json::from_slice(&second_transport.requests()[0].body)
        .expect("replay request body should be JSON");
    let input = second_body["input"]
        .as_array()
        .expect("input should be an array");
    let call = input
        .iter()
        .find(|item| item["type"] == "function_call")
        .expect("function call should be replayed");
    let output = input
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .expect("function result should be replayed");
    assert_eq!(call["id"], "fc_test_1");
    assert_eq!(call["call_id"], "call_test_1");
    assert_eq!(output["call_id"], "call_test_1");
    assert_eq!(output["output"], "path is a directory");
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
