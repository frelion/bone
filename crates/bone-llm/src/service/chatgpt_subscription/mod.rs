//! Experimental ChatGPT subscription access through the Codex Responses backend.
//!
//! This adapter uses Rig's in-process ChatGPT OAuth implementation. It does
//! not start a proxy or a Codex agent, and it is not the public OpenAI Platform
//! API. The explicit [`connect`] call may ask the user to complete a
//! device-code login; later requests reuse and refresh BONE's independently
//! managed ChatGPT token cache through a caller-provided cache lease.
//!
//! Never point Rig's `auth_file` option at `~/.codex/auth.json`. Codex and Rig
//! use different file schemas and independent refresh-token lifecycles.

use rig_core::{
    client::CompletionClient,
    completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse},
    providers::chatgpt as rig_chatgpt,
    streaming::StreamingCompletionResponse,
};
#[cfg(feature = "test-utils")]
use rig_core::{
    http_client::HttpClientExt,
    wasm_compat::{WasmCompatSend, WasmCompatSync},
};
use std::{
    fmt::{self, Debug},
    path::Path,
    sync::Arc,
};

use crate::{ConfigError, Endpoint, Protocol, error::validate_endpoint_id, model::RequestSupport};

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

/// A live, application-owned lease for Rig's ChatGPT OAuth cache file.
///
/// The application owns cache placement, access control, and exclusive
/// lifetime management. This adapter only needs the already-validated path
/// and retains the capability in every endpoint and model it creates.
pub trait ChatGptAuthCache: Send + Sync + 'static {
    /// The private cache file that Rig may read and update.
    fn auth_file(&self) -> &Path;
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

/// Explicitly connect an in-process ChatGPT subscription endpoint.
///
/// No API key, sidecar, or local HTTP proxy is required. The caller acquires
/// `auth` from the product application; it owns a validated OAuth cache path
/// and its exclusive lease. Rig remains the sole owner of the cache schema
/// and refresh lifecycle.
///
/// This call authorizes before returning, so a later model request never
/// surprises the caller by starting a device-code flow. The returned endpoint
/// and every model selected from it retain the lease until they are dropped.
pub async fn connect<F, A>(
    endpoint_id: impl Into<String>,
    auth: A,
    on_device_code: F,
) -> Result<Endpoint, Error>
where
    F: Fn(DeviceCodePrompt) + Send + Sync + 'static,
    A: ChatGptAuthCache,
{
    let endpoint_id = endpoint_id.into();
    validate_endpoint_id(&endpoint_id)?;
    let auth: Arc<dyn ChatGptAuthCache> = Arc::new(auth);
    let auth_file = auth.auth_file().to_path_buf();
    let interactive_client = rig_chatgpt::Client::builder()
        .oauth()
        .auth_file(&auth_file)
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
        .build()
        .map_err(|_| Error::InvalidClientConfiguration)?;

    interactive_client
        .authorize()
        .await
        .map_err(|_| Error::AuthorizationFailed)?;

    // Runtime requests must fail instead of unexpectedly starting an
    // interactive device-code flow.
    let client = rig_chatgpt::Client::builder()
        .oauth()
        .auth_file(auth_file)
        .allow_device_flow(false)
        .default_instructions("")
        .originator("bone")
        .user_agent(user_agent())
        .build()
        .map_err(|_| Error::InvalidClientConfiguration)?;

    Endpoint::from_model_factory_with_support(
        endpoint_id,
        Protocol::OpenAiResponses,
        RequestSupport::CHATGPT_SUBSCRIPTION,
        move |model_id| LeasedModel {
            inner: client.completion_model(model_id),
            _auth: auth.clone(),
        },
    )
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
    Endpoint::from_model_factory_with_support(
        endpoint_id,
        Protocol::OpenAiResponses,
        RequestSupport::CHATGPT_SUBSCRIPTION,
        move |model_id| client.completion_model(model_id),
    )
}

#[derive(Clone)]
struct LeasedModel<M> {
    inner: M,
    _auth: Arc<dyn ChatGptAuthCache>,
}

impl<M> CompletionModel for LeasedModel<M>
where
    M: CompletionModel + Send + Sync,
{
    async fn completion(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, CompletionError> {
        self.inner.completion(request).await
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse, CompletionError> {
        self.inner.stream(request).await
    }

    fn capabilities(&self) -> rig_core::completion::ProviderCapabilities {
        self.inner.capabilities()
    }
}

fn user_agent() -> String {
    format!(
        "bone-llm/{} ({} {})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

#[cfg(test)]
mod tests;
