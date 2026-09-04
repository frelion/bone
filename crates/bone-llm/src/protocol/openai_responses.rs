//! OpenAI Responses endpoint construction and typed request options.

use std::fmt::Debug;

use rig_core::{
    client::CompletionClient, completion::CompletionModel, http_client::HttpClientExt,
    providers::openai as rig_openai,
};
use serde_json::{Value, json};

use crate::{ConfigError, Endpoint, Protocol, protocol::validate_base_url};

/// Typed controls supported only by the OpenAI Responses protocol.
#[derive(Clone, Debug, Default)]
pub struct Options {
    reasoning: Option<Reasoning>,
}

impl Options {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reasoning(mut self, reasoning: Reasoning) -> Self {
        self.reasoning = Some(reasoning);
        self
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.reasoning.as_ref().is_none_or(Reasoning::is_empty)
    }

    pub(crate) fn into_json(self) -> Option<Value> {
        self.reasoning
            .map(|reasoning| json!({ "reasoning": reasoning.into_json() }))
    }
}

/// Reasoning controls for OpenAI Responses models.
#[derive(Clone, Debug, Default)]
pub struct Reasoning {
    effort: Option<ReasoningEffort>,
    summary: Option<ReasoningSummary>,
    mode: Option<ReasoningMode>,
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

    fn is_empty(&self) -> bool {
        self.effort.is_none()
            && self.summary.is_none()
            && self.mode.is_none()
            && self.context.is_none()
    }

    fn into_json(self) -> Value {
        let mut value = serde_json::Map::new();
        if let Some(effort) = self.effort {
            value.insert(
                "effort".to_owned(),
                Value::String(effort.as_str().to_owned()),
            );
        }
        if let Some(summary) = self.summary {
            value.insert(
                "summary".to_owned(),
                Value::String(summary.as_str().to_owned()),
            );
        }
        if let Some(mode) = self.mode {
            value.insert("mode".to_owned(), Value::String(mode.as_str().to_owned()));
        }
        if let Some(context) = self.context {
            value.insert(
                "context".to_owned(),
                Value::String(context.as_str().to_owned()),
            );
        }
        Value::Object(value)
    }
}

macro_rules! string_enum {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum $name { $($variant),+ }

        impl $name {
            const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $value),+ }
            }
        }
    };
}

string_enum!(ReasoningEffort {
    None => "none",
    Minimal => "minimal",
    Low => "low",
    Medium => "medium",
    High => "high",
    Xhigh => "xhigh",
    Max => "max",
});
string_enum!(ReasoningSummary {
    Auto => "auto",
    Concise => "concise",
    Detailed => "detailed",
});
string_enum!(ReasoningMode { Pro => "pro" });
string_enum!(ReasoningContext {
    Auto => "auto",
    AllTurns => "all_turns",
    CurrentTurn => "current_turn",
});

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
pub(crate) fn from_client<H>(
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
    use rig_core::test_utils::RecordingHttpClient;

    use crate::{InputItem, InputSource, Request};

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
            .complete(
                Request::new([InputItem::external(InputSource::User, "hello")])
                    .max_output_tokens(8),
            )
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
    fn serializes_bone_typed_reasoning_controls() {
        let params = Options::new()
            .reasoning(
                Reasoning::new()
                    .effort(ReasoningEffort::Max)
                    .mode(ReasoningMode::Pro)
                    .context(ReasoningContext::AllTurns),
            )
            .into_json()
            .unwrap();

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
