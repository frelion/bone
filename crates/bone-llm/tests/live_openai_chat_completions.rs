use bone_llm::{
    InputItem, InputSource, Protocol, Request, StreamEvent, protocol::openai_chat_completions,
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
        .stream(
            Request::new([InputItem::external(
                InputSource::User,
                "Reply with one short greeting.",
            )])
            .instructions("Be concise.")
            .max_output_tokens(64),
        )
        .await
        .expect("OpenAI Chat Completions request should succeed");
    let mut text = String::new();
    let mut completed = Vec::new();

    while let Some(item) = stream.next().await {
        match item.expect("OpenAI Chat Completions stream item should be valid") {
            StreamEvent::TextDelta(delta) => text.push_str(&delta),
            StreamEvent::Completed(response) => completed.push(response),
            _ => {}
        }
    }

    assert!(!text.trim().is_empty(), "live response should contain text");
    assert_eq!(
        completed.len(),
        1,
        "live stream should contain one terminal event"
    );
    let terminal = completed.pop().unwrap();
    assert_eq!(model.endpoint_id(), "openai-chat-live");
    assert_eq!(model.protocol(), Protocol::OpenAiChatCompletions);
    assert_eq!(terminal.origin().provider(), "openai");
    assert!(
        terminal
            .origin()
            .reported_model_id()
            .is_some_and(|model| !model.trim().is_empty()),
        "live terminal record should identify the resolved model"
    );
    assert!(
        terminal.usage().was_reported(),
        "live terminal record should report token usage"
    );
}
