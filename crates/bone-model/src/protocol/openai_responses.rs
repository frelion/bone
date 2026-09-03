//! OpenAI Responses protocol construction using Rig's native types.

use std::fmt::Debug;

use rig_core::{
    client::CompletionClient, completion::CompletionModel, http_client::HttpClientExt,
    providers::openai as rig_openai,
};
use serde_json::{Value, json};

use crate::{ConfigError, Endpoint, Protocol, protocol::validate_base_url};

pub use rig_core::providers::openai::responses_api::{
    Reasoning, ReasoningContext, ReasoningEffort, ReasoningMode, ReasoningSummaryLevel,
};

/// Configure the official OpenAI Responses endpoint.
pub fn official(
    endpoint_id: impl Into<String>,
    api_key: impl Into<String>,
) -> Result<Endpoint, ConfigError> {
    let api_key = api_key.into();
    validate_api_key(&api_key)?;

    let client = rig_openai::Client::new(api_key).map_err(|_| ConfigError::InvalidApiKey)?;
    from_client(endpoint_id, client)
}

/// Configure an endpoint implementing the OpenAI Responses wire protocol.
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

    let client = rig_openai::Client::builder()
        .api_key(api_key)
        .base_url(base_url)
        .http_client(http_client)
        .build()
        .map_err(|_| ConfigError::InvalidApiKey)?;
    from_client(endpoint_id, client)
}

/// Wrap a Responses client configured with custom headers, URL, or transport.
///
/// The transport type is erased when the returned endpoint constructs a
/// [`crate::Model`], keeping generic HTTP details out of runtime code.
pub fn from_client<H>(
    endpoint_id: impl Into<String>,
    client: rig_openai::Client<H>,
) -> Result<Endpoint, ConfigError>
where
    H: HttpClientExt + Clone + Default + Debug + Send + Sync + 'static,
{
    from_model_factory(endpoint_id, move |model_id| {
        client.completion_model(model_id)
    })
}

/// Build a Responses endpoint from a custom Rig Responses model factory.
///
/// This protocol-specific escape hatch supports model-level Rig options such
/// as strict tools or alternate system-instruction placement without mirroring
/// those options in a generic BONE configuration bag. The concrete return type
/// prevents a Chat Completions or another provider's model from being
/// mislabeled as OpenAI Responses.
///
/// ```compile_fail
/// use bone_model::{
///     protocol::openai_responses,
///     rig::{client::CompletionClient, providers::anthropic},
/// };
///
/// let client = anthropic::Client::new("test-only-key").unwrap();
/// let _ = openai_responses::from_model_factory("wrong-protocol", move |model_id| {
///     client.completion_model(model_id)
/// });
/// ```
pub fn from_model_factory<F, H>(
    endpoint_id: impl Into<String>,
    factory: F,
) -> Result<Endpoint, ConfigError>
where
    F: Fn(String) -> rig_openai::responses_api::ResponsesCompletionModel<H> + Send + Sync + 'static,
    H: HttpClientExt + Clone + Default + Debug + Send + Sync + 'static,
{
    from_completion_model_factory(endpoint_id, factory)
}

/// Assign the Responses protocol identity to another in-crate service adapter
/// whose concrete Rig model implements the same normalized wire contract.
///
/// This remains crate-private so public callers cannot label an arbitrary Rig
/// provider as OpenAI Responses.
pub(crate) fn from_completion_model_factory<F, M>(
    endpoint_id: impl Into<String>,
    factory: F,
) -> Result<Endpoint, ConfigError>
where
    F: Fn(String) -> M + Send + Sync + 'static,
    M: CompletionModel + Send + Sync + 'static,
{
    Endpoint::from_model_factory(endpoint_id, Protocol::OpenAiResponses, factory)
}

/// Encode Rig's typed OpenAI reasoning controls for
/// [`CompletionRequestBuilder::additional_params`](rig_core::completion::CompletionRequestBuilder::additional_params).
pub fn reasoning_params(reasoning: Reasoning) -> Value {
    json!({ "reasoning": reasoning })
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

    use super::*;

    const TEXT_RESPONSE: &str = r#"{
        "id":"resp_test_1",
        "object":"response",
        "created_at":0,
        "status":"completed",
        "model":"openai-test-model",
        "usage":{
            "input_tokens":1,
            "input_tokens_details":{"cached_tokens":0},
            "output_tokens":1,
            "output_tokens_details":{"reasoning_tokens":0},
            "total_tokens":2
        },
        "output":[{
            "type":"message",
            "id":"msg_test_1",
            "status":"completed",
            "role":"assistant",
            "content":[{"type":"output_text","annotations":[],"text":"ok"}]
        }],
        "tools":[]
    }"#;

    #[test]
    fn builds_an_official_responses_endpoint_without_network_io() {
        let endpoint = official("openai-primary", "test-only-key").unwrap();
        let model = endpoint.model("test-model").unwrap();

        assert_eq!(endpoint.id(), "openai-primary");
        assert_eq!(endpoint.protocol(), Protocol::OpenAiResponses);
        assert_eq!(model.endpoint_id(), "openai-primary");
        assert_eq!(model.protocol(), Protocol::OpenAiResponses);
        assert_eq!(model.id(), "test-model");
    }

    #[test]
    fn builds_a_compatible_endpoint_without_a_vendor_type() {
        let endpoint =
            compatible("gateway-a", "test-only-key", "https://gateway.example/v1").unwrap();
        let model = endpoint.model("vendor-model").unwrap();

        assert_eq!(endpoint.id(), "gateway-a");
        assert_eq!(endpoint.protocol(), Protocol::OpenAiResponses);
        assert_eq!(model.id(), "vendor-model");
    }

    #[tokio::test]
    async fn compatible_constructor_sets_the_real_wire_url_and_headers() {
        let transport = RecordingHttpClient::new(TEXT_RESPONSE);
        let endpoint = compatible_with_http_client(
            "gateway-a",
            "test-only-key",
            "https://gateway.example/v1",
            transport.clone(),
        )
        .unwrap();

        endpoint
            .model("openai-test-model")
            .unwrap()
            .request(Message::user("hello"))
            .max_tokens(8)
            .send()
            .await
            .unwrap();

        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].uri, "https://gateway.example/v1/responses");
        assert_eq!(
            requests[0]
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some("Bearer test-only-key")
        );
        assert_eq!(
            requests[0]
                .headers
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
    }

    #[test]
    fn rejects_credentials_without_echoing_them() {
        assert_eq!(
            official("openai-primary", "  ").unwrap_err(),
            ConfigError::EmptyApiKey
        );

        let credential = "secret\nheader";
        let error = official("openai-primary", credential).unwrap_err();
        assert_eq!(error, ConfigError::InvalidApiKey);
        assert!(!error.to_string().contains(credential));
    }

    #[test]
    fn rejects_invalid_endpoint_model_and_base_url_configuration() {
        assert_eq!(
            official("  ", "test-only-key").unwrap_err(),
            ConfigError::EmptyEndpointId
        );

        let endpoint = official("openai-primary", "test-only-key").unwrap();
        assert_eq!(endpoint.model("  ").unwrap_err(), ConfigError::EmptyModelId);

        assert_eq!(
            compatible("gateway-a", "test-only-key", "  ").unwrap_err(),
            ConfigError::EmptyBaseUrl
        );
        assert_eq!(
            compatible("gateway-a", "test-only-key", "gateway.example/v1").unwrap_err(),
            ConfigError::InvalidBaseUrl
        );
    }

    #[test]
    fn uses_rigs_typed_reasoning_controls() {
        let params = reasoning_params(
            Reasoning::new()
                .with_effort(ReasoningEffort::Max)
                .with_mode(ReasoningMode::Pro)
                .with_context(ReasoningContext::AllTurns),
        );

        assert_eq!(
            params,
            json!({
                "reasoning": {
                    "effort": "max",
                    "mode": "pro",
                    "context": "all_turns"
                }
            })
        );
    }
}
