//! Persistable, non-secret LLM connection and model-request configuration.
//!
//! An application owns endpoint identities, credentials, and storage. This
//! module only describes the wire endpoint and controls that are safe to
//! persist alongside a model selection.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::llm::{
    ConfigError, Protocol,
    protocol::{openai_responses, validate_base_url},
};

/// The non-secret wire configuration of one LLM endpoint.
///
/// `None` for `base_url` selects the protocol's official endpoint. A present
/// base URL selects a compatible service implementing that exact protocol.
/// Endpoint identity, API keys, and ChatGPT OAuth state deliberately do not
/// belong here.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EndpointConfig {
    /// ChatGPT subscription access through the Codex Responses backend.
    ///
    /// This uses ChatGPT OAuth rather than an API key and always uses the
    /// OpenAI Responses wire protocol.
    #[serde(rename = "chatgpt_subscription")]
    ChatGptSubscription,
    /// OpenAI's Responses API, or a compatible implementation of it.
    #[serde(rename = "openai_responses")]
    OpenAiResponses {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_url: Option<String>,
    },
    /// OpenAI's Chat Completions API, or a compatible implementation of it.
    #[serde(rename = "openai_chat_completions")]
    OpenAiChatCompletions {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_url: Option<String>,
    },
    /// Anthropic's Messages API, or a compatible implementation of it.
    AnthropicMessages {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_url: Option<String>,
    },
}

impl EndpointConfig {
    /// The wire protocol implemented by this endpoint configuration.
    ///
    /// ChatGPT subscription access is an OpenAI Responses service at this
    /// boundary, even though it uses different authentication.
    pub const fn protocol(&self) -> Protocol {
        match self {
            Self::ChatGptSubscription | Self::OpenAiResponses { .. } => Protocol::OpenAiResponses,
            Self::OpenAiChatCompletions { .. } => Protocol::OpenAiChatCompletions,
            Self::AnthropicMessages { .. } => Protocol::AnthropicMessages,
        }
    }

    /// The optional compatible-service URL.
    ///
    /// ChatGPT subscription access does not permit an alternate base URL.
    pub fn base_url(&self) -> Option<&str> {
        match self {
            Self::ChatGptSubscription => None,
            Self::OpenAiResponses { base_url }
            | Self::OpenAiChatCompletions { base_url }
            | Self::AnthropicMessages { base_url } => base_url.as_deref(),
        }
    }

    /// Validate local, non-secret endpoint fields.
    ///
    /// Applications should call this after deserializing stored settings and
    /// before constructing a protocol client with separately resolved
    /// credentials.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if let Some(base_url) = self.base_url() {
            validate_base_url(base_url)?;
        }
        Ok(())
    }
}

/// Persistable protocol-specific controls for one selected model.
///
/// Omit this value when the model uses provider defaults. The explicit enum
/// prevents an OpenAI Responses control such as reasoning effort from being
/// represented as a provider-neutral field or silently applied to another
/// protocol.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelOptions {
    /// Controls supported by the OpenAI Responses wire protocol.
    #[serde(rename = "openai_responses")]
    OpenAiResponses {
        /// Typed Responses reasoning controls, including reasoning effort.
        reasoning: openai_responses::Reasoning,
    },
}

impl ModelOptions {
    /// The only wire protocol that can receive these controls.
    pub const fn protocol(&self) -> Protocol {
        match self {
            Self::OpenAiResponses { .. } => Protocol::OpenAiResponses,
        }
    }

    /// Validate controls that are meaningful without knowing an endpoint.
    pub fn validate(&self) -> Result<(), ModelOptionsError> {
        match self {
            Self::OpenAiResponses { reasoning } if reasoning.is_empty() => {
                Err(ModelOptionsError::EmptyOpenAiResponses)
            }
            Self::OpenAiResponses { .. } => Ok(()),
        }
    }

    /// Validate that these controls can be used by an endpoint configuration.
    ///
    /// This is intended for application settings validation. Runtime request
    /// validation independently checks the selected model's actual protocol,
    /// so an option can never be silently ignored if configuration changes
    /// after it was validated.
    pub fn validate_for(&self, endpoint: &EndpointConfig) -> Result<(), ModelOptionsError> {
        self.validate_for_protocol(endpoint.protocol())
    }

    pub(crate) fn validate_for_protocol(
        &self,
        endpoint_protocol: Protocol,
    ) -> Result<(), ModelOptionsError> {
        self.validate()?;
        let options_protocol = self.protocol();
        if options_protocol == endpoint_protocol {
            Ok(())
        } else {
            Err(ModelOptionsError::UnsupportedProtocol {
                options_protocol,
                endpoint_protocol,
            })
        }
    }

    pub(crate) fn into_additional_params(self) -> serde_json::Value {
        match self {
            Self::OpenAiResponses { reasoning } => serde_json::json!({ "reasoning": reasoning }),
        }
    }
}

/// A local validation failure in persistable model request controls.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelOptionsError {
    /// OpenAI Responses options did not specify any actual control.
    EmptyOpenAiResponses,
    /// These controls belong to a different wire protocol than the endpoint.
    UnsupportedProtocol {
        options_protocol: Protocol,
        endpoint_protocol: Protocol,
    },
}

impl fmt::Display for ModelOptionsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyOpenAiResponses => {
                formatter.write_str("OpenAI Responses model options are empty")
            }
            Self::UnsupportedProtocol {
                options_protocol,
                endpoint_protocol,
            } => write!(
                formatter,
                "{options_protocol} model options cannot be used with {endpoint_protocol}"
            ),
        }
    }
}

impl std::error::Error for ModelOptionsError {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::llm::protocol::openai_responses::{Reasoning, ReasoningEffort, ReasoningSummary};

    #[test]
    fn endpoint_config_round_trips_without_credentials() {
        let config = EndpointConfig::OpenAiResponses {
            base_url: Some("https://gateway.example/v1".into()),
        };

        let encoded = serde_json::to_value(&config).unwrap();
        assert_eq!(
            encoded,
            json!({
                "type": "openai_responses",
                "base_url": "https://gateway.example/v1"
            })
        );
        assert_eq!(
            serde_json::from_value::<EndpointConfig>(encoded).unwrap(),
            config
        );
        assert_eq!(config.protocol(), Protocol::OpenAiResponses);
        assert_eq!(config.base_url(), Some("https://gateway.example/v1"));
        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn endpoint_config_keeps_subscription_distinct_from_protocol() {
        let config = EndpointConfig::ChatGptSubscription;

        assert_eq!(config.protocol(), Protocol::OpenAiResponses);
        assert_eq!(config.base_url(), None);
        assert_eq!(
            serde_json::to_value(config).unwrap(),
            json!({"type": "chatgpt_subscription"})
        );
    }

    #[test]
    fn endpoint_config_rejects_invalid_compatible_urls() {
        let config = EndpointConfig::AnthropicMessages {
            base_url: Some("gateway.example/v1".into()),
        };

        assert_eq!(config.validate(), Err(ConfigError::InvalidBaseUrl));
    }

    #[test]
    fn model_options_are_protocol_scoped_and_serialize_typed_effort() {
        let options = ModelOptions::OpenAiResponses {
            reasoning: Reasoning::new()
                .effort(ReasoningEffort::High)
                .summary(ReasoningSummary::Concise),
        };

        assert_eq!(options.protocol(), Protocol::OpenAiResponses);
        assert_eq!(
            serde_json::to_value(&options).unwrap(),
            json!({
                "type": "openai_responses",
                "reasoning": {
                    "effort": "high",
                    "summary": "concise"
                }
            })
        );
        assert_eq!(
            serde_json::from_value::<ModelOptions>(serde_json::to_value(&options).unwrap())
                .unwrap(),
            options
        );
    }

    #[test]
    fn model_options_must_match_the_endpoint_protocol_and_contain_a_control() {
        let options = ModelOptions::OpenAiResponses {
            reasoning: Reasoning::new().effort(ReasoningEffort::Low),
        };
        assert_eq!(
            options.validate_for(&EndpointConfig::ChatGptSubscription),
            Ok(())
        );
        assert_eq!(
            options.validate_for(&EndpointConfig::AnthropicMessages { base_url: None }),
            Err(ModelOptionsError::UnsupportedProtocol {
                options_protocol: Protocol::OpenAiResponses,
                endpoint_protocol: Protocol::AnthropicMessages,
            })
        );

        let empty = ModelOptions::OpenAiResponses {
            reasoning: Reasoning::new(),
        };
        assert_eq!(
            empty.validate(),
            Err(ModelOptionsError::EmptyOpenAiResponses)
        );
    }
}
