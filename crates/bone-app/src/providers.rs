//! Concrete composition of saved profiles, private credentials, and models.

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use bone_adapters::{
    ConfiguredModel, ModelAdapter,
    llm::{
        Endpoint, EndpointConfig,
        protocol::{anthropic_messages, openai_chat_completions, openai_responses},
        service::chatgpt_subscription::{self, DeviceCodePrompt},
    },
};
use bone_core::ModelPort;

use crate::{
    config::{Profile, ProfileId, ResolvedModel, RuntimeConfig},
    credentials::{
        ApiKey, ApiKeyCredentialError, ApiKeyCredentials, ChatGptCredentials, CredentialError,
    },
};

/// Constructs model ports; credentials are loaded by each OAuth request.
#[derive(Clone, Default)]
pub(crate) struct ProviderConnector {
    bone_home: PathBuf,
    credentials: Option<ChatGptCredentials>,
    #[cfg(test)]
    test_endpoints: HashMap<ProfileId, Endpoint>,
}

impl ProviderConnector {
    pub(crate) fn new(bone_home: PathBuf) -> Self {
        Self {
            credentials: ChatGptCredentials::at(bone_home.clone()).ok(),
            bone_home,
            #[cfg(test)]
            test_endpoints: HashMap::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_endpoints(test_endpoints: HashMap<ProfileId, Endpoint>) -> Self {
        Self {
            test_endpoints,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn with_chatgpt_credentials(credentials: ChatGptCredentials) -> Self {
        Self {
            credentials: Some(credentials),
            ..Self::default()
        }
    }

    fn credentials(&self) -> Result<&ChatGptCredentials, ProviderConnectError> {
        self.credentials
            .as_ref()
            .ok_or_else(|| chatgpt_error(CredentialError::Unavailable))
    }

    /// Build the existing Agent model adapter without starting a runtime or
    /// initiating interactive authentication.
    pub(crate) async fn connect(
        &self,
        runtime: &RuntimeConfig,
    ) -> Result<Arc<dyn ModelPort>, ProviderConnectError> {
        for model in [&runtime.coordinator, &runtime.worker] {
            validate_profile(&model.profile)?;
            if model.selection.profile != model.profile.id {
                return Err(ProviderConnectError::InvalidProfile(
                    model.profile.id.clone(),
                ));
            }
            model
                .selection
                .validate()
                .map_err(|_| ProviderConnectError::InvalidModel(model.profile.id.clone()))?;
        }
        if runtime.coordinator.profile.id == runtime.worker.profile.id
            && runtime.coordinator.profile.endpoint != runtime.worker.profile.endpoint
        {
            return Err(ProviderConnectError::InvalidProfile(
                runtime.worker.profile.id.clone(),
            ));
        }
        let chatgpt = if [&runtime.coordinator, &runtime.worker]
            .iter()
            .any(|model| matches!(model.profile.endpoint, EndpointConfig::ChatGptSubscription))
        {
            Some(self.connect_chatgpt().await?)
        } else {
            None
        };
        let mut endpoints = HashMap::new();
        #[cfg(test)]
        endpoints.extend(self.test_endpoints.clone());
        let coordinator = self
            .configured_model(&runtime.coordinator, chatgpt.as_ref(), &mut endpoints)
            .await?;
        let worker = self
            .configured_model(&runtime.worker, chatgpt.as_ref(), &mut endpoints)
            .await?;
        Ok(Arc::new(ModelAdapter::new(coordinator, worker)))
    }

    /// The caller owns cancellation by dropping this future and exposes the
    /// callback only through its short-lived login state.
    pub(crate) async fn login<F>(
        &self,
        profile: &Profile,
        on_device_code: F,
    ) -> Result<(), ProviderConnectError>
    where
        F: Fn(DeviceCodePrompt) + Send + Sync + 'static,
    {
        validate_profile(profile)?;
        if !matches!(profile.endpoint, EndpointConfig::ChatGptSubscription) {
            return Err(ProviderConnectError::InvalidProfile(profile.id.clone()));
        }
        let auth = self.credentials()?.auth_file().map_err(chatgpt_error)?;
        chatgpt_subscription::connect(profile.id.as_str(), &auth, on_device_code)
            .await
            .map_err(|_| ProviderConnectError::AuthorizationFailed(profile.id.clone()))?;
        Ok(())
    }

    pub(crate) async fn logout(&self, profile: &Profile) -> Result<(), ProviderConnectError> {
        validate_profile(profile)?;
        if matches!(profile.endpoint, EndpointConfig::ChatGptSubscription) {
            return self.credentials()?.clear().await.map_err(chatgpt_error);
        }
        let profile = profile.clone();
        let bone_home = self.bone_home.clone();
        credential_task(profile.id.clone(), move || {
            ApiKeyCredentials::for_profile(&bone_home, &profile)
                .and_then(|credentials| credentials.clear())
                .map_err(|error| api_key_error(profile.id, error))
        })
        .await
    }

    pub(crate) async fn set_api_key(
        &self,
        profile: &Profile,
        key: ApiKey,
    ) -> Result<(), ProviderConnectError> {
        validate_profile(profile)?;
        if matches!(profile.endpoint, EndpointConfig::ChatGptSubscription) {
            return Err(ProviderConnectError::InvalidProfile(profile.id.clone()));
        }
        let profile = profile.clone();
        let bone_home = self.bone_home.clone();
        credential_task(profile.id.clone(), move || {
            ApiKeyCredentials::for_profile(&bone_home, &profile)
                .and_then(|credentials| credentials.save(&key))
                .map_err(|error| api_key_error(profile.id, error))
        })
        .await
    }

    async fn connect_chatgpt(&self) -> Result<Endpoint, ProviderConnectError> {
        let profile = ProfileId::chatgpt();
        let auth = self.credentials()?.auth_file().map_err(chatgpt_error)?;
        if !auth.exists() {
            return Err(ProviderConnectError::LoginRequired(profile));
        }
        chatgpt_subscription::connect_cached(profile.as_str(), &auth)
            .map_err(|_| ProviderConnectError::InvalidProfile(profile))
    }

    async fn configured_model(
        &self,
        resolved: &ResolvedModel,
        chatgpt: Option<&Endpoint>,
        endpoints: &mut HashMap<ProfileId, Endpoint>,
    ) -> Result<ConfiguredModel, ProviderConnectError> {
        let profile = &resolved.profile;
        let endpoint = match endpoints.get(&profile.id) {
            Some(endpoint) => endpoint.clone(),
            None => {
                let endpoint = if matches!(profile.endpoint, EndpointConfig::ChatGptSubscription) {
                    chatgpt
                        .expect("ChatGPT connection resolved before model selection")
                        .clone()
                } else {
                    let owned_profile = profile.clone();
                    let bone_home = self.bone_home.clone();
                    let key = credential_task(profile.id.clone(), move || {
                        ApiKeyCredentials::for_profile(&bone_home, &owned_profile)
                            .and_then(|credentials| credentials.read())
                            .map_err(|error| api_key_error(owned_profile.id, error))
                    })
                    .await?;
                    api_endpoint(profile, &key)?
                };
                endpoints.insert(profile.id.clone(), endpoint.clone());
                endpoint
            }
        };
        let model = endpoint
            .model(resolved.selection.model.clone())
            .map_err(|_| ProviderConnectError::InvalidModel(profile.id.clone()))?;
        ConfiguredModel::new(model, resolved.selection.options.clone())
            .map_err(|_| ProviderConnectError::InvalidModelOptions(profile.id.clone()))
    }
}

fn api_endpoint(profile: &Profile, key: &ApiKey) -> Result<Endpoint, ProviderConnectError> {
    let endpoint = match &profile.endpoint {
        EndpointConfig::OpenAiResponses { base_url } => match base_url {
            Some(url) => openai_responses::compatible(
                profile.id.as_str(),
                key.as_str().to_owned(),
                url.clone(),
            ),
            None => openai_responses::official(profile.id.as_str(), key.as_str().to_owned()),
        },
        EndpointConfig::OpenAiChatCompletions { base_url } => match base_url {
            Some(url) => openai_chat_completions::compatible(
                profile.id.as_str(),
                key.as_str().to_owned(),
                url.clone(),
            ),
            None => openai_chat_completions::official(profile.id.as_str(), key.as_str().to_owned()),
        },
        EndpointConfig::AnthropicMessages { base_url } => match base_url {
            Some(url) => anthropic_messages::compatible(
                profile.id.as_str(),
                key.as_str().to_owned(),
                url.clone(),
            ),
            None => anthropic_messages::official(profile.id.as_str(), key.as_str().to_owned()),
        },
        EndpointConfig::ChatGptSubscription => {
            return Err(ProviderConnectError::InvalidProfile(profile.id.clone()));
        }
    };
    endpoint.map_err(|_| ProviderConnectError::InvalidProfile(profile.id.clone()))
}

fn validate_profile(profile: &Profile) -> Result<(), ProviderConnectError> {
    profile
        .validate()
        .map_err(|_| ProviderConnectError::InvalidProfile(profile.id.clone()))
}

async fn credential_task<T: Send + 'static>(
    profile: ProfileId,
    operation: impl FnOnce() -> Result<T, ProviderConnectError> + Send + 'static,
) -> Result<T, ProviderConnectError> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| credential_problem(profile, crate::CredentialProblemKind::Unavailable))?
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum ProviderConnectError {
    #[error("profile `{0}` has invalid endpoint settings")]
    InvalidProfile(ProfileId),
    #[error("profile `{0}` has an invalid model selection")]
    InvalidModel(ProfileId),
    #[error("profile `{0}` has incompatible model options")]
    InvalidModelOptions(ProfileId),
    #[error("profile `{0}` requires sign-in or an API key")]
    LoginRequired(ProfileId),
    #[error("{0}")]
    Credential(crate::CredentialProblem),
    #[error("authorization failed for profile `{0}`")]
    AuthorizationFailed(ProfileId),
}

fn api_key_error(profile: ProfileId, error: ApiKeyCredentialError) -> ProviderConnectError {
    match error {
        ApiKeyCredentialError::MissingApiKey => {
            credential_problem(profile, crate::CredentialProblemKind::Missing)
        }
        ApiKeyCredentialError::EndpointMismatch => {
            credential_problem(profile, crate::CredentialProblemKind::EndpointMismatch)
        }
        ApiKeyCredentialError::InvalidApiKey | ApiKeyCredentialError::Unavailable => {
            credential_problem(profile, crate::CredentialProblemKind::Unavailable)
        }
    }
}

fn credential_problem(
    profile: ProfileId,
    kind: crate::CredentialProblemKind,
) -> ProviderConnectError {
    ProviderConnectError::Credential(crate::CredentialProblem { profile, kind })
}

fn chatgpt_error(error: CredentialError) -> ProviderConnectError {
    match error {
        CredentialError::Unavailable => credential_problem(
            ProfileId::chatgpt(),
            crate::CredentialProblemKind::Unavailable,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime(workspace: PathBuf) -> RuntimeConfig {
        let resolved = ResolvedModel {
            selection: crate::config::ModelSelection::new(ProfileId::chatgpt(), "offline-model")
                .unwrap(),
            profile: Profile::chatgpt(),
        };
        RuntimeConfig {
            coordinator: resolved.clone(),
            worker: resolved,
            limits: bone_core::AgentLimits::default(),
            tools: crate::ToolLimits::default(),
            workspace,
        }
    }

    #[tokio::test]
    async fn cached_endpoints_do_not_refresh_and_live_ports_do_not_block_logout() {
        let directory = tempfile::tempdir().unwrap();
        let credentials = ChatGptCredentials::at(directory.path().join("credentials")).unwrap();
        let auth = credentials.auth_file().unwrap();
        // An expired token would require network access if construction authorized.
        crate::safe_file::atomic_write_private(
            &auth,
            br#"{"access_token":"expired","refresh_token":"unused","expires_at":1}"#,
        )
        .unwrap();
        let first = ProviderConnector::with_chatgpt_credentials(credentials.clone());
        let second = ProviderConnector::with_chatgpt_credentials(credentials);
        let runtime = runtime(directory.path().to_path_buf());
        let _first_port = first.connect(&runtime).await.unwrap();
        let _second_port = second.connect(&runtime).await.unwrap();

        first.logout(&Profile::chatgpt()).await.unwrap();
        assert!(!auth.exists());
        assert!(matches!(
            second.connect(&runtime).await,
            Err(ProviderConnectError::LoginRequired(_))
        ));
    }

    #[tokio::test]
    async fn missing_cache_requires_explicit_login_without_starting_network() {
        let directory = tempfile::tempdir().unwrap();
        let credentials = ChatGptCredentials::at(directory.path().join("credentials")).unwrap();
        let connector = ProviderConnector::with_chatgpt_credentials(credentials);
        assert!(matches!(
            connector.connect_chatgpt().await,
            Err(ProviderConnectError::LoginRequired(_))
        ));
    }
}
