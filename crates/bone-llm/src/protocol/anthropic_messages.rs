//! Anthropic Messages protocol construction.
//!
//! A service that speaks this protocol is an [`Endpoint`], regardless of
//! vendor name. Use [`official`] for Anthropic's endpoint or [`compatible`] for
//! a Messages-compatible base URL.

use rig_core::{
    client::CompletionClient, http_client::HttpClientExt, providers::anthropic as rig_anthropic,
};

use crate::{ConfigError, Endpoint, Protocol};

/// Create an endpoint for Anthropic's official Messages API.
pub fn official(
    endpoint_id: impl Into<String>,
    api_key: impl Into<String>,
) -> Result<Endpoint, ConfigError> {
    let api_key = api_key.into();
    validate_api_key(&api_key)?;

    let client = rig_anthropic::Client::new(api_key).map_err(|_| ConfigError::InvalidApiKey)?;
    from_client(endpoint_id, client)
}

/// Create an endpoint for an Anthropic Messages-compatible base URL.
///
/// Rig normalizes a trailing `/v1`, `/messages`, or `/v1/messages`, then sends
/// requests to `/v1/messages` exactly once.
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
    H: HttpClientExt + Clone + Default + Send + Sync + 'static,
{
    let api_key = api_key.into();
    validate_api_key(&api_key)?;

    let base_url = base_url.into();
    super::validate_base_url(&base_url)?;

    let client = rig_anthropic::Client::builder()
        .api_key(api_key)
        .base_url(base_url)
        .http_client(http_client)
        .build()
        .map_err(|_| ConfigError::InvalidApiKey)?;
    from_client(endpoint_id, client)
}

/// Wrap a configured Rig Anthropic client and erase its transport type.
pub(crate) fn from_client<H>(
    endpoint_id: impl Into<String>,
    client: rig_anthropic::Client<H>,
) -> Result<Endpoint, ConfigError>
where
    H: HttpClientExt + Clone + Default + Send + Sync + 'static,
{
    from_model_factory(endpoint_id, move |model_id| {
        client.completion_model(model_id)
    })
}

/// Internal seam for exact Anthropic Messages model construction.
pub(crate) fn from_model_factory<F, H>(
    endpoint_id: impl Into<String>,
    factory: F,
) -> Result<Endpoint, ConfigError>
where
    F: Fn(String) -> rig_anthropic::completion::CompletionModel<H> + Send + Sync + 'static,
    H: HttpClientExt + Clone + Default + Send + Sync + 'static,
{
    Endpoint::from_model_factory(endpoint_id, Protocol::AnthropicMessages, factory)
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
    use rig_core::test_utils::RecordingHttpClient;

    use super::*;
    use crate::{InputItem, InputSource, Request};

    const TEXT_RESPONSE: &str = r#"{
        "id":"msg_test_1",
        "type":"message",
        "role":"assistant",
        "model":"claude-test",
        "content":[{"type":"text","text":"ok"}],
        "stop_reason":"end_turn",
        "stop_sequence":null,
        "usage":{"input_tokens":1,"output_tokens":1}
    }"#;

    #[test]
    fn builds_official_and_compatible_endpoints_without_network_io() {
        let official = official("anthropic-primary", "test-only-key").unwrap();
        let compatible = compatible(
            "anthropic-gateway",
            "test-only-key",
            "https://gateway.example/v1/messages",
        )
        .unwrap();

        assert_eq!(official.protocol(), Protocol::AnthropicMessages);
        assert_eq!(
            official.model("claude-test").unwrap().endpoint_id(),
            "anthropic-primary"
        );
        assert_eq!(compatible.protocol(), Protocol::AnthropicMessages);
        assert_eq!(
            compatible.model("gateway-model").unwrap().endpoint_id(),
            "anthropic-gateway"
        );
    }

    #[test]
    fn rejects_invalid_local_configuration_without_echoing_credentials() {
        assert_eq!(
            official("anthropic-primary", "  ").unwrap_err(),
            ConfigError::EmptyApiKey
        );

        let credential = "secret\nheader";
        let error = official("anthropic-primary", credential).unwrap_err();
        assert_eq!(error, ConfigError::InvalidApiKey);
        assert!(!error.to_string().contains(credential));

        assert_eq!(
            compatible("anthropic-gateway", "test-only-key", "  ").unwrap_err(),
            ConfigError::EmptyBaseUrl
        );
        assert_eq!(
            compatible("anthropic-gateway", "test-only-key", "gateway.example/v1",).unwrap_err(),
            ConfigError::InvalidBaseUrl
        );
    }

    #[tokio::test]
    async fn compatible_constructor_normalizes_the_real_wire_url_and_headers() {
        let transport = RecordingHttpClient::new(TEXT_RESPONSE);
        let endpoint = compatible_with_http_client(
            "anthropic-gateway",
            "test-only-key",
            "https://gateway.example/v1/messages/",
            transport.clone(),
        )
        .unwrap();

        endpoint
            .model("claude-test")
            .unwrap()
            .complete(
                Request::new([InputItem::external(InputSource::User, "hello")])
                    .max_output_tokens(8),
            )
            .await
            .unwrap();

        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].uri, "https://gateway.example/v1/messages");
        assert_eq!(
            requests[0]
                .headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok()),
            Some("test-only-key")
        );
        assert_eq!(
            requests[0]
                .headers
                .get("anthropic-version")
                .and_then(|value| value.to_str().ok()),
            Some("2023-06-01")
        );
    }
}
