//! OpenAI Chat Completions endpoint construction.
//!
//! This module implements the `/chat/completions` wire contract. It is
//! intentionally separate from [`super::openai_responses`]: compatible
//! services must choose the protocol they actually implement.

use std::fmt::Debug;

use rig_core::{
    client::CompletionClient, http_client::HttpClientExt, providers::openai as rig_openai,
};

use crate::llm::{
    ConfigError, Endpoint, Protocol,
    protocol::{no_redirect_http_client, validate_base_url},
};

/// Configure the official OpenAI Chat Completions endpoint.
pub fn official(
    endpoint_id: impl Into<String>,
    api_key: impl Into<String>,
) -> Result<Endpoint, ConfigError> {
    let api_key = api_key.into();
    validate_api_key(&api_key)?;

    let client = rig_openai::CompletionsClient::builder()
        .api_key(api_key)
        .http_client(no_redirect_http_client()?)
        .build()
        .map_err(|_| ConfigError::InvalidApiKey)?;
    from_client(endpoint_id, client)
}

/// Configure an endpoint implementing the OpenAI Chat Completions protocol.
pub fn compatible(
    endpoint_id: impl Into<String>,
    api_key: impl Into<String>,
    base_url: impl Into<String>,
) -> Result<Endpoint, ConfigError> {
    compatible_with_http_client(endpoint_id, api_key, base_url, no_redirect_http_client()?)
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
pub(crate) fn from_client<H>(
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

/// Internal seam for exact Chat Completions model construction.
pub(crate) fn from_model_factory<F, H>(
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
    use futures_util::StreamExt;
    use serde_json::Value;

    use super::*;
    use crate::llm::{
        InputItem, InputSource, Request, StreamEvent,
        protocol::test_client::ScriptedStreamingClient,
    };

    const TEXT_STREAM: &str = r#"data: {"id":"chatcmpl_test_1","object":"chat.completion.chunk","created":0,"model":"chat-test","choices":[{"index":0,"delta":{"role":"assistant","content":"ok"},"finish_reason":null}],"usage":null}

data: {"id":"chatcmpl_test_1","object":"chat.completion.chunk","created":0,"model":"chat-test","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":null}

data: {"id":"chatcmpl_test_1","object":"chat.completion.chunk","created":0,"model":"chat-test","choices":[],"usage":{"prompt_tokens":2,"completion_tokens":1,"total_tokens":3}}

data: [DONE]

"#;

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
        let transport = ScriptedStreamingClient::sse(TEXT_STREAM);
        let endpoint = compatible_with_http_client(
            "chat-gateway",
            "test-only-key",
            "https://gateway.example/v1/",
            transport.clone(),
        )
        .unwrap();

        let mut stream = endpoint
            .model("chat-test")
            .unwrap()
            .stream(
                Request::new([InputItem::external(InputSource::User, "hello")])
                    .instructions("Answer briefly."),
            )
            .await
            .unwrap();
        let mut completed = false;
        while let Some(event) = stream.next().await {
            if matches!(event.unwrap(), StreamEvent::Completed(_)) {
                completed = true;
            }
        }
        assert!(
            completed,
            "the fixture stream must reach a terminal response"
        );

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
        // BONE sends no output bound of its own.
        assert!(body.get("max_tokens").is_none());
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
