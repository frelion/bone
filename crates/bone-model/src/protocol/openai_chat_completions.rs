//! OpenAI Chat Completions protocol construction using Rig's native types.
//!
//! This module implements the `/chat/completions` wire contract. It is
//! intentionally separate from [`super::openai_responses`]: compatible
//! services must choose the protocol they actually implement.

use std::fmt::Debug;

use rig_core::{
    client::CompletionClient, http_client::HttpClientExt, providers::openai as rig_openai,
};

use crate::{ConfigError, Endpoint, Protocol, protocol::validate_base_url};

/// Configure the official OpenAI Chat Completions endpoint.
pub fn official(
    endpoint_id: impl Into<String>,
    api_key: impl Into<String>,
) -> Result<Endpoint, ConfigError> {
    let api_key = api_key.into();
    validate_api_key(&api_key)?;

    let client =
        rig_openai::CompletionsClient::new(api_key).map_err(|_| ConfigError::InvalidApiKey)?;
    from_client(endpoint_id, client)
}

/// Configure an endpoint implementing the OpenAI Chat Completions protocol.
pub fn compatible(
    endpoint_id: impl Into<String>,
    api_key: impl Into<String>,
    base_url: impl Into<String>,
) -> Result<Endpoint, ConfigError> {
    compatible_with_http_client(
        endpoint_id,
        api_key,
        base_url,
        rig_core::http_client::ReqwestClient::default(),
    )
}

fn compatible_with_http_client<H>(
    endpoint_id: impl Into<String>,
    api_key: impl Into<String>,
    base_url: impl Into<String>,
    http_client: H,
) -> Result<Endpoint, ConfigError>
where
    H: HttpClientExt + Clone + Default + Debug + Send + Sync + 'static,
{
    let api_key = api_key.into();
    validate_api_key(&api_key)?;

    let base_url = base_url.into();
    validate_base_url(&base_url)?;

    let client = rig_openai::CompletionsClient::builder()
        .api_key(api_key)
        .base_url(base_url)
        .http_client(http_client)
        .build()
        .map_err(|_| ConfigError::InvalidApiKey)?;
    from_client(endpoint_id, client)
}

/// Wrap a Chat Completions client configured with custom headers, URL, or transport.
pub fn from_client<H>(
    endpoint_id: impl Into<String>,
    client: rig_openai::CompletionsClient<H>,
) -> Result<Endpoint, ConfigError>
where
    H: HttpClientExt + Clone + Default + Debug + Send + Sync + 'static,
{
    from_model_factory(endpoint_id, move |model_id| {
        client.completion_model(model_id)
    })
}

/// Build a Chat Completions endpoint from an exact Rig Chat Completions model factory.
///
/// This is the protocol-specific escape hatch for model-level Rig options such
/// as strict tools without mirroring them into a generic BONE configuration.
/// Its concrete return type prevents Responses or Anthropic models from being
/// mislabeled as Chat Completions.
///
/// An OpenAI Responses model is rejected at compile time:
///
/// ```compile_fail
/// use bone_model::{
///     protocol::openai_chat_completions,
///     rig::{client::CompletionClient, providers::openai},
/// };
///
/// let responses = openai::Client::new("test-only-key").unwrap();
/// let _ = openai_chat_completions::from_model_factory("wrong-protocol", move |model_id| {
///     responses.completion_model(model_id)
/// });
/// ```
///
/// An Anthropic Messages model is also rejected:
///
/// ```compile_fail
/// use bone_model::{
///     protocol::openai_chat_completions,
///     rig::{client::CompletionClient, providers::anthropic},
/// };
///
/// let anthropic = anthropic::Client::new("test-only-key").unwrap();
/// let _ = openai_chat_completions::from_model_factory("wrong-protocol", move |model_id| {
///     anthropic.completion_model(model_id)
/// });
/// ```
pub fn from_model_factory<F, H>(
    endpoint_id: impl Into<String>,
    factory: F,
) -> Result<Endpoint, ConfigError>
where
    F: Fn(String) -> rig_openai::completion::CompletionModel<H> + Send + Sync + 'static,
    H: HttpClientExt + Clone + Default + Debug + Send + Sync + 'static,
{
    Endpoint::from_model_factory(endpoint_id, Protocol::OpenAiChatCompletions, factory)
}

fn validate_api_key(api_key: &str) -> Result<(), ConfigError> {
    if api_key.trim().is_empty() {
        Err(ConfigError::EmptyApiKey)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rig_core::{message::Message, test_utils::RecordingHttpClient};
    use serde_json::Value;

    use super::*;

    const TEXT_RESPONSE: &str = r#"{
        "id":"chatcmpl_test_1",
        "object":"chat.completion",
        "created":0,
        "model":"chat-test",
        "system_fingerprint":null,
        "choices":[{
            "index":0,
            "message":{"role":"assistant","content":"ok"},
            "logprobs":null,
            "finish_reason":"stop"
        }],
        "usage":{"prompt_tokens":2,"completion_tokens":1,"total_tokens":3}
    }"#;

    #[test]
    fn builds_official_endpoint_without_network_io() {
        let endpoint = official("openai-chat-primary", "test-only-key").unwrap();
        let model = endpoint.model("chat-test").unwrap();

        assert_eq!(endpoint.id(), "openai-chat-primary");
        assert_eq!(endpoint.protocol(), Protocol::OpenAiChatCompletions);
        assert_eq!(model.endpoint_id(), "openai-chat-primary");
        assert_eq!(model.protocol(), Protocol::OpenAiChatCompletions);
        assert_eq!(model.id(), "chat-test");
    }

    #[tokio::test]
    async fn compatible_constructor_sends_the_chat_completions_wire_contract() {
        let transport = RecordingHttpClient::new(TEXT_RESPONSE);
        let endpoint = compatible_with_http_client(
            "chat-gateway",
            "test-only-key",
            "https://gateway.example/v1/",
            transport.clone(),
        )
        .unwrap();

        endpoint
            .model("chat-test")
            .unwrap()
            .request(Message::user("hello"))
            .preamble("Answer briefly.".to_owned())
            .max_tokens(16)
            .send()
            .await
            .unwrap();

        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.uri, "https://gateway.example/v1/chat/completions");
        assert_ne!(request.uri, "https://gateway.example/v1/responses");
        assert_eq!(
            request
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some("Bearer test-only-key")
        );
        assert_eq!(
            request
                .headers
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );

        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["model"], "chat-test");
        assert_eq!(body["max_tokens"], 16);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
        assert_eq!(body["messages"][0]["content"][0]["text"], "Answer briefly.");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "hello");
    }

    #[test]
    fn rejects_invalid_local_configuration_without_echoing_credentials() {
        assert_eq!(
            official("openai-chat-primary", "  ").unwrap_err(),
            ConfigError::EmptyApiKey
        );

        let credential = "secret\nheader";
        let error = official("openai-chat-primary", credential).unwrap_err();
        assert_eq!(error, ConfigError::InvalidApiKey);
        assert!(!error.to_string().contains(credential));

        assert_eq!(
            compatible("chat-gateway", "test-only-key", "  ").unwrap_err(),
            ConfigError::EmptyBaseUrl
        );
        assert_eq!(
            compatible("chat-gateway", "test-only-key", "gateway.example/v1").unwrap_err(),
            ConfigError::InvalidBaseUrl
        );
    }
}
