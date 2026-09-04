use std::time::Duration;

use bone_llm::{
    FinishReason, InputItem, InputSource, Protocol, Request, StreamEvent, ToolChoice,
    ToolDefinition, ToolOutput, service::chatgpt_subscription,
};
use futures_util::StreamExt;
use serde_json::json;

#[tokio::test]
#[ignore = "requires BONE_CHATGPT_MODEL, network access, and an interactive or cached ChatGPT subscription login"]
async fn chatgpt_subscription_live_text_tool_and_replay_certification() {
    tokio::time::timeout(Duration::from_secs(5 * 60), run_live_certification())
        .await
        .expect("ChatGPT subscription certification exceeded five minutes");
}

async fn run_live_certification() {
    let model_id = std::env::var("BONE_CHATGPT_MODEL")
        .expect("set BONE_CHATGPT_MODEL before running the ignored live test");
    let credential_root = chatgpt_subscription::default_credential_root()
        .expect("default BONE credential root should resolve");
    let endpoint =
        chatgpt_subscription::connect("chatgpt-subscription-live", credential_root, |prompt| {
            println!("Authorize at {}", prompt.verification_uri);
            println!("Device code: {}", prompt.user_code);
        })
        .await
        .expect("ChatGPT subscription endpoint should build");
    let model = endpoint
        .model(model_id)
        .expect("ChatGPT subscription model should build");
    let mut stream = model
        .stream(
            Request::new([user("Reply with exactly: ok")])
                .instructions("Follow the requested output exactly."),
        )
        .await
        .expect("ChatGPT subscription request should succeed");
    let mut text = String::new();
    let mut completed = Vec::new();

    while let Some(event) = stream.next().await {
        match event.expect("ChatGPT stream item should be valid") {
            StreamEvent::TextDelta(delta) => text.push_str(&delta),
            StreamEvent::Completed(response) => completed.push(response),
            _ => {}
        }
    }

    assert_eq!(text.trim(), "ok");
    assert_eq!(completed.len(), 1);
    let terminal = completed.pop().unwrap();
    assert_eq!(endpoint.protocol(), Protocol::OpenAiResponses);
    assert_eq!(model.protocol(), Protocol::OpenAiResponses);
    assert_eq!(terminal.origin().provider(), "chatgpt");
    assert!(
        terminal
            .origin()
            .reported_model_id()
            .is_some_and(|model| !model.trim().is_empty()),
        "live terminal record should identify the resolved model"
    );
    assert!(terminal.usage().was_reported());

    let initial_user =
        user("Call inspect_path with path /tmp/bone. Do not answer before using the tool.");
    let response = model
        .complete(
            Request::new([initial_user.clone()])
                .tools([inspect_path()])
                .tool_choice(ToolChoice::Specific(vec!["inspect_path".to_owned()])),
        )
        .await
        .expect("ChatGPT subscription tool request should succeed");

    assert_eq!(response.finish_reason(), Some(&FinishReason::ToolCalls));
    let call = response
        .tool_calls()
        .next()
        .expect("expected one tool call")
        .clone();
    assert_eq!(call.name(), "inspect_path");
    assert_eq!(call.arguments()["path"], "/tmp/bone");

    let replay = response
        .into_item()
        .expect("live tool response should be replayable as one opaque item");
    let final_response = model
        .complete(
            Request::new([
                initial_user,
                replay,
                InputItem::tool_result(&call, ToolOutput::text("path is a directory")),
            ])
            .instructions("After the tool result, reply with exactly: done"),
        )
        .await
        .expect("ChatGPT subscription tool result replay should succeed");

    assert_eq!(
        final_response.text().as_deref().map(str::trim),
        Some("done")
    );
    assert!(
        final_response
            .origin()
            .reported_model_id()
            .is_some_and(|model| !model.trim().is_empty()),
        "live tool replay should identify the resolved model"
    );
    assert!(final_response.usage().was_reported());
}

fn user(text: &str) -> InputItem {
    InputItem::external(InputSource::User, text)
}

fn inspect_path() -> ToolDefinition {
    ToolDefinition::new(
        "inspect_path",
        "Inspect one path without accessing the real filesystem.",
        json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"],
            "additionalProperties": false
        }),
    )
}
