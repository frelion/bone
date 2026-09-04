//! Experimental ChatGPT subscription access through the Codex Responses backend.
//!
//! This adapter uses Rig's in-process ChatGPT OAuth implementation. It does
//! not start a proxy or a Codex agent, and it is not the public OpenAI Platform
//! API. The explicit [`connect`] call may ask the user to complete a
//! device-code login; later requests reuse and refresh BONE's independent
//! ChatGPT token cache beneath the credential root supplied by the caller.
//!
//! Never point Rig's `auth_file` option at `~/.codex/auth.json`. Codex and Rig
//! use different file schemas and independent refresh-token lifecycles.

mod credential_store;

use std::{
    fmt::{self, Debug},
    path::{Path, PathBuf},
    sync::Arc,
};

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

use self::credential_store::CredentialLease;
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
    /// Managed subscription credentials are unsupported on this target.
    UnsupportedPlatform,
    /// The managed credential store is unavailable or unsafe.
    CredentialStoreUnavailable,
    /// Another BONE process or live endpoint owns the credential store.
    CredentialStoreBusy,
    /// Interactive login, cached-token loading, or token refresh failed.
    AuthorizationFailed,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Configuration(error) => return fmt::Display::fmt(error, formatter),
            Self::InvalidClientConfiguration => "ChatGPT client configuration is invalid",
            Self::UnsupportedPlatform => {
                "managed ChatGPT subscription credentials are unsupported on this platform"
            }
            Self::CredentialStoreUnavailable => "ChatGPT credential store is unavailable or unsafe",
            Self::CredentialStoreBusy => "ChatGPT credential store is in use by another client",
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

/// Explicitly connect an in-process ChatGPT subscription endpoint.
///
/// No API key, sidecar, or local HTTP proxy is required. `credential_root`
/// must be an absolute, private, application-owned BONE directory. An existing
/// root must already be owner-only; the connector never changes its permissions.
/// Rig stores its OAuth record at `chatgpt-subscription/auth.json` beneath that
/// root. This call authorizes before returning, so a later model request never
/// surprises the caller by starting a device-code flow. Managed credentials are
/// unsupported on other targets.
pub async fn connect<F>(
    endpoint_id: impl Into<String>,
    credential_root: impl AsRef<Path>,
    on_device_code: F,
) -> Result<Endpoint, Error>
where
    F: Fn(DeviceCodePrompt) + Send + Sync + 'static,
{
    let endpoint_id = endpoint_id.into();
    validate_endpoint_id(&endpoint_id)?;
    let (auth_file, lease) = credential_store::prepare_in(credential_root.as_ref())?;
    let lease = Arc::new(lease);
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
            _lease: Arc::clone(&lease),
        },
    )
    .map_err(Into::into)
}

/// Return the conventional BONE credential root for the current user.
///
/// This helper only resolves a path (`$XDG_CONFIG_HOME/bone`, falling back to
/// `$HOME/.config/bone`) and does not create files or directories. Products
/// that want conventional local storage can call it explicitly and inject the
/// result into [`connect`] or [`disconnect`].
pub fn default_credential_root() -> Result<PathBuf, Error> {
    credential_store::default_credential_root()
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

/// Delete BONE's independent local ChatGPT credential record.
///
/// This disconnects future BONE clients but does not revoke the upstream
/// ChatGPT session. Existing endpoint and model handles must be dropped first.
/// Calling this before the first connection is a successful no-op on Unix.
/// `credential_root` must be the same application-owned root supplied to
/// [`connect`]. Managed subscription credentials are unsupported on other
/// targets.
pub fn disconnect(credential_root: impl AsRef<Path>) -> Result<(), Error> {
    credential_store::disconnect_in(credential_root.as_ref())
}

#[derive(Clone)]
struct LeasedModel<M> {
    inner: M,
    _lease: Arc<CredentialLease>,
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
