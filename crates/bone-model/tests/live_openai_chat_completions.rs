use bone_model::{
    Protocol, StreamedAssistantContent, protocol::openai_chat_completions, rig::message::Message,
};
use futures_util::StreamExt;

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY, BONE_OPENAI_CHAT_MODEL, network access, and API billing"]
async fn openai_chat_completions_live_certification() {
    let api_key = std::env::var("OPENAI_API_KEY")
        .expect("set OPENAI_API_KEY before running the ignored live test");
    let model_id = std::env::var("BONE_OPENAI_CHAT_MODEL")
        .expect("set BONE_OPENAI_CHAT_MODEL before running the ignored live test");
    let endpoint = match std::env::var("OPENAI_BASE_URL") {
        Ok(base_url) if !base_url.trim().is_empty() => {
            openai_chat_completions::compatible("openai-chat-live", api_key, base_url)
        }
        _ => openai_chat_completions::official("openai-chat-live", api_key),
    }
    .expect("OpenAI Chat Completions endpoint should build");
    let model = endpoint
        .model(&model_id)
        .expect("OpenAI Chat Completions model should build");
    let mut stream = model
        .request(Message::user("Reply with one short greeting."))
        .preamble("Be concise.".to_owned())
        .max_tokens(64)
        .stream()
        .await
        .expect("OpenAI Chat Completions request should succeed");
    let mut text = String::new();
    let mut finals = 0;

    while let Some(item) = stream.next().await {
        match item.expect("OpenAI Chat Completions stream item should be valid") {
            StreamedAssistantContent::Text(delta) => text.push_str(&delta.text),
            StreamedAssistantContent::Final(_) => finals += 1,
            _ => {}
        }
    }

    assert!(!text.trim().is_empty(), "live response should contain text");
    assert_eq!(finals, 1, "live stream should contain one terminal event");
    let terminal = stream
        .response
        .expect("live stream needs a terminal record");
    assert_eq!(model.endpoint_id(), "openai-chat-live");
    assert_eq!(model.protocol(), Protocol::OpenAiChatCompletions);
    assert_eq!(terminal.provider, "openai");
    assert!(
        terminal
            .model
            .as_deref()
            .is_some_and(|model| !model.trim().is_empty()),
        "live terminal record should identify the resolved model"
    );
    assert!(
        terminal.usage.has_values(),
        "live terminal record should report token usage"
    );
}
