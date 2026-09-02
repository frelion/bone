//! OpenAI Responses construction using Rig's native types.

use std::fmt;

use rig_core::{client::CompletionClient, providers::openai as rig_openai};
use serde_json::{Value, json};

use crate::Model;

pub use rig_core::providers::openai::responses_api::{
    Reasoning, ReasoningContext, ReasoningEffort, ReasoningMode, ReasoningSummaryLevel,
};

/// A configured Rig client for the OpenAI Responses protocol.
#[derive(Clone)]
pub struct OpenAi {
    client: rig_openai::Client,
}

impl OpenAi {
    /// Create a client for the official OpenAI Responses endpoint.
    pub fn new(api_key: impl Into<String>) -> Result<Self, ConfigError> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(ConfigError::EmptyApiKey);
        }

        let client = rig_openai::Client::new(api_key).map_err(|_| ConfigError::InvalidApiKey)?;
        Ok(Self { client })
    }

    /// Create a client for an OpenAI Responses-compatible endpoint.
    pub fn compatible(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Result<Self, ConfigError> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(ConfigError::EmptyApiKey);
        }

        let base_url = base_url.into();
        validate_base_url(&base_url)?;

        let client = rig_openai::Client::builder()
            .api_key(api_key)
            .base_url(base_url)
            .build()
            .map_err(|_| ConfigError::InvalidApiKey)?;
        Ok(Self { client })
    }

    /// Wrap a Rig client configured with custom headers or a base URL.
    pub fn from_client(client: rig_openai::Client) -> Self {
        Self { client }
    }

    /// Select an OpenAI Responses model.
    pub fn model(&self, id: impl Into<String>) -> Result<Model, ConfigError> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err(ConfigError::EmptyModelId);
        }

        Ok(Model::new(id.clone(), self.client.completion_model(id)))
    }
}

/// Encode Rig's typed OpenAI reasoning controls for
/// [`CompletionRequestBuilder::additional_params`](rig_core::completion::CompletionRequestBuilder::additional_params).
pub fn reasoning_params(reasoning: Reasoning) -> Value {
    json!({ "reasoning": reasoning })
}

/// Safe, local Responses-client construction failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    EmptyApiKey,
    InvalidApiKey,
    EmptyBaseUrl,
    InvalidBaseUrl,
    EmptyModelId,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EmptyApiKey => "API key is empty",
            Self::InvalidApiKey => "API key is not a valid authorization header",
            Self::EmptyBaseUrl => "Responses-compatible base URL is empty",
            Self::InvalidBaseUrl => "Responses-compatible base URL must be an absolute HTTP URL",
            Self::EmptyModelId => "model identifier is empty",
        })
    }
}

impl std::error::Error for ConfigError {}

fn validate_base_url(base_url: &str) -> Result<(), ConfigError> {
    if base_url.trim().is_empty() {
        return Err(ConfigError::EmptyBaseUrl);
    }

    let uri = base_url
        .parse::<rig_core::http_client::Uri>()
        .map_err(|_| ConfigError::InvalidBaseUrl)?;
    let is_http = matches!(uri.scheme_str(), Some("http" | "https"));
    if !is_http || uri.authority().is_none() || uri.query().is_some() {
        return Err(ConfigError::InvalidBaseUrl);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_type_erased_responses_model_without_network_io() {
        let openai = OpenAi::new("test-only-key").unwrap();
        let model = openai.model("test-model").unwrap();

        assert_eq!(model.id(), "test-model");
    }

    #[test]
    fn builds_a_compatible_endpoint_without_a_vendor_type() {
        let openai = OpenAi::compatible("test-only-key", "https://gateway.example/v1").unwrap();
        let model = openai.model("vendor-model").unwrap();

        assert_eq!(openai.client.base_url(), "https://gateway.example/v1");
        assert_eq!(model.id(), "vendor-model");
    }

    #[test]
    fn rejects_empty_or_invalid_credentials_without_echoing_them() {
        assert_eq!(OpenAi::new("  ").err(), Some(ConfigError::EmptyApiKey));

        let credential = "secret\nheader";
        let error = OpenAi::new(credential).err().unwrap();
        assert_eq!(error, ConfigError::InvalidApiKey);
        assert!(!error.to_string().contains(credential));
    }

    #[test]
    fn rejects_an_empty_model_identifier() {
        let openai = OpenAi::new("test-only-key").unwrap();
        let error = openai.model("  ").err().unwrap();

        assert_eq!(error, ConfigError::EmptyModelId);
    }

    #[test]
    fn rejects_invalid_compatible_base_urls() {
        assert_eq!(
            OpenAi::compatible("test-only-key", "  ").err(),
            Some(ConfigError::EmptyBaseUrl)
        );
        assert_eq!(
            OpenAi::compatible("test-only-key", "gateway.example/v1").err(),
            Some(ConfigError::InvalidBaseUrl)
        );
        assert_eq!(
            OpenAi::compatible("test-only-key", "https://gateway.example/v1?tenant=one").err(),
            Some(ConfigError::InvalidBaseUrl)
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
