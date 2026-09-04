use bone_llm::{InputItem, InputSource, Request, StreamEvent, protocol::anthropic_messages};
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
        .stream(
            Request::new([InputItem::external(
                InputSource::User,
                "Reply with exactly: ok",
            )])
            .instructions("Be concise and follow the requested output exactly.")
            .max_output_tokens(32),
        )
        .await
        .expect("Anthropic Messages request should succeed");
    let mut text = String::new();
    let mut completed = Vec::new();

    while let Some(item) = stream.next().await {
        match item.expect("Anthropic stream item should be valid") {
            StreamEvent::TextDelta(delta) => text.push_str(&delta),
            StreamEvent::Completed(response) => completed.push(response),
            _ => {}
        }
    }

    assert!(!text.trim().is_empty());
    assert_eq!(completed.len(), 1);
    let terminal = completed.pop().unwrap();
    assert_eq!(terminal.origin().provider(), "anthropic");
    assert!(terminal.usage().was_reported());
    assert!(
        terminal
            .origin()
            .reported_model_id()
            .is_some_and(|model| !model.trim().is_empty())
    );
}
