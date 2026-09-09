use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use bone_adapters::llm::service::chatgpt_subscription::DeviceCodePrompt;
use bone_core::{ModelPort, ToolPort};
use tokio::sync::{Mutex, oneshot, watch};

use crate::{
    ApiKey, AppOptions, AppShutdownReport, ConfigChange, ConfigProblem, ConfigScope, DataStore,
    Error, LoginState, Profile, ProfileId, ProviderConnector, ResolvedConfig, Result,
    RuntimeConfig, RuntimeOverrides, Session, SessionId, SessionInfo, WorkspaceId, WorkspaceInfo,
    config::{resolve_runtime, validate_agent_limits, validate_tool_settings},
    providers::ProviderConnectError,
};

/// Owns durable product state, provider connections, and live Session tasks.
#[derive(Clone)]
pub struct App {
    inner: Arc<AppInner>,
}

struct AppInner {
    store: DataStore,
    backend: RuntimeBackend,
    providers: ProviderConnector,
    /// Orders durable configuration writes through live Session acknowledgement.
    config_updates: Mutex<()>,
    sessions: Mutex<BTreeMap<SessionId, Session>>,
    write_gates: Mutex<BTreeMap<WorkspaceId, Arc<crate::tools::WriteGate>>>,
    shutdown: Mutex<Shutdown>,
    closed: Arc<AtomicBool>,
}

#[derive(Default)]
enum Shutdown {
    #[default]
    Idle,
    Running(tokio::task::JoinHandle<Result<AppShutdownReport>>),
    Complete(AppShutdownReport),
}

#[derive(Clone)]
pub(crate) enum RuntimeBackend {
    Providers(ProviderConnector),
    #[cfg(test)]
    Ports {
        model: Arc<dyn ModelPort>,
        tools: Option<Vec<Arc<dyn ToolPort>>>,
    },
}

impl RuntimeBackend {
    pub(crate) async fn connect(&self, config: &RuntimeConfig) -> Result<Arc<dyn ModelPort>> {
        match self {
            Self::Providers(providers) => providers.connect(config).await.map_err(Into::into),
            #[cfg(test)]
            Self::Ports { model, .. } => Ok(Arc::clone(model)),
        }
    }

    pub(crate) fn tools(
        &self,
        config: &RuntimeConfig,
        context: crate::tools::ToolContext,
    ) -> Result<Vec<Arc<dyn ToolPort>>> {
        match self {
            Self::Providers(_) => crate::tools::assemble(config, context)
                .map_err(|error| Error::Tools(error.to_string())),
            #[cfg(test)]
            Self::Ports {
                tools: Some(tools), ..
            } => Ok(tools.clone()),
            #[cfg(test)]
            Self::Ports { tools: None, .. } => crate::tools::assemble(config, context)
                .map_err(|error| Error::Tools(error.to_string())),
        }
    }
}

impl App {
    #[cfg(test)]
    pub(crate) fn test_store(&self) -> DataStore {
        self.inner.store.clone()
    }

    #[cfg(test)]
    pub(crate) async fn test_write_gate(
        &self,
        workspace: WorkspaceId,
    ) -> Arc<crate::tools::WriteGate> {
        Arc::clone(
            self.inner
                .write_gates
                .lock()
                .await
                .get(&workspace)
                .expect("workspace write gate exists"),
        )
    }

    pub async fn open(options: AppOptions) -> Result<Self> {
        let store = DataStore::open(options.data_dir)?;
        let providers = ProviderConnector::new();
        Ok(Self::from_parts(
            store,
            RuntimeBackend::Providers(providers.clone()),
            providers,
        ))
    }

    #[cfg(test)]
    pub(crate) async fn with_ports(
        options: AppOptions,
        model: Arc<dyn ModelPort>,
        tools: Vec<Arc<dyn ToolPort>>,
    ) -> Result<Self> {
        let store = DataStore::open(options.data_dir)?;
        let providers = ProviderConnector::new();
        Ok(Self::from_parts(
            store,
            RuntimeBackend::Ports {
                model,
                tools: Some(tools),
            },
            providers,
        ))
    }

    #[cfg(test)]
    pub(crate) async fn with_model(options: AppOptions, model: Arc<dyn ModelPort>) -> Result<Self> {
        let store = DataStore::open(options.data_dir)?;
        let providers = ProviderConnector::new();
        Ok(Self::from_parts(
            store,
            RuntimeBackend::Ports { model, tools: None },
            providers,
        ))
    }

    #[cfg(test)]
    pub(crate) async fn with_provider_connector(
        options: AppOptions,
        providers: ProviderConnector,
    ) -> Result<Self> {
        let store = DataStore::open(options.data_dir)?;
        Ok(Self::from_parts(
            store,
            RuntimeBackend::Providers(providers.clone()),
            providers,
        ))
    }

    fn from_parts(store: DataStore, backend: RuntimeBackend, providers: ProviderConnector) -> Self {
        Self {
            inner: Arc::new(AppInner {
                store,
                backend,
                providers,
                config_updates: Mutex::new(()),
                sessions: Mutex::new(BTreeMap::new()),
                write_gates: Mutex::new(BTreeMap::new()),
                shutdown: Mutex::new(Shutdown::Idle),
                closed: Arc::new(AtomicBool::new(false)),
            }),
        }
    }

    pub async fn open_workspace(
        &self,
        path: impl Into<std::path::PathBuf>,
    ) -> Result<WorkspaceInfo> {
        self.ensure_open()?;
        let path = path.into();
        if !path.is_dir() {
            return Err(Error::InvalidState(
                "workspace must be an existing directory".into(),
            ));
        }
        self.inner.store.workspace(&path).map_err(Into::into)
    }

    pub async fn create_session(
        &self,
        workspace: WorkspaceId,
        title: impl Into<String>,
    ) -> Result<Session> {
        let title = title.into();
        validate_title(&title)?;
        let mut sessions = self.inner.sessions.lock().await;
        self.ensure_open()?;
        if self.inner.store.workspace_by_id(workspace)?.is_none() {
            return Err(Error::WorkspaceNotFound);
        }
        let saved = self.inner.store.create_session(workspace, title)?;
        self.register_session(&mut sessions, saved).await
    }

    pub async fn session(&self, id: SessionId) -> Result<Session> {
        let sessions = self.inner.sessions.lock().await;
        self.ensure_open()?;
        if let Some(session) = sessions.get(&id).cloned() {
            return Ok(session);
        }
        drop(sessions);
        let saved = self
            .inner
            .store
            .session(id)?
            .ok_or(Error::SessionNotFound)?;
        self.open_session(saved).await
    }

    pub async fn list_sessions(&self, workspace: WorkspaceId) -> Result<Vec<SessionInfo>> {
        self.ensure_open()?;
        if self.inner.store.workspace_by_id(workspace)?.is_none() {
            return Err(Error::WorkspaceNotFound);
        }
        self.inner.store.sessions(workspace).map_err(Into::into)
    }

    /// Returns the authoritative set of writes that still need inspection.
    pub async fn unresolved_writes(
        &self,
        workspace: WorkspaceId,
    ) -> Result<Vec<crate::UnresolvedWriteView>> {
        self.ensure_open()?;
        if self.inner.store.workspace_by_id(workspace)?.is_none() {
            return Err(Error::WorkspaceNotFound);
        }
        self.inner
            .store
            .unresolved_writes(workspace, None)
            .map_err(Into::into)
    }

    pub async fn config(&self, scope: ConfigScope) -> Result<RuntimeOverrides> {
        self.ensure_open()?;
        self.ensure_scope(scope)?;
        self.inner.store.config(scope).map_err(Into::into)
    }

    /// Persist one override and wait for every affected open Session. Success
    /// means all applied it; a failure does not roll back Sessions that did.
    /// Once dispatched, cancelling the future stops waiting but does not revoke
    /// the update.
    pub async fn update_config(
        &self,
        scope: ConfigScope,
        change: ConfigChange,
    ) -> Result<RuntimeOverrides> {
        match &change {
            ConfigChange::Worker(value) => {
                if let Some(value) = value {
                    value
                        .validate()
                        .map_err(|error| Error::InvalidState(error.to_string()))?;
                }
            }
            ConfigChange::Coordinator(value) => {
                if let Some(value) = value {
                    value
                        .validate()
                        .map_err(|error| Error::InvalidState(error.to_string()))?;
                }
            }
            ConfigChange::Limits(value) => {
                if let Some(value) = value {
                    validate_agent_limits(value)
                        .map_err(|error| Error::InvalidState(error.to_string()))?;
                }
            }
            ConfigChange::Tools(value) => {
                if let Some(value) = value {
                    validate_tool_settings(value)
                        .map_err(|error| Error::InvalidState(error.to_string()))?;
                }
            }
        }

        let app = self.clone();
        await_app_task(tokio::spawn(async move {
            app.apply_config_update(scope, change).await
        }))
        .await
    }

    async fn apply_config_update(
        &self,
        scope: ConfigScope,
        change: ConfigChange,
    ) -> Result<RuntimeOverrides> {
        let _update = self.inner.config_updates.lock().await;
        let sessions = self.inner.sessions.lock().await;
        self.ensure_open()?;
        self.ensure_scope(scope)?;
        let saved = self
            .inner
            .store
            .update_config(scope, change)
            .map_err(Error::from)?;

        let targets = sessions
            .values()
            .filter(|session| config_affects(scope, session))
            .cloned()
            .collect::<Vec<_>>();
        drop(sessions);
        reload_sessions(&targets).await?;
        Ok(saved)
    }

    /// Return the durable desired configuration and the last configuration
    /// installed in this App instance's live Runtime.
    pub async fn resolved_config(&self, session: SessionId) -> Result<ResolvedConfig> {
        let _update = self.inner.config_updates.lock().await;
        let sessions = self.inner.sessions.lock().await;
        self.ensure_open()?;
        let saved = self
            .inner
            .store
            .session(session)?
            .ok_or(Error::SessionNotFound)?;
        let workspace = self
            .inner
            .store
            .workspace_by_id(saved.info.workspace)?
            .ok_or(Error::WorkspaceNotFound)?;
        let global = self.inner.store.global_settings()?;
        let workspace_config = self
            .inner
            .store
            .config(ConfigScope::Workspace(workspace.id))?;
        let session_config = self.inner.store.config(ConfigScope::Session(session))?;
        let desired = resolve_runtime(
            &global,
            Some(&workspace_config),
            Some(&session_config),
            &self.inner.store.profiles()?,
            workspace.root,
        );
        let live = sessions.get(&session).cloned();
        let running = live.and_then(|session| {
            let view = session.observe();
            match &view.borrow().runtime {
                crate::RuntimeState::Running { config, .. } => Some(config.as_ref().clone()),
                _ => None,
            }
        });
        Ok(ResolvedConfig { desired, running })
    }

    pub async fn profiles(&self) -> Result<Vec<Profile>> {
        self.ensure_open()?;
        self.inner.store.profiles().map_err(Into::into)
    }

    /// Save a profile and apply its resolved value to open Sessions. A failure
    /// does not roll back Sessions that already applied it.
    /// Once dispatched, cancelling the future stops waiting but does not revoke
    /// the update.
    pub async fn save_profile(&self, profile: Profile) -> Result<()> {
        profile
            .validate()
            .map_err(|error| Error::InvalidState(error.to_string()))?;
        let app = self.clone();
        await_app_task(tokio::spawn(
            async move { app.apply_profile(profile).await },
        ))
        .await
    }

    async fn apply_profile(&self, profile: Profile) -> Result<()> {
        let _update = self.inner.config_updates.lock().await;
        let sessions = self.inner.sessions.lock().await;
        self.ensure_open()?;
        self.inner.store.save_profile(profile)?;
        let targets = sessions.values().cloned().collect::<Vec<_>>();
        drop(sessions);
        reload_sessions(&targets).await
    }

    pub async fn set_api_key(&self, profile: ProfileId, key: ApiKey) -> Result<()> {
        self.ensure_open()?;
        let profile = self.profile(&profile)?;
        self.inner
            .providers
            .set_api_key(&profile, key)
            .await
            .map_err(Into::into)
    }

    pub async fn login(&self, profile: ProfileId) -> Result<LoginAttempt> {
        self.ensure_open()?;
        let profile = self.profile(&profile)?;
        let providers = self.inner.providers.clone();
        let (state_tx, state) = watch::channel(Arc::new(LoginState::Connecting));
        let prompt_tx = state_tx.clone();
        let (cancel, cancellation) = oneshot::channel();
        tokio::spawn(async move {
            let login = providers.login(&profile, move |prompt: DeviceCodePrompt| {
                prompt_tx.send_replace(Arc::new(LoginState::DeviceCode {
                    verification_uri: prompt.verification_uri,
                    user_code: prompt.user_code,
                }));
            });
            tokio::select! {
                result = login => {
                    let state = match result {
                        Ok(()) => LoginState::Succeeded,
                        Err(error) => LoginState::Failed { message: error.to_string() },
                    };
                    state_tx.send_replace(Arc::new(state));
                }
                _ = cancellation => {
                    state_tx.send_replace(Arc::new(LoginState::Cancelled));
                }
            }
        });
        Ok(LoginAttempt {
            state,
            cancel: Some(cancel),
        })
    }

    pub async fn logout(&self, profile: ProfileId) -> Result<()> {
        self.ensure_open()?;
        let profile = self.profile(&profile)?;
        self.inner
            .providers
            .logout(&profile)
            .await
            .map_err(Into::into)
    }

    pub async fn shutdown(&self) -> Result<AppShutdownReport> {
        let mut shutdown = self.inner.shutdown.lock().await;
        loop {
            match &mut *shutdown {
                Shutdown::Complete(report) => return Ok(report.clone()),
                Shutdown::Idle => {
                    let _update = self.inner.config_updates.lock().await;
                    let sessions = self.inner.sessions.lock().await;
                    self.inner.closed.store(true, Ordering::Release);
                    drop(sessions);
                    *shutdown =
                        Shutdown::Running(tokio::spawn(finish_shutdown(Arc::clone(&self.inner))));
                }
                Shutdown::Running(task) => {
                    let result = task.await;
                    match result {
                        Ok(Ok(report)) => {
                            *shutdown = Shutdown::Complete(report.clone());
                            return Ok(report);
                        }
                        Ok(Err(error)) => {
                            *shutdown = Shutdown::Idle;
                            return Err(error);
                        }
                        Err(error) => {
                            *shutdown = Shutdown::Idle;
                            return Err(Error::InvalidState(format!(
                                "shutdown task failed: {error}"
                            )));
                        }
                    }
                }
            }
        }
    }

    async fn open_session(&self, saved: crate::SavedSession) -> Result<Session> {
        let mut sessions = self.inner.sessions.lock().await;
        self.ensure_open()?;
        self.register_session(&mut sessions, saved).await
    }

    async fn register_session(
        &self,
        sessions: &mut BTreeMap<SessionId, Session>,
        saved: crate::SavedSession,
    ) -> Result<Session> {
        if let Some(session) = sessions.get(&saved.info.id) {
            return Ok(session.clone());
        }
        let write_gate = {
            let mut gates = self.inner.write_gates.lock().await;
            Arc::clone(
                gates
                    .entry(saved.info.workspace)
                    .or_insert_with(|| Arc::new(crate::tools::WriteGate::new())),
            )
        };
        let session = Session::spawn(
            self.inner.store.clone(),
            self.inner.backend.clone(),
            saved,
            write_gate,
            Arc::clone(&self.inner.closed),
        )?;
        sessions.insert(session.id(), session.clone());
        Ok(session)
    }

    fn profile(&self, id: &ProfileId) -> Result<Profile> {
        self.inner
            .store
            .profiles()?
            .into_iter()
            .find(|profile| &profile.id == id)
            .ok_or_else(|| Error::Configuration(ConfigProblem::MissingProfile(id.clone())))
    }

    fn ensure_scope(&self, scope: ConfigScope) -> Result<()> {
        match scope {
            ConfigScope::User => Ok(()),
            ConfigScope::Workspace(id) => self
                .inner
                .store
                .workspace_by_id(id)?
                .map(|_| ())
                .ok_or(Error::WorkspaceNotFound),
            ConfigScope::Session(id) => self
                .inner
                .store
                .session(id)?
                .map(|_| ())
                .ok_or(Error::SessionNotFound),
        }
    }

    fn ensure_open(&self) -> Result<()> {
        if self.inner.closed.load(Ordering::Acquire) {
            Err(Error::Closed)
        } else {
            Ok(())
        }
    }
}

/// Short-lived state for an explicit interactive login.
pub struct LoginAttempt {
    state: watch::Receiver<Arc<LoginState>>,
    cancel: Option<oneshot::Sender<()>>,
}

impl LoginAttempt {
    pub fn state(&self) -> Arc<LoginState> {
        self.state.borrow().clone()
    }

    pub fn observe(&self) -> watch::Receiver<Arc<LoginState>> {
        self.state.clone()
    }

    pub fn cancel(mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
}

impl Drop for LoginAttempt {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
}

async fn finish_shutdown(inner: Arc<AppInner>) -> Result<AppShutdownReport> {
    let sessions = inner
        .sessions
        .lock()
        .await
        .values()
        .cloned()
        .collect::<Vec<_>>();

    let mut shutdowns = tokio::task::JoinSet::new();
    for session in sessions {
        shutdowns.spawn(async move { session.shutdown().await });
    }
    let mut first_error = None;
    while let Some(result) = shutdowns.join_next().await {
        match result {
            Ok(Ok(_)) => {}
            Ok(Err(error)) if first_error.is_none() => first_error = Some(error),
            Err(error) if first_error.is_none() => {
                first_error = Some(Error::InvalidState(format!(
                    "session shutdown task failed: {error}"
                )));
            }
            _ => {}
        }
    }
    if let Err(error) = inner.providers.close().await
        && first_error.is_none()
    {
        first_error = Some(error.into());
    }
    if let Some(error) = first_error {
        return Err(error);
    }

    let mut unresolved_writes = Vec::new();
    for workspace in inner.store.workspaces()? {
        let gate = inner.write_gates.lock().await.get(&workspace.id).cloned();
        let writes = if let Some(gate) = gate {
            crate::tools::unresolved_after_write_gate(&inner.store, &gate, workspace.id, None)
                .await?
        } else {
            inner.store.unresolved_writes(workspace.id, None)?
        };
        unresolved_writes.extend(writes);
    }
    unresolved_writes.sort_by_key(|write| (write.workspace, write.session, write.call));
    inner.sessions.lock().await.clear();
    Ok(AppShutdownReport { unresolved_writes })
}

impl From<ProviderConnectError> for Error {
    fn from(error: ProviderConnectError) -> Self {
        match error {
            ProviderConnectError::Closed => Self::Closed,
            ProviderConnectError::LoginRequired(profile) => Self::LoginRequired(profile),
            ProviderConnectError::Busy(profile) => Self::ProfileBusy(profile),
            error => Self::Provider(error.to_string()),
        }
    }
}

fn validate_title(title: &str) -> Result<()> {
    if title.trim().is_empty() || title.trim() != title || title.len() > 200 {
        Err(Error::InvalidState("invalid session title".into()))
    } else {
        Ok(())
    }
}

fn config_affects(scope: ConfigScope, session: &Session) -> bool {
    match scope {
        ConfigScope::User => true,
        ConfigScope::Workspace(workspace) => session.workspace() == workspace,
        ConfigScope::Session(id) => session.id() == id,
    }
}

async fn reload_sessions(sessions: &[Session]) -> Result<()> {
    let reloads = sessions
        .iter()
        .cloned()
        .map(|session| tokio::spawn(async move { session.reload_config().await }))
        .collect::<Vec<_>>();

    let mut first_error = None;
    for reload in reloads {
        let result = reload.await;
        match result {
            Ok(Err(error)) if first_error.is_none() => first_error = Some(error),
            Err(error) if first_error.is_none() => {
                first_error = Some(Error::InvalidState(format!(
                    "session configuration task failed: {error}"
                )));
            }
            _ => {}
        }
    }
    first_error.map_or(Ok(()), Err)
}

async fn await_app_task<T>(task: tokio::task::JoinHandle<Result<T>>) -> Result<T> {
    task.await.map_err(|error| {
        Error::InvalidState(format!("application configuration task failed: {error}"))
    })?
}
