//! Constructors and protocol-specific helpers for supported LLM wire APIs.

use crate::ConfigError;

pub mod anthropic_messages;
pub mod openai_chat_completions;
pub mod openai_responses;

/// The wire contract used for requests, responses, and streaming events.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Protocol {
    /// OpenAI's `/responses` API, distinct from Chat Completions.
    OpenAiResponses,
    /// OpenAI's `/chat/completions` API, distinct from Responses.
    OpenAiChatCompletions,
    /// Anthropic's `/v1/messages` API.
    AnthropicMessages,
}

impl Protocol {
    /// A stable, human-readable protocol identifier for logs and telemetry.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiResponses => "openai-responses",
            Self::OpenAiChatCompletions => "openai-chat-completions",
            Self::AnthropicMessages => "anthropic-messages",
        }
    }
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

pub(crate) fn validate_base_url(base_url: &str) -> Result<(), ConfigError> {
    if base_url.trim().is_empty() {
        return Err(ConfigError::EmptyBaseUrl);
    }

    let uri = base_url
        .parse::<rig_core::http_client::Uri>()
        .map_err(|_| ConfigError::InvalidBaseUrl)?;
    let is_http = matches!(uri.scheme_str(), Some("http" | "https"));
    let has_safe_authority = uri
        .authority()
        .is_some_and(|authority| !authority.as_str().contains('@'));
    if !is_http || !has_safe_authority || uri.query().is_some() {
        return Err(ConfigError::InvalidBaseUrl);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_stable_protocol_names() {
        assert_eq!(Protocol::OpenAiResponses.as_str(), "openai-responses");
        assert_eq!(
            Protocol::OpenAiChatCompletions.as_str(),
            "openai-chat-completions"
        );
        assert_eq!(Protocol::AnthropicMessages.as_str(), "anthropic-messages");
    }

    #[test]
    fn accepts_only_absolute_http_base_urls_without_credentials_or_queries() {
        assert_eq!(validate_base_url("  "), Err(ConfigError::EmptyBaseUrl));
        assert_eq!(
            validate_base_url("gateway.example/v1"),
            Err(ConfigError::InvalidBaseUrl)
        );
        assert_eq!(
            validate_base_url("ftp://gateway.example/v1"),
            Err(ConfigError::InvalidBaseUrl)
        );
        assert_eq!(
            validate_base_url("https://gateway.example/v1?tenant=one"),
            Err(ConfigError::InvalidBaseUrl)
        );
        let credentialed_url = "https://user:secret@gateway.example/v1";
        let error = validate_base_url(credentialed_url).unwrap_err();
        assert_eq!(error, ConfigError::InvalidBaseUrl);
        assert!(!error.to_string().contains("secret"));
        assert_eq!(validate_base_url("https://gateway.example/v1"), Ok(()));
    }
}
