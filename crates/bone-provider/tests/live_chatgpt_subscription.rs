use bone_provider::{
    Protocol, StreamedAssistantContent, rig::message::Message, service::chatgpt_subscription,
};
use futures_util::StreamExt;
use std::time::Duration;

#[tokio::test]
#[ignore = "requires BONE_CHATGPT_MODEL, network access, and an interactive or cached ChatGPT subscription login"]
async fn chatgpt_subscription_live_certification() {
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

    assert!(!text.trim().is_empty());
    assert_eq!(finals, 1);
    let terminal = stream
        .response
        .expect("complete stream needs terminal record");
    assert_eq!(endpoint.protocol(), Protocol::OpenAiResponses);
    assert_eq!(model.protocol(), Protocol::OpenAiResponses);
    assert_eq!(terminal.provider, "chatgpt");
    assert!(terminal.usage.has_values());
}
