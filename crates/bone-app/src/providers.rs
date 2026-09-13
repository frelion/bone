//! Concrete composition of saved profiles, private credentials, and models.

use std::{collections::HashMap, sync::Arc};

use bone_adapters::{
    ConfiguredModel, ModelAdapter,
    llm::{
        Endpoint, EndpointConfig,
        protocol::{anthropic_messages, openai_chat_completions, openai_responses},
        service::chatgpt_subscription::{self, DeviceCodePrompt},
    },
};
use bone_core::{
    CallContext, CallError, CheckpointDraft, CompactInput, CoordinateInput, KernelDecision,
    ModelPort, PortFuture, WorkInput, WorkProposal,
};
use tokio::sync::{Mutex, RwLock};

use crate::{
    config::{Profile, ProfileId, ResolvedModel, RuntimeConfig},
    credentials::{
        ApiKey, ApiKeyCredentialError, ApiKeyCredentials, ChatGptCredentials, CredentialError,
    },
};

/// API-key endpoints are rebuilt per runtime. ChatGPT runtimes share an
/// endpoint, including Rig's refresh lock and the credential-cache lease.
#[derive(Clone, Default)]
pub(crate) struct ProviderConnector {
    chatgpt: Arc<Mutex<ChatGptState>>,
    chatgpt_operation: Arc<RwLock<()>>,
    #[cfg(test)]
    test_endpoints: HashMap<ProfileId, Endpoint>,
}

#[derive(Default)]
struct ChatGptState {
    credentials: Option<ChatGptCredentials>,
    connection: Option<Arc<ChatGptConnection>>,
    closed: bool,
}

struct ChatGptConnection {
    endpoint: Endpoint,
}

impl ChatGptState {
    fn credentials(&mut self) -> Result<ChatGptCredentials, CredentialError> {
        if self.credentials.is_none() {
            self.credentials = Some(ChatGptCredentials::default_for_current_user()?);
        }
        Ok(self
            .credentials
            .as_ref()
            .expect("credentials initialized")
            .clone())
    }

    fn release_idle(&mut self, profile: &ProfileId) -> Result<(), ProviderConnectError> {
        if self
            .connection
            .as_ref()
            .is_some_and(|connection| Arc::strong_count(connection) > 1)
        {
            return Err(ProviderConnectError::Busy(profile.clone()));
        }
        self.connection = None;
        Ok(())
    }
}

impl ProviderConnector {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub(crate) fn with_endpoints(endpoints: HashMap<ProfileId, Endpoint>) -> Self {
        Self {
            test_endpoints: endpoints,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn with_chatgpt_credentials(credentials: ChatGptCredentials) -> Self {
        Self {
            chatgpt: Arc::new(Mutex::new(ChatGptState {
                credentials: Some(credentials),
                connection: None,
                closed: false,
            })),
            ..Self::default()
        }
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
            .configured_model(&runtime.coordinator, chatgpt.as_deref(), &mut endpoints)
            .await?;
        let worker = self
            .configured_model(&runtime.worker, chatgpt.as_deref(), &mut endpoints)
            .await?;
        Ok(Arc::new(ConnectedModels {
            adapter: ModelAdapter::new(coordinator, worker),
            chatgpt,
        }))
    }

    /// Prove that one model can be constructed from its saved connection
    /// without starting a Session or an interactive sign-in flow.
    pub(crate) async fn prepare_model(
        &self,
        model: &ResolvedModel,
    ) -> Result<(), ProviderConnectError> {
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

        let chatgpt = if matches!(model.profile.endpoint, EndpointConfig::ChatGptSubscription) {
            Some(self.connect_chatgpt().await?)
        } else {
            None
        };
        let mut endpoints = HashMap::new();
        #[cfg(test)]
        endpoints.extend(self.test_endpoints.clone());
        self.configured_model(model, chatgpt.as_deref(), &mut endpoints)
            .await?;
        Ok(())
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
        let _operation = self
            .chatgpt_operation
            .try_write()
            .map_err(|_| ProviderConnectError::Busy(profile.id.clone()))?;
        let mut state = self
            .chatgpt
            .try_lock()
            .map_err(|_| ProviderConnectError::Busy(profile.id.clone()))?;
        if state.closed {
            return Err(ProviderConnectError::Closed);
        }
        if state
            .connection
            .as_ref()
            .is_some_and(|connection| Arc::strong_count(connection) > 1)
        {
            // A live runtime already holds an authenticated connection. Login
            // is idempotent in that state; replacing its credentials would
            // invalidate work that still owns the connection.
            return Ok(());
        }
        state.release_idle(&profile.id)?;
        let auth = acquire_chatgpt(&mut state).await?;
        let endpoint = chatgpt_subscription::connect(profile.id.as_str(), auth, on_device_code)
            .await
            .map_err(|_| ProviderConnectError::AuthorizationFailed(profile.id.clone()))?;
        state.connection = Some(Arc::new(ChatGptConnection { endpoint }));
        Ok(())
    }

    pub(crate) async fn logout(&self, profile: &Profile) -> Result<(), ProviderConnectError> {
        validate_profile(profile)?;
        if matches!(profile.endpoint, EndpointConfig::ChatGptSubscription) {
            let _operation = self
                .chatgpt_operation
                .try_write()
                .map_err(|_| ProviderConnectError::Busy(profile.id.clone()))?;
            let mut state = self
                .chatgpt
                .try_lock()
                .map_err(|_| ProviderConnectError::Busy(profile.id.clone()))?;
            if state.closed {
                return Err(ProviderConnectError::Closed);
            }
            state.release_idle(&profile.id)?;
            let credentials = state.credentials().map_err(chatgpt_error)?;
            return credential_task(profile.id.clone(), move || {
                credentials.clear().map_err(chatgpt_error)
            })
            .await;
        }
        let profile = profile.clone();
        credential_task(profile.id.clone(), move || {
            ApiKeyCredentials::for_profile(&profile)
                .and_then(|credentials| credentials.clear())
                .map_err(|error| api_key_error(profile.id, error))
        })
        .await
    }

    /// Release idle process-local connections during App shutdown without
    /// deleting credentials needed by the next App instance.
    pub(crate) async fn close(&self) -> Result<(), ProviderConnectError> {
        let _operation = self
            .chatgpt_operation
            .try_write()
            .map_err(|_| ProviderConnectError::Busy(ProfileId::chatgpt()))?;
        let mut state = self
            .chatgpt
            .try_lock()
            .map_err(|_| ProviderConnectError::Busy(ProfileId::chatgpt()))?;
        state.closed = true;
        state.release_idle(&ProfileId::chatgpt())
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
        credential_task(profile.id.clone(), move || {
            ApiKeyCredentials::for_profile(&profile)
                .and_then(|credentials| credentials.save(&key))
                .map_err(|error| api_key_error(profile.id, error))
        })
        .await
    }

    async fn connect_chatgpt(&self) -> Result<Arc<ChatGptConnection>, ProviderConnectError> {
        let profile = ProfileId::chatgpt();
        let _operation = self
            .chatgpt_operation
            .try_read()
            .map_err(|_| ProviderConnectError::Busy(profile.clone()))?;
        let mut state = self.chatgpt.lock().await;
        if state.closed {
            return Err(ProviderConnectError::Closed);
        }
        if let Some(connection) = &state.connection {
            return Ok(Arc::clone(connection));
        }
        let auth = acquire_chatgpt(&mut state).await?;
        let endpoint = chatgpt_subscription::connect_cached(profile.as_str(), auth)
            .await
            .map_err(|error| match error {
                chatgpt_subscription::Error::AuthorizationFailed => {
                    ProviderConnectError::LoginRequired(profile.clone())
                }
                _ => ProviderConnectError::InvalidProfile(profile.clone()),
            })?;
        let connection = Arc::new(ChatGptConnection { endpoint });
        state.connection = Some(Arc::clone(&connection));
        Ok(connection)
    }

    async fn configured_model(
        &self,
        resolved: &ResolvedModel,
        chatgpt: Option<&ChatGptConnection>,
        endpoints: &mut HashMap<ProfileId, Endpoint>,
    ) -> Result<ConfiguredModel, ProviderConnectError> {
        let profile = &resolved.profile;
        let endpoint = match endpoints.get(&profile.id) {
            Some(endpoint) => endpoint.clone(),
            None => {
                let endpoint = if matches!(profile.endpoint, EndpointConfig::ChatGptSubscription) {
                    chatgpt
                        .expect("ChatGPT connection resolved before model selection")
                        .endpoint
                        .clone()
                } else {
                    let owned_profile = profile.clone();
                    let key = credential_task(profile.id.clone(), move || {
                        ApiKeyCredentials::for_profile(&owned_profile)
                            .and_then(|credentials| credentials.read())
                            .map_err(|error| api_key_error(owned_profile.id, error))
                    })
                    .await?;
                    api_endpoint(profile, key)?
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

async fn acquire_chatgpt(
    state: &mut ChatGptState,
) -> Result<crate::credentials::ChatGptAuthLease, ProviderConnectError> {
    let credentials = state.credentials().map_err(chatgpt_error)?;
    credential_task(ProfileId::chatgpt(), move || {
        credentials.acquire().map_err(chatgpt_error)
    })
    .await
}

/// Retain the shared connection through both runtime and individual call
/// lifetimes, including calls still draining after runtime shutdown.
struct ConnectedModels {
    adapter: ModelAdapter,
    chatgpt: Option<Arc<ChatGptConnection>>,
}

impl ConnectedModels {
    fn retain_connection<T: Send + 'static>(&self, future: PortFuture<T>) -> PortFuture<T> {
        let connection = self.chatgpt.clone();
        Box::pin(async move {
            let _connection = connection;
            future.await
        })
    }
}

impl ModelPort for ConnectedModels {
    fn coordinate(
        &self,
        input: CoordinateInput,
        context: CallContext,
    ) -> PortFuture<Result<KernelDecision, CallError>> {
        self.retain_connection(self.adapter.coordinate(input, context))
    }

    fn work(
        &self,
        input: WorkInput,
        context: CallContext,
    ) -> PortFuture<Result<WorkProposal, CallError>> {
        self.retain_connection(self.adapter.work(input, context))
    }

    fn compact(
        &self,
        input: CompactInput,
        context: CallContext,
    ) -> PortFuture<Result<CheckpointDraft, CallError>> {
        self.retain_connection(self.adapter.compact(input, context))
    }
}

fn api_endpoint(profile: &Profile, key: ApiKey) -> Result<Endpoint, ProviderConnectError> {
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
        .map_err(|_| ProviderConnectError::CredentialUnavailable(profile))?
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum ProviderConnectError {
    #[error("the provider connector is closed")]
    Closed,
    #[error("profile `{0}` has invalid endpoint settings")]
    InvalidProfile(ProfileId),
    #[error("profile `{0}` has an invalid model selection")]
    InvalidModel(ProfileId),
    #[error("profile `{0}` has incompatible model options")]
    InvalidModelOptions(ProfileId),
    #[error("profile `{0}` requires sign-in or an API key")]
    LoginRequired(ProfileId),
    #[error("credential storage is unavailable for profile `{0}`")]
    CredentialUnavailable(ProfileId),
    #[error("authorization failed for profile `{0}`")]
    AuthorizationFailed(ProfileId),
    #[error("profile `{0}` is in use")]
    Busy(ProfileId),
}

fn api_key_error(profile: ProfileId, error: ApiKeyCredentialError) -> ProviderConnectError {
    match error {
        ApiKeyCredentialError::MissingApiKey => ProviderConnectError::LoginRequired(profile),
        ApiKeyCredentialError::InvalidApiKey | ApiKeyCredentialError::Unavailable => {
            ProviderConnectError::CredentialUnavailable(profile)
        }
    }
}

fn chatgpt_error(error: CredentialError) -> ProviderConnectError {
    match error {
        CredentialError::Busy => ProviderConnectError::Busy(ProfileId::chatgpt()),
        CredentialError::Unavailable => {
            ProviderConnectError::CredentialUnavailable(ProfileId::chatgpt())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{future::Future, task::Poll};

    use bone_adapters::llm::Protocol;
    use bone_adapters::llm::service::chatgpt_subscription::ChatGptAuthCache;

    use super::*;

    #[test]
    fn api_profiles_use_the_selected_protocol() {
        for (config, protocol) in [
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
        ] {
            let profile = Profile::new(ProfileId::new("test").unwrap(), "Test", config).unwrap();
            let endpoint = api_endpoint(&profile, ApiKey::new("test-key".into()).unwrap()).unwrap();
            assert_eq!(endpoint.id(), "test");
            assert_eq!(endpoint.protocol(), protocol);
        }
    }

    #[tokio::test]
    async fn runtime_connection_without_cached_auth_requires_explicit_login() {
        let directory = tempfile::tempdir().unwrap();
        let credentials = ChatGptCredentials::at(directory.path().join("credentials")).unwrap();
        let connector = ProviderConnector::with_chatgpt_credentials(credentials.clone());
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            connector.connect_chatgpt(),
        )
        .await
        .unwrap();
        assert!(matches!(
            result,
            Err(ProviderConnectError::LoginRequired(_))
        ));
        credentials.clear().unwrap();
    }

    #[tokio::test]
    async fn runtime_connection_is_busy_while_interactive_login_owns_the_state() {
        let directory = tempfile::tempdir().unwrap();
        let credentials = ChatGptCredentials::at(directory.path().join("credentials")).unwrap();
        let connector = ProviderConnector::with_chatgpt_credentials(credentials);
        let _interactive_login = connector.chatgpt_operation.write().await;

        let result = connector.connect_chatgpt().await;

        assert!(matches!(
            result,
            Err(ProviderConnectError::Busy(profile)) if profile == ProfileId::chatgpt()
        ));
        assert_eq!(
            connector.close().await,
            Err(ProviderConnectError::Busy(ProfileId::chatgpt()))
        );
    }

    #[tokio::test]
    async fn concurrent_runtime_connections_wait_and_share_the_cached_connection() {
        let directory = tempfile::tempdir().unwrap();
        let credentials = ChatGptCredentials::at(directory.path().join("credentials")).unwrap();
        let lease = credentials.acquire().unwrap();
        std::fs::write(
            lease.auth_file(),
            br#"{"access_token":"offline-test-token","expires_at":4102444800,"account_id":"offline-account"}"#,
        )
        .unwrap();
        drop(lease);
        let connector = ProviderConnector::with_chatgpt_credentials(credentials);

        let state = connector.chatgpt.lock().await;
        let mut first = Box::pin(connector.connect_chatgpt());
        std::future::poll_fn(|context| match first.as_mut().poll(context) {
            Poll::Pending => Poll::Ready(()),
            Poll::Ready(_) => panic!("the first connection did not contend on runtime state"),
        })
        .await;
        let mut second = Box::pin(connector.connect_chatgpt());
        std::future::poll_fn(|context| match second.as_mut().poll(context) {
            Poll::Pending => Poll::Ready(()),
            Poll::Ready(_) => panic!("the second connection did not contend on runtime state"),
        })
        .await;
        drop(state);

        let (first, second) = tokio::join!(first, second);
        let first = first.unwrap();
        let second = second.unwrap();
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[tokio::test]
    async fn model_preflight_reuses_a_live_chatgpt_connection() {
        let directory = tempfile::tempdir().unwrap();
        let credentials = ChatGptCredentials::at(directory.path().join("credentials")).unwrap();
        let lease = credentials.acquire().unwrap();
        std::fs::write(
            lease.auth_file(),
            br#"{"access_token":"offline-test-token","expires_at":4102444800,"account_id":"offline-account"}"#,
        )
        .unwrap();
        drop(lease);
        let connector = ProviderConnector::with_chatgpt_credentials(credentials);
        let resolved = ResolvedModel {
            selection: crate::config::ModelSelection::new(ProfileId::chatgpt(), "gpt-5.4").unwrap(),
            profile: Profile::chatgpt(),
        };
        let runtime = RuntimeConfig {
            coordinator: resolved.clone(),
            worker: resolved.clone(),
            limits: bone_core::AgentLimits::default(),
            tools: crate::config::ToolSettings::default(),
            workspace: directory.path().to_path_buf(),
        };

        let live_runtime = connector.connect(&runtime).await.unwrap();
        connector.prepare_model(&resolved).await.unwrap();
        connector.prepare_model(&resolved).await.unwrap();
        connector.login(&Profile::chatgpt(), |_| {}).await.unwrap();

        drop(live_runtime);
        connector.close().await.unwrap();
    }

    #[tokio::test]
    async fn close_prevents_a_later_login_from_reopening_the_connector() {
        let directory = tempfile::tempdir().unwrap();
        let credentials = ChatGptCredentials::at(directory.path().join("credentials")).unwrap();
        let connector = ProviderConnector::with_chatgpt_credentials(credentials);
        connector.close().await.unwrap();

        assert_eq!(
            connector.login(&Profile::chatgpt(), |_| {}).await,
            Err(ProviderConnectError::Closed)
        );
        assert!(connector.chatgpt.lock().await.connection.is_none());
    }

    #[tokio::test]
    async fn cached_auth_is_shared_and_logout_waits_for_all_runtimes() {
        let directory = tempfile::tempdir().unwrap();
        let credentials = ChatGptCredentials::at(directory.path().join("credentials")).unwrap();
        let lease = credentials.acquire().unwrap();
        let auth_file = lease.auth_file().to_path_buf();
        std::fs::write(&auth_file, br#"{"access_token":"offline-test-token","expires_at":4102444800,"account_id":"offline-account"}"#).unwrap();
        drop(lease);
        let connector = ProviderConnector::with_chatgpt_credentials(credentials.clone());
        let selection =
            crate::config::ModelSelection::new(ProfileId::chatgpt(), "offline-model").unwrap();
        let resolved = ResolvedModel {
            selection,
            profile: Profile::chatgpt(),
        };
        let runtime = RuntimeConfig {
            coordinator: resolved.clone(),
            worker: resolved,
            limits: bone_core::AgentLimits::default(),
            tools: crate::config::ToolSettings::default(),
            workspace: directory.path().to_path_buf(),
        };
        let first = connector.connect(&runtime).await.unwrap();
        let second = connector.connect(&runtime).await.unwrap();
        let shared = connector.chatgpt.lock().await;
        assert_eq!(Arc::strong_count(shared.connection.as_ref().unwrap()), 3);
        drop(shared);
        assert_eq!(
            connector.logout(&Profile::chatgpt()).await,
            Err(ProviderConnectError::Busy(ProfileId::chatgpt()))
        );
        drop(first);
        assert_eq!(
            connector.logout(&Profile::chatgpt()).await,
            Err(ProviderConnectError::Busy(ProfileId::chatgpt()))
        );
        drop(second);
        connector.logout(&Profile::chatgpt()).await.unwrap();
        assert!(!auth_file.exists());
    }

    #[test]
    fn in_flight_model_future_retains_connection_after_adapter_drops() {
        let endpoint = openai_responses::official("chatgpt", "test-key").unwrap();
        let model = endpoint.model("test-model").unwrap();
        let connection = Arc::new(ChatGptConnection { endpoint });
        let mut state = ChatGptState {
            connection: Some(Arc::clone(&connection)),
            ..ChatGptState::default()
        };
        let models = ConnectedModels {
            adapter: ModelAdapter::new(model.clone(), model),
            chatgpt: Some(connection),
        };
        let future = models.retain_connection(Box::pin(std::future::pending::<()>()));
        drop(models);
        assert_eq!(
            state.release_idle(&ProfileId::chatgpt()),
            Err(ProviderConnectError::Busy(ProfileId::chatgpt()))
        );
        drop(future);
        state.release_idle(&ProfileId::chatgpt()).unwrap();
    }

    #[test]
    fn idle_connections_are_released_but_live_runtime_connections_block_logout() {
        let endpoint = openai_responses::official("chatgpt", "test-key").unwrap();
        let live = Arc::new(ChatGptConnection { endpoint });
        let mut state = ChatGptState {
            connection: Some(Arc::clone(&live)),
            ..ChatGptState::default()
        };
        assert_eq!(
            state.release_idle(&ProfileId::chatgpt()),
            Err(ProviderConnectError::Busy(ProfileId::chatgpt()))
        );
        assert!(state.connection.is_some());
        drop(live);
        state.release_idle(&ProfileId::chatgpt()).unwrap();
        assert!(state.connection.is_none());
    }
}
