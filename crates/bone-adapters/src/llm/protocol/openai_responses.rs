//! OpenAI Responses endpoint construction and typed reasoning controls.

use std::fmt::Debug;

use rig_core::{
    client::CompletionClient, completion::CompletionModel, http_client::HttpClientExt,
    providers::openai as rig_openai,
};
use serde::{Deserialize, Serialize};

use crate::llm::{
    ConfigError, Endpoint, Protocol,
    protocol::{no_redirect_http_client, validate_base_url},
};

/// Reasoning controls for OpenAI Responses models.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Reasoning {
    #[serde(skip_serializing_if = "Option::is_none")]
    effort: Option<ReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<ReasoningSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<ReasoningMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<ReasoningContext>,
}

impl Reasoning {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn effort(mut self, effort: ReasoningEffort) -> Self {
        self.effort = Some(effort);
        self
    }

    pub fn summary(mut self, summary: ReasoningSummary) -> Self {
        self.summary = Some(summary);
        self
    }

    pub fn mode(mut self, mode: ReasoningMode) -> Self {
        self.mode = Some(mode);
        self
    }

    pub fn context(mut self, context: ReasoningContext) -> Self {
        self.context = Some(context);
        self
    }

    pub fn effort_level(&self) -> Option<ReasoningEffort> {
        self.effort
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.effort.is_none()
            && self.summary.is_none()
            && self.mode.is_none()
            && self.context.is_none()
    }
}

macro_rules! string_enum {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }
    };
}

string_enum!(ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
});

impl ReasoningEffort {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}
string_enum!(ReasoningSummary {
    Auto,
    Concise,
    Detailed,
});
string_enum!(ReasoningMode { Pro });
string_enum!(ReasoningContext {
    Auto,
    AllTurns,
    CurrentTurn,
});

/// Configure the official OpenAI Responses endpoint.
pub fn official(
    endpoint_id: impl Into<String>,
    api_key: impl Into<String>,
) -> Result<Endpoint, ConfigError> {
    let api_key = api_key.into();
    validate_api_key(&api_key)?;

    let client = rig_openai::Client::builder()
        .api_key(api_key)
        .http_client(no_redirect_http_client()?)
        .build()
        .map_err(|_| ConfigError::InvalidApiKey)?;
    from_client(endpoint_id, client)
}

/// Configure an endpoint implementing the OpenAI Responses wire protocol.
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
/// [`crate::llm::Model`], keeping generic HTTP details out of runtime code.
pub(crate) fn from_client<H>(
    endpoint_id: impl Into<String>,
    client: rig_openai::Client<H>,
) -> Result<Endpoint, ConfigError>
where
    H: HttpClientExt + Clone + Default + Debug + Send + Sync + 'static,
{
    from_model_factory(endpoint_id, move |model_id| {
        client.completion_model(model_id).with_strict_tools()
    })
}

/// Internal seam for exact Responses model construction.
pub(crate) fn from_model_factory<F, H>(
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
    use serde_json::json;

    use crate::llm::{
        InputItem, InputSource, Request, StreamEvent,
        protocol::test_client::ScriptedStreamingClient,
    };

    use super::*;

    const TEXT_STREAM: &str = r#"data: {"type":"response.output_text.delta","item_id":"msg_test_1","output_index":0,"content_index":0,"sequence_number":1,"delta":"ok"}

data: {"type":"response.completed","sequence_number":2,"response":{"id":"resp_test_1","object":"response","created_at":0,"status":"completed","model":"openai-test-model","usage":{"input_tokens":1,"input_tokens_details":{"cached_tokens":0},"output_tokens":1,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":2},"output":[{"type":"message","id":"msg_test_1","status":"completed","role":"assistant","content":[{"type":"output_text","annotations":[],"text":"ok"}]}],"tools":[]}}

"#;

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
        let transport = ScriptedStreamingClient::sse(TEXT_STREAM);
        let endpoint = compatible_with_http_client(
            "gateway-a",
            "test-only-key",
            "https://gateway.example/v1",
            transport.clone(),
        )
        .unwrap();

        let mut stream = endpoint
            .model("openai-test-model")
            .unwrap()
            .stream(Request::new([InputItem::external(
                InputSource::User,
                "hello",
            )]))
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
        assert_eq!(requests[0].uri, "https://gateway.example/v1/responses");
        // BONE sends no output bound of its own.
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert!(body.get("max_output_tokens").is_none());
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
    fn serializes_bone_typed_reasoning_controls() {
        let params = serde_json::to_value(
            Reasoning::new()
                .effort(ReasoningEffort::Max)
                .mode(ReasoningMode::Pro)
                .context(ReasoningContext::AllTurns),
        )
        .unwrap();

        assert_eq!(
            params,
            json!({
                "effort": "max",
                "mode": "pro",
                "context": "all_turns"
            })
        );
    }
}
