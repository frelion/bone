use std::{error::Error as StdError, fmt};

/// Failures while constructing a configured endpoint or selecting a model.
///
/// This type covers local endpoint construction only. Model calls use
/// [`Error`], and neither error type includes credential values by design.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// The endpoint identity is empty.
    EmptyEndpointId,
    /// The configured API key is empty.
    EmptyApiKey,
    /// The API key cannot be encoded as an HTTP authorization header.
    InvalidApiKey,
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
            Self::EmptyBaseUrl => "endpoint base URL is empty",
            Self::InvalidBaseUrl => {
                "endpoint base URL must be an absolute HTTP(S) URL without embedded credentials or a query string"
            }
            Self::EmptyModelId => "model identifier is empty",
        })
    }
}

impl std::error::Error for ConfigError {}

/// Broad category of a model-call failure.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// The request violates BONE's model contract.
    InvalidRequest,
    /// The selected endpoint cannot honor an explicitly requested option.
    UnsupportedOption,
    /// The provider transport failed.
    Transport,
    /// A request or response could not be encoded or decoded.
    Protocol,
    /// The remote model service rejected or failed the request.
    Provider,
    /// A stream ended without one trustworthy terminal response.
    IncompleteStream,
}

/// A failure at BONE's model boundary.
///
/// The concrete provider implementation is intentionally absent from this
/// type. [`Error::kind`] is stable control-flow information; the display text
/// remains useful for diagnostics.
pub struct Error {
    kind: ErrorKind,
    message: String,
    source: Option<ErrorSource>,
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
type ErrorSource = Box<dyn StdError + Send + Sync + 'static>;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
type ErrorSource = Box<dyn StdError + 'static>;

impl Error {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidRequest, message)
    }

    pub(crate) fn unsupported(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::UnsupportedOption, message)
    }

    pub(crate) fn protocol(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Protocol, message)
    }

    pub(crate) fn incomplete_stream() -> Self {
        Self::new(
            ErrorKind::IncompleteStream,
            "model stream ended without a complete response",
        )
    }

    pub(crate) fn from_rig(error: rig_core::completion::CompletionError) -> Self {
        use rig_core::completion::CompletionError as RigError;

        let kind = match &error {
            RigError::HttpError(_) | RigError::UrlError(_) => ErrorKind::Transport,
            RigError::RequestError(_) => ErrorKind::InvalidRequest,
            RigError::JsonError(_) | RigError::ResponseError(_) => ErrorKind::Protocol,
            RigError::ProviderError(_) | RigError::ProviderResponse(_) => ErrorKind::Provider,
        };
        let message = match &error {
            RigError::HttpError(error) => error.to_string(),
            RigError::JsonError(error) => format!("invalid model protocol payload: {error}"),
            RigError::UrlError(error) => format!("invalid model endpoint URL: {error}"),
            RigError::RequestError(error) => format!("invalid model request: {error}"),
            RigError::ResponseError(error) => format!("invalid model response: {error}"),
            RigError::ProviderError(error) => error.clone(),
            RigError::ProviderResponse(error) => error.to_string(),
        };
        Self {
            kind,
            message,
            source: Some(Box::new(error)),
        }
    }

    pub(crate) fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            source: None,
        }
    }

    /// Stable failure category for control flow.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Error")
            .field("kind", &self.kind)
            .field("message", &self.message)
            .finish_non_exhaustive()
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn StdError + 'static))
    }
}

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
