use bone_model::{
    protocol::anthropic_messages,
    rig::{message::Message, streaming::StreamedAssistantContent},
};
use futures_util::StreamExt;

#[tokio::test]
#[ignore = "requires ANTHROPIC_API_KEY, BONE_ANTHROPIC_MODEL, network access, and API billing"]
async fn anthropic_messages_live_smoke() {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .expect("set ANTHROPIC_API_KEY before running the ignored Anthropic smoke test");
    let model_id = std::env::var("BONE_ANTHROPIC_MODEL")
        .expect("set BONE_ANTHROPIC_MODEL before running the ignored Anthropic smoke test");
    let endpoint = match std::env::var("ANTHROPIC_BASE_URL") {
        Ok(base_url) if !base_url.trim().is_empty() => {
            anthropic_messages::compatible("anthropic-live", api_key, base_url)
        }
        _ => anthropic_messages::official("anthropic-live", api_key),
    }
    .expect("Anthropic endpoint should build");
    let model = endpoint
        .model(model_id)
        .expect("Anthropic Messages model should build");
    let mut stream = model
        .request(Message::user("Reply with exactly: ok"))
        .preamble("Be concise and follow the requested output exactly.".to_owned())
        .max_tokens(32)
        .stream()
        .await
        .expect("Anthropic Messages request should succeed");
    let mut text = String::new();
    let mut final_count = 0;

    while let Some(item) = stream.next().await {
        match item.expect("Anthropic stream item should be valid") {
            StreamedAssistantContent::Text(delta) => text.push_str(&delta.text),
            StreamedAssistantContent::Final(_) => final_count += 1,
            _ => {}
        }
    }

    assert!(!text.trim().is_empty());
    assert_eq!(final_count, 1);
    let terminal = stream
        .response
        .expect("complete stream needs terminal record");
    assert_eq!(terminal.provider, "anthropic");
    assert!(terminal.usage.has_values());
    assert!(
        terminal
            .model
            .as_deref()
            .is_some_and(|model| !model.trim().is_empty())
    );
}
