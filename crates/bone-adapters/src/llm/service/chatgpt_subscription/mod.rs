//! Experimental ChatGPT subscription access through the Codex Responses backend.
//!
//! This adapter uses Rig's in-process ChatGPT OAuth implementation. It does
//! not start a proxy or a Codex agent, and it is not the public OpenAI Platform
//! API. The explicit [`connect`] call may ask the user to complete a
//! device-code login; later requests reuse and refresh BONE's independently
//! managed ChatGPT token cache at a caller-provided private path.
//!
//! Never point Rig's `auth_file` option at `~/.codex/auth.json`. Codex and Rig
//! use different file schemas and independent refresh-token lifecycles.

use rig_core::{client::CompletionClient, providers::chatgpt as rig_chatgpt};
#[cfg(feature = "test-utils")]
use rig_core::{
    http_client::HttpClientExt,
    wasm_compat::{WasmCompatSend, WasmCompatSync},
};
use std::{
    fmt::{self, Debug},
    path::Path,
};

use crate::llm::{
    ConfigError, Endpoint, Protocol, error::validate_endpoint_id, protocol::no_redirect_http_client,
};

/// A redacted ChatGPT subscription service failure.
///
/// OAuth response bodies, tokens, and credential contents are deliberately not
/// exposed through this error boundary.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The endpoint identity or another protocol-neutral setting is invalid.
    Configuration(ConfigError),
    /// Rig rejected the local ChatGPT client configuration.
    InvalidClientConfiguration,
    /// Interactive login, cached-token loading, or token refresh failed.
    AuthorizationFailed,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Configuration(error) => return fmt::Display::fmt(error, formatter),
            Self::InvalidClientConfiguration => "ChatGPT client configuration is invalid",
            Self::AuthorizationFailed => {
                "ChatGPT authorization failed; reconnect the subscription and try again"
            }
        })
    }
}

impl std::error::Error for Error {}

impl From<ConfigError> for Error {
    fn from(error: ConfigError) -> Self {
        Self::Configuration(error)
    }
}

/// Device-code details for the application's explicit ChatGPT connection UI.
///
/// Treat the short code as ephemeral authentication material: display it only
/// in the active connection UI and do not log or persist it.
#[derive(Clone, PartialEq, Eq)]
pub struct DeviceCodePrompt {
    pub verification_uri: String,
    pub user_code: String,
}

impl Debug for DeviceCodePrompt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceCodePrompt")
            .field("verification_uri", &self.verification_uri)
            .field("user_code", &"<redacted>")
            .finish()
    }
}

/// Explicitly authorize a private OAuth cache, allowing device-code login.
pub async fn connect<F>(
    endpoint_id: impl Into<String>,
    auth_file: &Path,
    on_device_code: F,
) -> Result<Endpoint, Error>
where
    F: Fn(DeviceCodePrompt) + Send + Sync + 'static,
{
    let endpoint_id = endpoint_id.into();
    validate_endpoint_id(&endpoint_id)?;
    let interactive_client = rig_chatgpt::Client::builder()
        .oauth()
        .auth_file(auth_file)
        .allow_device_flow(true)
        .on_device_code(move |prompt| {
            on_device_code(DeviceCodePrompt {
                verification_uri: prompt.verification_uri,
                user_code: prompt.user_code,
            });
        })
        .default_instructions("")
        .originator("bone")
        .user_agent(user_agent())
        .http_client(no_redirect_http_client().map_err(|_| Error::InvalidClientConfiguration)?)
        .build()
        .map_err(|_| Error::InvalidClientConfiguration)?;

    interactive_client
        .authorize()
        .await
        .map_err(|_| Error::AuthorizationFailed)?;

    connect_cached(endpoint_id, auth_file)
}

/// Construct an endpoint without authorizing or accessing the network.
/// Each request loads current credentials and refreshes them when necessary.
pub fn connect_cached(endpoint_id: impl Into<String>, auth_file: &Path) -> Result<Endpoint, Error> {
    let endpoint_id = endpoint_id.into();
    validate_endpoint_id(&endpoint_id)?;
    // Runtime requests must fail instead of unexpectedly starting an
    // interactive device-code flow.
    let client = rig_chatgpt::Client::builder()
        .oauth()
        .auth_file(auth_file)
        .allow_device_flow(false)
        .default_instructions("")
        .originator("bone")
        .user_agent(user_agent())
        .http_client(no_redirect_http_client().map_err(|_| Error::InvalidClientConfiguration)?)
        .build()
        .map_err(|_| Error::InvalidClientConfiguration)?;

    Endpoint::from_model_factory(endpoint_id, Protocol::OpenAiResponses, move |model_id| {
        client.completion_model(model_id).with_strict_tools()
    })
    .map_err(Into::into)
}

/// Internal unmanaged-client seam used by offline contract tests.
#[cfg(feature = "test-utils")]
pub(crate) fn from_unmanaged_client<H>(
    endpoint_id: impl Into<String>,
    client: rig_chatgpt::Client<H>,
) -> Result<Endpoint, ConfigError>
where
    H: HttpClientExt
        + Clone
        + Default
        + Debug
        + WasmCompatSend
        + WasmCompatSync
        + Send
        + Sync
        + 'static,
{
    Endpoint::from_model_factory(endpoint_id, Protocol::OpenAiResponses, move |model_id| {
        client.completion_model(model_id).with_strict_tools()
    })
}

/// Remove cached authorization using the refresh transaction lock.
pub async fn clear_cache(auth_file: &Path) -> Result<(), Error> {
    rig_chatgpt::clear_cache(auth_file)
        .await
        .map_err(|_| Error::AuthorizationFailed)
}

fn user_agent() -> String {
    format!(
        "bone-adapters/{} ({} {})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

#[cfg(test)]
mod tests;
