use std::fmt;

/// Failures while constructing a configured endpoint or selecting a model.
///
/// Request and response failures continue to use Rig's
/// [`CompletionError`](rig_core::completion::CompletionError). This type only
/// covers local configuration and deliberately never includes credential
/// values in its messages.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// The endpoint identity is empty.
    EmptyEndpointId,
    /// The configured API key is empty.
    EmptyApiKey,
    /// The API key cannot be encoded as an HTTP authorization header.
    InvalidApiKey,
    /// A provider client could not be constructed from local configuration.
    InvalidClientConfiguration,
    /// The provider's application-owned credential store is unavailable or unsafe.
    CredentialStoreUnavailable,
    /// The configured base URL is empty.
    EmptyBaseUrl,
    /// The base URL is not an absolute HTTP(S) URL without embedded credentials or a query string.
    InvalidBaseUrl,
    /// The selected model identity is empty.
    EmptyModelId,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EmptyEndpointId => "endpoint identifier is empty",
            Self::EmptyApiKey => "API key is empty",
            Self::InvalidApiKey => "API key is not a valid HTTP header value",
            Self::InvalidClientConfiguration => "provider client configuration is invalid",
            Self::CredentialStoreUnavailable => {
                "provider credential store is unavailable or unsafe"
            }
            Self::EmptyBaseUrl => "endpoint base URL is empty",
            Self::InvalidBaseUrl => {
                "endpoint base URL must be an absolute HTTP(S) URL without embedded credentials or a query string"
            }
            Self::EmptyModelId => "model identifier is empty",
        })
    }
}

impl std::error::Error for ConfigError {}

pub(crate) fn validate_endpoint_id(id: &str) -> Result<(), ConfigError> {
    if id.trim().is_empty() {
        Err(ConfigError::EmptyEndpointId)
    } else {
        Ok(())
    }
}

pub(crate) fn validate_model_id(id: &str) -> Result<(), ConfigError> {
    if id.trim().is_empty() {
        Err(ConfigError::EmptyModelId)
    } else {
        Ok(())
    }
}
