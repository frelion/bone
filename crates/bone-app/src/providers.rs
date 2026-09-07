//! App composition of saved LLM profiles, private credentials, and endpoints.
//!
//! `bone-llm` owns protocol configuration and endpoint constructors.  This
//! module owns the one product-level `match` that supplies App-managed
//! credentials and returns an Agent host made from already-selected models.
//! It is deliberately not a provider registry or a credential trait framework.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use bone_agent::{AgentHost, AgentModels, ConfiguredModel};
use bone_llm::{
    Endpoint, EndpointConfig,
    protocol::{anthropic_messages, openai_chat_completions, openai_responses},
    service::chatgpt_subscription::{self, DeviceCodePrompt},
};
use thiserror::Error;
use tokio::sync::Mutex as AsyncMutex;

use crate::{
    ApiKeyCredentialError, ApiKeyCredentials, ChatGptCredentials, CredentialError, LlmProfile,
    LlmProfileId, ResolvedModel, ResolvedRuntime,
};

/// A cloneable process-local connector for configured LLM profiles.
///
/// Endpoints are intentionally rebuilt from the pinned profile for each
/// runtime, so a future start observes an updated base URL or API key rather
/// than a stale process cache. The connector never retains a ChatGPT OAuth
/// lease: only a live endpoint/model holds one, so an idle TUI does not block
/// another BONE process from authenticating.
#[derive(Clone)]
pub struct ProviderConnector {
    chatgpt_credentials: Arc<Mutex<Option<ChatGptCredentials>>>,
    chatgpt_connect: Arc<AsyncMutex<()>>,
}

impl Default for ProviderConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderConnector {
    /// Create a connector that lazily opens ChatGPT credentials only if a
    /// selected profile actually uses the subscription protocol.
    pub fn new() -> Self {
        Self::with_optional_chatgpt_credentials(None)
    }

    /// Construct a connector with an explicit ChatGPT credential manager.
    /// Embedders and tests can use this when they need a non-default private
    /// OAuth-cache root.
    pub fn with_chatgpt_credentials(chatgpt: ChatGptCredentials) -> Self {
        Self::with_optional_chatgpt_credentials(Some(chatgpt))
    }

    fn with_optional_chatgpt_credentials(chatgpt: Option<ChatGptCredentials>) -> Self {
        Self {
            chatgpt_credentials: Arc::new(Mutex::new(chatgpt)),
            chatgpt_connect: Arc::new(AsyncMutex::new(())),
        }
    }

    /// Clear the separate ChatGPT credential cache. Live endpoint/models own
    /// their leases directly, so the credential manager rejects logout while
    /// a runtime is actually using it.
    pub fn logout_chatgpt(&self) -> Result<(), CredentialError> {
        self.chatgpt_credentials()?.clear()
    }

    /// Perform the ChatGPT subscription authorization flow without creating an
    /// Agent runtime. This is the explicit `/login` path, so users can sign in
    /// before they choose or send a model request.
    pub async fn authenticate_chatgpt<F>(
        &self,
        profile: &LlmProfile,
        on_device_code: F,
    ) -> Result<(), ProviderConnectError>
    where
        F: Fn(DeviceCodePrompt) + Send + Sync + 'static,
    {
        if !matches!(&profile.endpoint, EndpointConfig::ChatGptSubscription) {
            return Err(ProviderConnectError::InvalidProfile(profile.id.clone()));
        }
        self.endpoint_for(profile, Arc::new(on_device_code))
            .await
            .map(|_| ())
    }

    /// Resolve the two role models from a pinned App runtime plan and build
    /// one Agent host.  The coordinator and solver may use different
    /// profiles, protocols, and credentials.
    pub async fn connect_agent<F>(
        &self,
        runtime: &ResolvedRuntime,
        on_device_code: F,
    ) -> Result<AgentHost, ProviderConnectError>
    where
        F: Fn(DeviceCodePrompt) + Send + Sync + 'static,
    {
        let on_device_code: Arc<dyn Fn(DeviceCodePrompt) + Send + Sync> = Arc::new(on_device_code);
        // A role pair often selects the same profile. Reuse its just-created
        // endpoint within this one immutable runtime plan, without making a
        // process-wide API-key client cache that could become stale.
        let mut endpoints = HashMap::new();
        let coordinator = self
            .configured_model(
                &runtime.coordinator,
                Arc::clone(&on_device_code),
                &mut endpoints,
            )
            .await?;
        let solver = self
            .configured_model(&runtime.solver, on_device_code, &mut endpoints)
            .await?;
        Ok(AgentHost::new(AgentModels::new(coordinator, solver)))
    }

    async fn configured_model(
        &self,
        resolved: &ResolvedModel,
        on_device_code: Arc<dyn Fn(DeviceCodePrompt) + Send + Sync>,
        endpoints: &mut HashMap<LlmProfileId, Endpoint>,
    ) -> Result<ConfiguredModel, ProviderConnectError> {
        let endpoint = match endpoints.get(&resolved.profile.id) {
            Some(endpoint) => endpoint.clone(),
            None => {
                let endpoint = self.endpoint_for(&resolved.profile, on_device_code).await?;
                endpoints.insert(resolved.profile.id.clone(), endpoint.clone());
                endpoint
            }
        };
        let model = endpoint
            .model(resolved.selection.model.clone())
            .map_err(|_| ProviderConnectError::InvalidModel {
                profile: resolved.profile.id.clone(),
                model: resolved.selection.model.clone(),
            })?;
        ConfiguredModel::new(model, resolved.selection.options.clone()).map_err(|_| {
            ProviderConnectError::InvalidModelOptions {
                profile: resolved.profile.id.clone(),
            }
        })
    }

    async fn endpoint_for(
        &self,
        profile: &LlmProfile,
        on_device_code: Arc<dyn Fn(DeviceCodePrompt) + Send + Sync>,
    ) -> Result<Endpoint, ProviderConnectError> {
        profile
            .validate()
            .map_err(|_| ProviderConnectError::InvalidProfile(profile.id.clone()))?;
        let endpoint = match &profile.endpoint {
            EndpointConfig::ChatGptSubscription => {
                // Rig may refresh the shared auth.json during authorization.
                // Serialize only that short connection phase; endpoint/model
                // work remains independent after a runtime is created.
                let _connecting = self.chatgpt_connect.lock().await;
                let auth = self
                    .chatgpt_credentials()
                    .map_err(|error| map_chatgpt_credential_error(profile.id.clone(), error))?
                    .acquire()
                    .map_err(|error| map_chatgpt_credential_error(profile.id.clone(), error))?;
                chatgpt_subscription::connect(profile.id.as_str(), auth, move |prompt| {
                    on_device_code(prompt);
                })
                .await
                .map_err(|_| ProviderConnectError::ChatGptAuthorization(profile.id.clone()))?
            }
            EndpointConfig::OpenAiResponses { base_url } => {
                let api_key = self.api_key(profile)?;
                match base_url {
                    Some(base_url) => openai_responses::compatible(
                        profile.id.as_str(),
                        api_key.as_str().to_owned(),
                        base_url.clone(),
                    ),
                    None => {
                        openai_responses::official(profile.id.as_str(), api_key.as_str().to_owned())
                    }
                }
                .map_err(|_| ProviderConnectError::InvalidProfile(profile.id.clone()))?
            }
            EndpointConfig::OpenAiChatCompletions { base_url } => {
                let api_key = self.api_key(profile)?;
                match base_url {
                    Some(base_url) => openai_chat_completions::compatible(
                        profile.id.as_str(),
                        api_key.as_str().to_owned(),
                        base_url.clone(),
                    ),
                    None => openai_chat_completions::official(
                        profile.id.as_str(),
                        api_key.as_str().to_owned(),
                    ),
                }
                .map_err(|_| ProviderConnectError::InvalidProfile(profile.id.clone()))?
            }
            EndpointConfig::AnthropicMessages { base_url } => {
                let api_key = self.api_key(profile)?;
                match base_url {
                    Some(base_url) => anthropic_messages::compatible(
                        profile.id.as_str(),
                        api_key.as_str().to_owned(),
                        base_url.clone(),
                    ),
                    None => anthropic_messages::official(
                        profile.id.as_str(),
                        api_key.as_str().to_owned(),
                    ),
                }
                .map_err(|_| ProviderConnectError::InvalidProfile(profile.id.clone()))?
            }
        };
        Ok(endpoint)
    }

    fn chatgpt_credentials(&self) -> Result<ChatGptCredentials, CredentialError> {
        let mut credentials = self
            .chatgpt_credentials
            .lock()
            .map_err(|_| CredentialError::Unavailable)?;
        if credentials.is_none() {
            *credentials = Some(ChatGptCredentials::default_for_current_user()?);
        }
        Ok(credentials
            .as_ref()
            .expect("ChatGPT credentials were initialized above")
            .clone())
    }

    fn api_key(&self, profile: &LlmProfile) -> Result<crate::ApiKey, ProviderConnectError> {
        let credentials = ApiKeyCredentials::for_profile(profile)
            .map_err(|_| ProviderConnectError::CredentialUnavailable(profile.id.clone()))?;
        credentials
            .read()
            .map_err(|error| map_api_key_error(profile.id.clone(), error))
    }
}

/// Redacted failures while connecting one saved profile.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ProviderConnectError {
    #[error("profile `{0}` has invalid saved endpoint settings")]
    InvalidProfile(LlmProfileId),
    #[error("model `{model}` is invalid for profile `{profile}`")]
    InvalidModel {
        profile: LlmProfileId,
        model: String,
    },
    #[error("profile `{profile}` has model options that its protocol cannot use")]
    InvalidModelOptions { profile: LlmProfileId },
    #[error("API key is missing for profile `{0}`; run `bone credentials set {0}`")]
    MissingApiKey(LlmProfileId),
    #[error("system credential storage is unavailable for profile `{0}`")]
    CredentialUnavailable(LlmProfileId),
    #[error("ChatGPT authorization could not complete for profile `{0}`")]
    ChatGptAuthorization(LlmProfileId),
    #[error("ChatGPT credentials are busy for profile `{0}`")]
    ChatGptBusy(LlmProfileId),
}

fn map_api_key_error(profile: LlmProfileId, error: ApiKeyCredentialError) -> ProviderConnectError {
    match error {
        ApiKeyCredentialError::MissingApiKey => ProviderConnectError::MissingApiKey(profile),
        ApiKeyCredentialError::InvalidApiKey | ApiKeyCredentialError::Unavailable => {
            ProviderConnectError::CredentialUnavailable(profile)
        }
    }
}

fn map_chatgpt_credential_error(
    profile: LlmProfileId,
    error: CredentialError,
) -> ProviderConnectError {
    match error {
        CredentialError::Busy => ProviderConnectError::ChatGptBusy(profile),
        CredentialError::Unavailable => ProviderConnectError::ChatGptAuthorization(profile),
    }
}

#[cfg(test)]
mod tests {
    use bone_llm::{EndpointConfig, Protocol};

    use super::*;

    fn profile(endpoint: EndpointConfig) -> LlmProfile {
        LlmProfile::new(LlmProfileId::new("test").unwrap(), "Test", endpoint).unwrap()
    }

    #[test]
    fn api_protocol_profiles_construct_the_expected_endpoint_without_network_io() {
        let key = crate::ApiKey::new("test-key".into()).unwrap();
        let cases = [
            (
                EndpointConfig::OpenAiResponses { base_url: None },
                Protocol::OpenAiResponses,
            ),
            (
                EndpointConfig::OpenAiChatCompletions { base_url: None },
                Protocol::OpenAiChatCompletions,
            ),
            (
                EndpointConfig::AnthropicMessages { base_url: None },
                Protocol::AnthropicMessages,
            ),
        ];
        for (endpoint, protocol) in cases {
            let profile = profile(endpoint);
            let endpoint = match &profile.endpoint {
                EndpointConfig::OpenAiResponses { base_url: None } => {
                    openai_responses::official(profile.id.as_str(), key.as_str().to_owned())
                }
                EndpointConfig::OpenAiChatCompletions { base_url: None } => {
                    openai_chat_completions::official(profile.id.as_str(), key.as_str().to_owned())
                }
                EndpointConfig::AnthropicMessages { base_url: None } => {
                    anthropic_messages::official(profile.id.as_str(), key.as_str().to_owned())
                }
                _ => unreachable!(),
            }
            .unwrap();
            assert_eq!(endpoint.protocol(), protocol);
            assert_eq!(endpoint.id(), "test");
        }
    }
}
