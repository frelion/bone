use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use bone_adapters::llm::service::chatgpt_subscription::DeviceCodePrompt;
use bone_core::{ModelPort, ToolPort};
use tokio::sync::{Mutex, oneshot, watch};

use crate::{
    AcceptanceCursor, AcceptancePage, ApiKey, AppOptions, AppShutdownReport, ConfigChange,
    ConfigProblem, ConfigScope, CreateSessionRequest, DataStore, Error, EvidenceAvailability,
    EvidenceCursor, EvidencePage, EvidenceRef, EvidenceSourcePage, HistoryCursor, LoginState,
    Profile, ProfileId, ProviderConnector, ResolvedConfig, Result, ResultArtifact, ResultPage,
    ResultRef, RuntimeConfig, RuntimeOverrides, Session, SessionId, SessionInfo,
    SessionReleaseReceipt, SessionReleaseStatus, WorkspaceChangeCursor, WorkspaceChangePage,
    WorkspaceFileCursor, WorkspaceFilePage, WorkspaceFileSource, WorkspaceFileView, WorkspaceId,
    WorkspaceInfo, WorkspaceOverview,
    config::{resolve_model, resolve_runtime, validate_agent_limits, validate_tool_limits},
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
    sessions: Mutex<BTreeMap<SessionId, LiveSession>>,
    write_gates: Mutex<BTreeMap<WorkspaceId, Arc<crate::tools::WriteGate>>>,
    shutdown: Mutex<Shutdown>,
    closed: Arc<AtomicBool>,
}

struct LiveSession {
    handle: Session,
    releasing: bool,
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
    #[cfg(test)]
    Factory(Arc<ModelFactory>),
}

#[cfg(test)]
type ModelFactory =
    dyn Fn(RuntimeConfig) -> bone_core::PortFuture<Result<Arc<dyn ModelPort>>> + Send + Sync;

impl RuntimeBackend {
    pub(crate) async fn connect(&self, config: &RuntimeConfig) -> Result<Arc<dyn ModelPort>> {
        match self {
            Self::Providers(providers) => providers.connect(config).await.map_err(Into::into),
            #[cfg(test)]
            Self::Ports { model, .. } => Ok(Arc::clone(model)),
            #[cfg(test)]
            Self::Factory(connect) => connect(config.clone()).await,
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
            #[cfg(test)]
            Self::Factory(_) => Ok(Vec::new()),
        }
    }
}

impl App {
    #[cfg(test)]
    pub(crate) fn with_backend(options: AppOptions, backend: RuntimeBackend) -> Result<Self> {
        let store = DataStore::open_with_home(&options.data_dir, &options.bone_home)?;
        let providers = ProviderConnector::new(options.bone_home);
        Ok(Self::from_parts(store, backend, providers))
    }
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

    #[cfg(test)]
    pub(crate) async fn test_open_session_count(&self) -> usize {
        self.inner.sessions.lock().await.len()
    }

    pub async fn open(options: AppOptions) -> Result<Self> {
        let store = DataStore::open_with_home(&options.data_dir, &options.bone_home)?;
        let providers = ProviderConnector::new(options.bone_home);
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
        let store = DataStore::open_with_home(&options.data_dir, &options.bone_home)?;
        let providers = ProviderConnector::new(options.bone_home);
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
        let store = DataStore::open_with_home(&options.data_dir, &options.bone_home)?;
        let providers = ProviderConnector::new(options.bone_home);
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
        let store = DataStore::open_with_home(&options.data_dir, &options.bone_home)?;
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

    /// Create a Session exactly once for a durable request identity.
    ///
    /// Retrying the same request returns the same Session, including after an
    /// App restart. Reusing its identity with different content is rejected.
    pub async fn create_session_idempotent(
        &self,
        request: CreateSessionRequest,
    ) -> Result<Session> {
        validate_title(&request.title)?;
        let mut sessions = self.inner.sessions.lock().await;
        self.ensure_open()?;
        if self
            .inner
            .store
            .workspace_by_id(request.workspace)?
            .is_none()
        {
            return Err(Error::WorkspaceNotFound);
        }
        let saved = self
            .inner
            .store
            .create_session_idempotent(
                request.request_id,
                request.workspace,
                request.title,
                request.provisional,
            )?
            .ok_or(Error::RequestConflict)?;
        self.register_session(&mut sessions, saved).await
    }

    pub async fn session(&self, id: SessionId) -> Result<Session> {
        let mut sessions = self.inner.sessions.lock().await;
        self.ensure_open()?;
        if let Some(session) = sessions.get(&id) {
            if session.releasing {
                return Err(Error::SessionBusy(id));
            }
            if !session.handle.actor_closed() {
                return Ok(session.handle.clone());
            }
            sessions.remove(&id);
        }
        drop(sessions);
        let saved = self
            .inner
            .store
            .session(id)?
            .ok_or(Error::SessionNotFound)?;
        self.open_session(saved).await
    }

    /// Release an idle Session actor and its exclusive writer lease. The App,
    /// not the frontend, decides whether live execution and durable state make
    /// release safe. A retained Session continues running in the background.
    pub async fn release_session(&self, id: SessionId) -> Result<SessionReleaseReceipt> {
        let app = self.clone();
        tokio::spawn(async move { app.release_session_inner(id).await })
            .await
            .map_err(|error| Error::InvalidState(format!("session release task failed: {error}")))?
    }

    async fn release_session_inner(&self, id: SessionId) -> Result<SessionReleaseReceipt> {
        let session = {
            let mut sessions = self.inner.sessions.lock().await;
            self.ensure_open()?;
            let Some(session) = sessions.get_mut(&id) else {
                return Ok(SessionReleaseReceipt {
                    session: id,
                    status: SessionReleaseStatus::NotOpen,
                });
            };
            if session.releasing {
                return Ok(SessionReleaseReceipt {
                    session: id,
                    status: SessionReleaseStatus::Retained(
                        crate::SessionRetentionReason::ReleaseInProgress,
                    ),
                });
            }
            if session.handle.actor_closed() {
                sessions.remove(&id);
                return Ok(SessionReleaseReceipt {
                    session: id,
                    status: SessionReleaseStatus::NotOpen,
                });
            }
            session.releasing = true;
            session.handle.clone()
        };
        let result = session.release_if_idle().await;
        let mut sessions = self.inner.sessions.lock().await;
        match &result {
            Ok(SessionReleaseStatus::Released) => {
                sessions.remove(&id);
            }
            _ => {
                if let Some(session) = sessions.get_mut(&id) {
                    session.releasing = false;
                }
            }
        }
        let status = result?;
        Ok(SessionReleaseReceipt {
            session: id,
            status,
        })
    }

    pub async fn list_sessions(&self, workspace: WorkspaceId) -> Result<Vec<SessionInfo>> {
        self.ensure_open()?;
        if self.inner.store.workspace_by_id(workspace)?.is_none() {
            return Err(Error::WorkspaceNotFound);
        }
        self.inner.store.sessions(workspace).map_err(Into::into)
    }

    /// Return the last Session explicitly selected in a workspace, if any.
    pub async fn last_active_session(&self, workspace: WorkspaceId) -> Result<Option<SessionId>> {
        self.ensure_open()?;
        if self.inner.store.workspace_by_id(workspace)?.is_none() {
            return Err(Error::WorkspaceNotFound);
        }
        self.inner
            .store
            .last_active_session(workspace)
            .map_err(Into::into)
    }

    /// Persist the Session a frontend selected for the next startup.
    pub async fn set_last_active_session(
        &self,
        workspace: WorkspaceId,
        session: SessionId,
    ) -> Result<()> {
        self.ensure_open()?;
        if self.inner.store.workspace_by_id(workspace)?.is_none() {
            return Err(Error::WorkspaceNotFound);
        }
        if !self
            .inner
            .store
            .set_last_active_session(workspace, session)?
        {
            return Err(Error::SessionNotFound);
        }
        Ok(())
    }

    /// Returns durable workspace navigation and attention data without opening
    /// Sessions or acquiring their exclusive writer leases.
    pub async fn workspace_overview(&self, workspace: WorkspaceId) -> Result<WorkspaceOverview> {
        self.ensure_open()?;
        let workspace = self
            .inner
            .store
            .workspace_by_id(workspace)?
            .ok_or(Error::WorkspaceNotFound)?;
        let store = self.inner.store.clone();
        tokio::task::spawn_blocking(move || {
            store.workspace_overview(workspace).map_err(Error::from)
        })
        .await
        .map_err(|error| Error::InvalidState(format!("workspace overview task failed: {error}")))?
    }

    /// Lists current workspace changes relative to Git HEAD. The returned
    /// changes describe the whole workspace and are not attributed to a task.
    pub async fn workspace_changes(
        &self,
        workspace: WorkspaceId,
        cursor: Option<WorkspaceChangeCursor>,
        limit: usize,
    ) -> Result<WorkspaceChangePage> {
        self.ensure_open()?;
        let root = self
            .inner
            .store
            .workspace_by_id(workspace)?
            .ok_or(Error::WorkspaceNotFound)?
            .root;
        tokio::task::spawn_blocking(move || crate::workspace_changes::changes(&root, cursor, limit))
            .await
            .map_err(|error| {
                Error::InvalidState(format!("workspace changes task failed: {error}"))
            })?
    }

    /// Reads a strictly limited diff or working-tree body for one file in the
    /// current change set. Paths are always workspace-relative.
    pub async fn workspace_file(
        &self,
        workspace: WorkspaceId,
        path: impl Into<String>,
        source: WorkspaceFileSource,
        max_bytes: usize,
    ) -> Result<WorkspaceFileView> {
        let page = self
            .workspace_file_page(workspace, path, source, None, max_bytes)
            .await?;
        Ok(WorkspaceFileView {
            baseline: page.baseline,
            path: page.path,
            source: page.source,
            media: page.media,
            text: page.text,
            bytes_read: page.bytes_read,
            total_bytes: page.total_bytes,
            truncated: page.next_cursor.is_some(),
        })
    }

    /// Reads one stable page of a changed file or diff. Continuations are
    /// rejected when the HEAD, path, source, or observed content has changed.
    pub async fn workspace_file_page(
        &self,
        workspace: WorkspaceId,
        path: impl Into<String>,
        source: WorkspaceFileSource,
        cursor: Option<WorkspaceFileCursor>,
        max_bytes: usize,
    ) -> Result<WorkspaceFilePage> {
        self.ensure_open()?;
        let root = self
            .inner
            .store
            .workspace_by_id(workspace)?
            .ok_or(Error::WorkspaceNotFound)?
            .root;
        let path = path.into();
        tokio::task::spawn_blocking(move || {
            crate::workspace_changes::file_page(&root, &path, source, cursor, max_bytes)
        })
        .await
        .map_err(|error| Error::InvalidState(format!("workspace file task failed: {error}")))?
    }

    pub async fn results(
        &self,
        session: SessionId,
        cursor: Option<HistoryCursor>,
        limit: usize,
    ) -> Result<ResultPage> {
        self.ensure_open()?;
        if cursor.is_some_and(|cursor| cursor.session() != session) {
            return Err(Error::InvalidState(
                "history cursor belongs to another session".into(),
            ));
        }
        if self.inner.store.session(session)?.is_none() {
            return Err(Error::SessionNotFound);
        }
        let store = self.inner.store.clone();
        tokio::task::spawn_blocking(move || {
            store
                .results(session, cursor, limit.clamp(1, 32))
                .map_err(Error::from)
        })
        .await
        .map_err(|error| Error::InvalidState(format!("result query task failed: {error}")))?
    }

    /// Returns one completed result as a durable product artifact. Evidence
    /// bodies remain separate so callers cannot accidentally fetch unbounded
    /// tool output while rendering a result list.
    pub async fn result_artifact(&self, result: ResultRef) -> Result<ResultArtifact> {
        self.ensure_open()?;
        let store = self.inner.store.clone();
        tokio::task::spawn_blocking(move || {
            store
                .result_artifact(result)?
                .ok_or_else(|| Error::InvalidState("result artifact not found".into()))
        })
        .await
        .map_err(|error| Error::InvalidState(format!("result artifact task failed: {error}")))?
    }

    /// Pages only the sources explicitly cited by this exact result version.
    pub async fn result_evidence(
        &self,
        result: ResultRef,
        cursor: Option<EvidenceCursor>,
        limit: usize,
    ) -> Result<EvidencePage> {
        self.ensure_open()?;
        if cursor.is_some_and(|cursor| cursor.result() != result) {
            return Err(Error::InvalidState(
                "evidence cursor belongs to another result".into(),
            ));
        }
        let store = self.inner.store.clone();
        tokio::task::spawn_blocking(move || {
            store
                .result_evidence(result, cursor, limit.clamp(1, 32))?
                .ok_or_else(|| Error::InvalidState("evidence result not found".into()))
        })
        .await
        .map_err(|error| Error::InvalidState(format!("result evidence task failed: {error}")))?
    }

    /// Reads a byte-bounded UTF-8 page from an explicitly cited public source.
    /// Private Core records are represented as `Private` without exposing
    /// their title or body; tool arguments are never part of this projection.
    pub async fn evidence_source(
        &self,
        result: ResultRef,
        source: EvidenceRef,
        offset: u64,
        max_bytes: usize,
    ) -> Result<EvidenceSourcePage> {
        self.ensure_open()?;
        if source.session != result.session {
            return Err(Error::InvalidState(
                "evidence source belongs to another session".into(),
            ));
        }
        let store = self.inner.store.clone();
        tokio::task::spawn_blocking(move || {
            let projection_pending = store.evidence_projection_pending(result.session)?;
            if !store.result_cites(result, source)? {
                return Err(Error::InvalidState(
                    "source is not cited by this result".into(),
                ));
            }
            let projected =
                store.evidence_source_page_record(source, offset, max_bytes.clamp(1, 64 * 1024))?;
            let Some(projected) = projected else {
                return Ok(EvidenceSourcePage {
                    source,
                    availability: EvidenceAvailability::Missing,
                    text: None,
                    offset: 0,
                    next_offset: None,
                    total_bytes: None,
                    projection_pending,
                });
            };
            match projected.metadata {
                crate::persistence::StoredEvidenceMetadata::Private => Ok(EvidenceSourcePage {
                    source,
                    availability: EvidenceAvailability::Private,
                    text: None,
                    offset: 0,
                    next_offset: None,
                    total_bytes: None,
                    projection_pending,
                }),
                crate::persistence::StoredEvidenceMetadata::Public {
                    kind,
                    title,
                    byte_len,
                    ..
                } => Ok(EvidenceSourcePage {
                    source,
                    availability: EvidenceAvailability::Available { kind, title },
                    text: projected.text,
                    offset: projected.offset,
                    next_offset: projected.next_offset,
                    total_bytes: Some(byte_len),
                    projection_pending,
                }),
            }
        })
        .await
        .map_err(|error| Error::InvalidState(format!("evidence source task failed: {error}")))?
    }

    pub async fn acceptances(
        &self,
        result: ResultRef,
        cursor: Option<AcceptanceCursor>,
        limit: usize,
    ) -> Result<AcceptancePage> {
        self.ensure_open()?;
        if cursor.is_some_and(|cursor| cursor.result() != result) {
            return Err(Error::InvalidState(
                "acceptance cursor belongs to another result".into(),
            ));
        }
        let store = self.inner.store.clone();
        tokio::task::spawn_blocking(move || {
            if store.result(result)?.is_none() {
                return Err(Error::InvalidState("acceptance result not found".into()));
            }
            store
                .acceptances(result, cursor, limit.clamp(1, 32))
                .map_err(Error::from)
        })
        .await
        .map_err(|error| Error::InvalidState(format!("acceptance query task failed: {error}")))?
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

    pub async fn project_config_status(
        &self,
        workspace: WorkspaceId,
    ) -> Result<crate::ProjectConfigStatus> {
        self.ensure_open()?;
        if self.inner.store.workspace_by_id(workspace)?.is_none() {
            return Err(Error::WorkspaceNotFound);
        }
        self.inner
            .store
            .project_config_status(workspace)
            .map_err(Into::into)
    }

    pub async fn reload_user_config(&self) -> Result<()> {
        let _update = self.inner.config_updates.lock().await;
        self.ensure_open()?;
        self.inner.store.reload_user_config()?;
        let targets = self.active_sessions(None).await;
        reload_sessions(&targets, false).await
    }

    pub async fn reload_workspace_config(
        &self,
        workspace: WorkspaceId,
    ) -> Result<crate::ProjectConfigStatus> {
        let _update = self.inner.config_updates.lock().await;
        self.ensure_open()?;
        let status = self.inner.store.reload_workspace_config(workspace)?;
        let targets = self.active_sessions(Some(workspace)).await;
        reload_sessions(&targets, false).await?;
        Ok(status)
    }

    /// Atomically reload user and project files, then notify open Sessions.
    /// Application results are available through each Session's observation.
    pub async fn reload_config(
        &self,
        workspace: WorkspaceId,
    ) -> Result<crate::ReloadConfigOutcome> {
        let _update = self.inner.config_updates.lock().await;
        self.ensure_open()?;
        let project = self.inner.store.reload_config(workspace)?;
        let targets = self.active_sessions(None).await;
        reload_sessions(&targets, false).await?;
        Ok(crate::ReloadConfigOutcome { project })
    }

    async fn active_sessions(&self, workspace: Option<WorkspaceId>) -> Vec<Session> {
        self.inner
            .sessions
            .lock()
            .await
            .values()
            .filter(|entry| !entry.releasing)
            .map(|entry| &entry.handle)
            .filter(|session| workspace.is_none_or(|id| session.workspace() == id))
            .cloned()
            .collect()
    }

    /// Persist one override and notify affected Sessions. Success means the
    /// selection is saved; observe each Session for its application result.
    pub async fn update_config(
        &self,
        scope: ConfigScope,
        change: ConfigChange,
    ) -> Result<RuntimeOverrides> {
        match &change {
            ConfigChange::Model(value)
            | ConfigChange::Worker(value)
            | ConfigChange::Coordinator(value) => {
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
                    validate_tool_limits(value)
                        .map_err(|error| Error::InvalidState(error.to_string()))?;
                }
            }
        }

        self.apply_config_update(scope, change).await
    }

    async fn apply_config_update(
        &self,
        scope: ConfigScope,
        change: ConfigChange,
    ) -> Result<RuntimeOverrides> {
        let _update = self.inner.config_updates.lock().await;
        self.ensure_open()?;
        self.ensure_scope(scope)?;
        if let ConfigChange::Model(Some(selection))
        | ConfigChange::Worker(Some(selection))
        | ConfigChange::Coordinator(Some(selection)) = &change
        {
            resolve_model(selection.clone(), &self.inner.store.profiles()?)
                .map_err(Error::Configuration)?;
        }
        let sessions = self.inner.sessions.lock().await;
        let force_model = matches!(&change, ConfigChange::Model(Some(_)));
        let saved = self
            .inner
            .store
            .update_config(scope, change)
            .map_err(Error::from)?;

        let mut forced = Vec::new();
        let mut ordinary = Vec::new();
        for session in sessions
            .values()
            .filter(|session| !session.releasing)
            .map(|session| &session.handle)
            .filter(|session| config_affects(scope, session))
        {
            if force_model && model_scope_applies(&self.inner.store, scope, session)? {
                forced.push(session.clone());
            } else {
                ordinary.push(session.clone());
            }
        }
        drop(sessions);
        drop(_update);
        let (forced_result, ordinary_result) = tokio::join!(
            reload_sessions(&forced, true),
            reload_sessions(&ordinary, false)
        );
        forced_result?;
        ordinary_result?;
        Ok(saved)
    }

    /// Return the durable desired configuration and the last configuration
    /// installed in this App instance's live Runtime.
    pub async fn resolved_config(&self, session: SessionId) -> Result<ResolvedConfig> {
        let _update = self.inner.config_updates.lock().await;
        let sessions = self.inner.sessions.lock().await;
        self.ensure_open()?;
        let desired = resolve_session_config(&self.inner.store, session)?;
        let live = sessions.get(&session).map(|session| session.handle.clone());
        let running = live.and_then(|session| {
            let view = session.observe();
            match &view.borrow().runtime {
                crate::RuntimeState::Running { config, .. } => Some(config.as_ref().clone()),
                _ => None,
            }
        });
        Ok(ResolvedConfig { desired, running })
    }

    /// Resolve the configuration inherited by a new Session in this workspace.
    /// No Session is opened and `running` is always `None`.
    pub async fn resolved_workspace_config(
        &self,
        workspace: WorkspaceId,
    ) -> Result<ResolvedConfig> {
        let _update = self.inner.config_updates.lock().await;
        self.ensure_open()?;
        let workspace = self
            .inner
            .store
            .workspace_by_id(workspace)?
            .ok_or(Error::WorkspaceNotFound)?;
        let global = self.inner.store.global_settings()?;
        let workspace_config = self
            .inner
            .store
            .config(ConfigScope::Workspace(workspace.id))?;
        let desired = resolve_runtime(
            &global,
            Some(&workspace_config),
            None,
            &self.inner.store.profiles()?,
            workspace.root,
        );
        Ok(ResolvedConfig {
            desired,
            running: None,
        })
    }

    pub async fn profiles(&self) -> Result<Vec<Profile>> {
        self.ensure_open()?;
        self.inner.store.profiles().map_err(Into::into)
    }

    /// Save a profile and notify open Sessions to apply its resolved value.
    pub async fn save_profile(&self, profile: Profile) -> Result<()> {
        profile
            .validate()
            .map_err(|error| Error::InvalidState(error.to_string()))?;
        self.apply_profile(profile).await
    }

    async fn apply_profile(&self, profile: Profile) -> Result<()> {
        let _update = self.inner.config_updates.lock().await;
        let sessions = self.inner.sessions.lock().await;
        self.ensure_open()?;
        self.inner.store.save_profile(profile)?;
        let targets = sessions
            .values()
            .filter(|session| !session.releasing)
            .map(|session| session.handle.clone())
            .collect::<Vec<_>>();
        drop(sessions);
        drop(_update);
        reload_sessions(&targets, false).await
    }

    /// Delete one saved connection and its stored credentials. Deleting an id
    /// that is not saved is a no-op, so a repeated delete never fails.
    ///
    /// Sessions that still select the removed connection keep their selection;
    /// they resolve to [`ConfigProblem::MissingProfile`] until it is changed.
    pub async fn delete_profile(&self, profile: ProfileId) -> Result<()> {
        let _update = self.inner.config_updates.lock().await;
        self.ensure_open()?;
        let profile = match self.profile(&profile) {
            Ok(profile) => profile,
            Err(Error::Configuration(ConfigProblem::MissingProfile(_))) => return Ok(()),
            Err(error) => return Err(error),
        };
        self.inner
            .providers
            .logout(&profile)
            .await
            .map_err(Error::from)?;
        self.inner.store.delete_profile(&profile.id)?;
        let sessions = self.inner.sessions.lock().await;
        let targets = sessions
            .values()
            .filter(|session| !session.releasing)
            .map(|session| session.handle.clone())
            .collect::<Vec<_>>();
        drop(sessions);
        drop(_update);
        reload_sessions(&targets, false).await
    }

    pub async fn set_api_key(&self, profile: ProfileId, key: ApiKey) -> Result<()> {
        self.ensure_open()?;
        let profile = self.profile(&profile)?;
        self.set_api_key_for_profile(profile, key).await
    }

    /// Provision one API-key slot without opening SQLite or a Session.
    /// Intended for installers and automation; the key should be read from stdin.
    pub async fn provision_api_key(
        bone_home: PathBuf,
        profile: Profile,
        key: ApiKey,
    ) -> Result<()> {
        let profile_id = profile.id.clone();
        tokio::task::spawn_blocking(move || {
            crate::credentials::ApiKeyCredentials::for_profile(&bone_home, &profile)
                .and_then(|credentials| credentials.save(&key))
        })
        .await
        .map_err(|_| {
            Error::Credential(crate::CredentialProblem {
                profile: profile_id.clone(),
                kind: crate::CredentialProblemKind::Unavailable,
            })
        })?
        .map_err(|error| {
            let kind = match error {
                crate::ApiKeyCredentialError::MissingApiKey => {
                    crate::CredentialProblemKind::Missing
                }
                crate::ApiKeyCredentialError::EndpointMismatch => {
                    crate::CredentialProblemKind::EndpointMismatch
                }
                crate::ApiKeyCredentialError::InvalidApiKey
                | crate::ApiKeyCredentialError::Unavailable => {
                    crate::CredentialProblemKind::Unavailable
                }
            };
            Error::Credential(crate::CredentialProblem {
                profile: profile_id,
                kind,
            })
        })
    }

    /// Save credentials only for the caller's observed profile and endpoint.
    /// A later profile edit cannot redirect this key: the provider uses the
    /// captured expected endpoint to choose its credential slot.
    pub async fn set_api_key_for_profile(&self, expected: Profile, key: ApiKey) -> Result<()> {
        let _update = self.inner.config_updates.lock().await;
        self.ensure_open()?;
        if self.profile(&expected.id)? != expected {
            return Err(Error::InvalidState(
                "profile changed before credentials were saved".into(),
            ));
        }
        self.inner
            .providers
            .set_api_key(&expected, key)
            .await
            .map_err(Error::from)?;

        self.reload_profile_sessions_locked(&expected.id)
            .await
            .map_err(|error| Error::CredentialsSaved {
                profile: expected.id,
                message: error.to_string(),
            })
    }

    async fn reload_profile_sessions(&self, profile: &ProfileId) -> Result<()> {
        let _update = self.inner.config_updates.lock().await;
        self.ensure_open()?;
        self.reload_profile_sessions_locked(profile).await
    }

    async fn reload_profile_sessions_locked(&self, profile: &ProfileId) -> Result<()> {
        let sessions = self.inner.sessions.lock().await;
        let targets = sessions
            .values()
            .filter(|session| !session.releasing)
            .map(|session| &session.handle)
            .filter(|session| {
                resolve_session_config(&self.inner.store, session.id()).is_ok_and(|config| {
                    config.is_ok_and(|config| {
                        config.worker.profile.id == *profile
                            || config.coordinator.profile.id == *profile
                    })
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        drop(sessions);
        reload_sessions(&targets, true).await
    }

    pub async fn login(&self, profile: ProfileId) -> Result<LoginAttempt> {
        self.ensure_open()?;
        let profile = self.profile(&profile)?;
        let providers = self.inner.providers.clone();
        let app = self.clone();
        let profile_id = profile.id.clone();
        let (state_tx, state) = watch::channel(Arc::new(LoginState::Connecting));
        let prompt_tx = state_tx.clone();
        let (cancel, cancellation) = oneshot::channel();
        tokio::spawn(async move {
            let login = async {
                providers
                    .login(&profile, move |prompt: DeviceCodePrompt| {
                        prompt_tx.send_replace(Arc::new(LoginState::DeviceCode {
                            verification_uri: prompt.verification_uri,
                            user_code: prompt.user_code,
                        }));
                    })
                    .await?;
                // Authentication succeeded even if an unrelated invalid model
                // prevents one Session from reloading. Each Session exposes
                // that configuration problem through its normal state.
                let _ = app.reload_profile_sessions(&profile_id).await;
                Ok::<(), ProviderConnectError>(())
            };
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
        sessions: &mut BTreeMap<SessionId, LiveSession>,
        saved: crate::SavedSession,
    ) -> Result<Session> {
        if let Some(session) = sessions.get(&saved.info.id) {
            if session.releasing {
                return Err(Error::SessionBusy(saved.info.id));
            }
            return Ok(session.handle.clone());
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
        sessions.insert(
            session.id(),
            LiveSession {
                handle: session.clone(),
                releasing: false,
            },
        );
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
        .map(|session| session.handle.clone())
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
            ProviderConnectError::LoginRequired(profile) => Self::LoginRequired(profile),
            ProviderConnectError::Credential(problem) => Self::Credential(problem),
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

pub(crate) fn title_from_first_input(input: &str) -> Option<String> {
    const MAX_CHARS: usize = 60;
    const MAX_BYTES: usize = 200;
    let normalized = input.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    if normalized.chars().count() <= MAX_CHARS && normalized.len() <= MAX_BYTES {
        return Some(normalized);
    }
    let mut title = String::new();
    for character in normalized.chars().take(MAX_CHARS) {
        if title.len() + character.len_utf8() + '…'.len_utf8() > MAX_BYTES {
            break;
        }
        title.push(character);
    }
    title.push('…');
    Some(title)
}

fn config_affects(scope: ConfigScope, session: &Session) -> bool {
    match scope {
        ConfigScope::User => true,
        ConfigScope::Workspace(workspace) => session.workspace() == workspace,
        ConfigScope::Session(id) => session.id() == id,
    }
}

fn model_scope_applies(store: &DataStore, scope: ConfigScope, session: &Session) -> Result<bool> {
    let session_config = store.config(ConfigScope::Session(session.id()))?;
    match scope {
        ConfigScope::Session(id) => Ok(session.id() == id),
        ConfigScope::Workspace(workspace) => Ok(session.workspace() == workspace
            && (session_config.worker.is_none() || session_config.coordinator.is_none())),
        ConfigScope::User => {
            let workspace_config = store.config(ConfigScope::Workspace(session.workspace()))?;
            Ok(
                (session_config.worker.is_none() && workspace_config.worker.is_none())
                    || (session_config.coordinator.is_none()
                        && workspace_config.coordinator.is_none()),
            )
        }
    }
}

fn resolve_session_config(
    store: &DataStore,
    session: SessionId,
) -> Result<std::result::Result<RuntimeConfig, ConfigProblem>> {
    let saved = store.session(session)?.ok_or(Error::SessionNotFound)?;
    let workspace = store
        .workspace_by_id(saved.info.workspace)?
        .ok_or(Error::WorkspaceNotFound)?;
    let global = store.global_settings()?;
    let workspace_config = store.config(ConfigScope::Workspace(workspace.id))?;
    let session_config = store.config(ConfigScope::Session(session))?;
    Ok(resolve_runtime(
        &global,
        Some(&workspace_config),
        Some(&session_config),
        &store.profiles()?,
        workspace.root,
    ))
}

async fn reload_sessions(sessions: &[Session], force: bool) -> Result<()> {
    for session in sessions {
        // A Session may close after the App snapshots its handles. The saved
        // configuration is still valid and will apply when it is next opened.
        match session.notify_config_changed(force).await {
            Ok(()) | Err(Error::Closed) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod expected_profile_tests {
    use super::*;

    #[tokio::test]
    async fn stale_profile_is_rejected_before_credential_work() {
        let root = tempfile::tempdir().unwrap();
        let app = App::open(AppOptions::isolated(root.path().join("data")))
            .await
            .unwrap();
        // Both profiles use subscription transport, which cannot reach API-key
        // storage even if this regression accidentally reaches the provider.
        let expected = Profile::chatgpt();
        app.save_profile(expected.clone()).await.unwrap();
        let mut current = expected.clone();
        current.label = "Changed label".into();
        app.save_profile(current.clone()).await.unwrap();
        let result = app
            .set_api_key_for_profile(expected, ApiKey::new("unused-test-value".into()).unwrap())
            .await;
        assert!(
            matches!(result, Err(Error::InvalidState(message)) if message == "profile changed before credentials were saved")
        );
        assert!(app.profiles().await.unwrap().contains(&current));
        app.shutdown().await.unwrap();
    }
}
