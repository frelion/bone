use bone_model::{
    Protocol, StreamedAssistantContent,
    rig::{
        completion::{FinishReason, ToolDefinition},
        message::{AssistantContent, Message, ToolChoice, ToolResultContent, UserContent},
    },
    service::chatgpt_subscription,
};
use futures_util::StreamExt;
use serde_json::json;
use std::time::Duration;

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
    let endpoint = chatgpt_subscription::connect("chatgpt-subscription-live", |prompt| {
        println!("Authorize at {}", prompt.verification_uri);
        println!("Device code: {}", prompt.user_code);
    })
    .await
    .expect("ChatGPT subscription endpoint should build");
    let model = endpoint
        .model(model_id)
        .expect("ChatGPT subscription model should build");
    let mut stream = model
        .request(Message::user("Reply with exactly: ok"))
        .preamble("Follow the requested output exactly.".to_owned())
        .stream()
        .await
        .expect("ChatGPT subscription request should succeed");
    let mut text = String::new();
    let mut finals = 0;

    while let Some(item) = stream.next().await {
        match item.expect("ChatGPT stream item should be valid") {
            StreamedAssistantContent::Text(delta) => text.push_str(&delta.text),
            StreamedAssistantContent::Final(_) => finals += 1,
            _ => {}
        }
    }

    assert_eq!(text.trim(), "ok");
    assert_eq!(finals, 1);
    let terminal = stream
        .response
        .expect("complete stream needs terminal record");
    assert_eq!(endpoint.protocol(), Protocol::OpenAiResponses);
    assert_eq!(model.protocol(), Protocol::OpenAiResponses);
    assert_eq!(terminal.provider, "chatgpt");
    assert!(
        terminal
            .model
            .as_deref()
            .is_some_and(|model| !model.trim().is_empty()),
        "live terminal record should identify the resolved model"
    );
    assert!(terminal.usage.has_values());

    let initial_user = Message::user(
        "Call inspect_path with path /tmp/bone. Do not answer before using the tool.",
    );
    let response = model
        .request(initial_user.clone())
        .tool(inspect_path())
        .tool_choice(ToolChoice::Specific {
            function_names: vec!["inspect_path".to_owned()],
        })
        .send()
        .await
        .expect("ChatGPT subscription tool request should succeed");

    assert_eq!(response.finish_reason(), Some(FinishReason::ToolCalls));
    let [AssistantContent::ToolCall(tool_call)] = response.choice.as_slice() else {
        panic!("expected one tool call, got {:?}", response.choice);
    };
    assert_eq!(tool_call.function.name, "inspect_path");
    assert_eq!(tool_call.function.arguments["path"], "/tmp/bone");
    assert!(
        tool_call
            .provider
            .as_ref()
            .is_some_and(|id| !id.call_id.trim().is_empty()),
        "live tool call should preserve the provider call id"
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
    let final_response = model
        .request(Message::User {
            content: vec![result],
        })
        .messages([initial_user, assistant])
        .preamble("After the tool result, reply with exactly: done".to_owned())
        .send()
        .await
        .expect("ChatGPT subscription tool result replay should succeed");

    let [AssistantContent::Text(text)] = final_response.choice.as_slice() else {
        panic!(
            "expected one final text response, got {:?}",
            final_response.choice
        );
    };
    assert_eq!(text.text.trim(), "done");
    assert!(
        final_response
            .model
            .as_deref()
            .is_some_and(|model| !model.trim().is_empty()),
        "live tool replay should identify the resolved model"
    );
    assert!(final_response.usage.has_values());
}

fn inspect_path() -> ToolDefinition {
    ToolDefinition {
        name: "inspect_path".to_owned(),
        description: "Inspect one path without accessing the real filesystem.".to_owned(),
        parameters: json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"],
            "additionalProperties": false
        }),
    }
}
