use bone_provider::{StreamedAssistantContent, openai::OpenAi, rig::message::Message};
use futures_util::StreamExt;

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY, BONE_OPENAI_MODEL, network access, and API billing"]
async fn openai_responses_live_smoke() {
    let api_key = std::env::var("OPENAI_API_KEY")
        .expect("set OPENAI_API_KEY before running the ignored OpenAI smoke test");
    let model_id = std::env::var("BONE_OPENAI_MODEL")
        .expect("set BONE_OPENAI_MODEL before running the ignored OpenAI smoke test");
    let provider = match std::env::var("OPENAI_BASE_URL") {
        Ok(base_url) => OpenAi::compatible(api_key, base_url),
        Err(_) => OpenAi::new(api_key),
    };
    let model = provider
        .expect("OpenAI client should build")
        .model(model_id)
        .expect("OpenAI Responses model should build");
    let mut stream = model
        .request(Message::user("Reply with exactly: ok"))
        .preamble("Be concise and follow the requested output exactly.".to_owned())
        .max_tokens(128)
        .stream()
        .await
        .expect("OpenAI Responses request should succeed");
    let mut saw_text = false;
    let mut saw_final = false;

    while let Some(item) = stream.next().await {
        match item.expect("OpenAI stream item should be valid") {
            StreamedAssistantContent::Text(text) if !text.text.trim().is_empty() => {
                saw_text = true;
            }
            StreamedAssistantContent::Final(_) => saw_final = true,
            _ => {}
        }
    }

    assert!(saw_text);
    assert!(saw_final);
    assert!(stream.response.is_some());
}
