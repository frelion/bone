use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use bone_config::{ConfigError, ConfigManager, ConfigManagerBuilder, ConfigSnapshot};
use bone_llm::{Endpoint, LlmConfig, service::chatgpt_subscription};
use bone_tools::{ToolEnvironment, ToolError, ToolLimits};

use crate::{
    AgentHandle, KernelConfig, LoginPrompt, ModelAdapter, ModelSettings, Runtime, RuntimeConfig,
    RuntimeError, SystemConfig, TaskConfig, read_only_tools,
};

/// Register the settings needed by every agent session. Frontends may register
/// their own sections on the returned builder before choosing a storage path.
pub fn config_builder() -> Result<ConfigManagerBuilder, ConfigError> {
    ConfigManager::builder()
        .register::<SystemConfig>()?
        .register::<LlmConfig>()?
        .register::<ToolLimits>()
}

/// Failures while validating, connecting, or starting a configured session.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)]
    Configuration(#[from] ConfigError),
    #[error("invalid task settings: {0}")]
    Task(String),
    #[error(transparent)]
    Tools(#[from] ToolError),
    #[error(transparent)]
    Connection(#[from] chatgpt_subscription::Error),
    #[error(transparent)]
    Model(#[from] bone_llm::ConfigError),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error("session preparation failed: {0}")]
    Preparation(#[from] tokio::task::JoinError),
}

/// A connected application host that can start independent Agent sessions.
///
/// The model connection is shared, while every session reads fresh Agent,
/// tool, and runtime settings and owns its own tools, Kernel, and Runtime.
#[derive(Clone)]
pub struct AgentHost {
    manager: ConfigManager,
    endpoint: Endpoint,
}

impl AgentHost {
    pub async fn start(
        &self,
        workspace: impl AsRef<Path>,
        task: TaskConfig,
    ) -> Result<AgentHandle, StartError> {
        let session = prepare(&self.manager, workspace.as_ref().to_path_buf(), task).await?;
        session.spawn(self.endpoint.clone())
    }
}

/// Connect once, then use the returned host to start independent sessions.
pub async fn connect(
    manager: &ConfigManager,
    on_login: impl Fn(LoginPrompt) + Send + Sync + 'static,
) -> Result<AgentHost, StartError> {
    let runtime =
        tokio::runtime::Handle::try_current().map_err(|_| RuntimeError::NoTokioRuntime)?;
    let owned_manager = manager.clone();
    let credential_root = runtime
        .spawn_blocking(move || -> Result<_, StartError> {
            let snapshot = owned_manager.snapshot()?;
            SystemConfig::from_snapshot(&snapshot)?;
            credential_root(&snapshot)
        })
        .await??;
    let endpoint = chatgpt_subscription::connect("bone-agent", credential_root, on_login).await?;
    Ok(AgentHost {
        manager: manager.clone(),
        endpoint,
    })
}

/// Start a session from one fresh configuration snapshot.
///
/// Task settings, local tools, and paths are validated before connecting the
/// model service. Later configuration changes apply to later sessions.
/// Use [`connect`] when several sessions should share one model connection.
pub async fn start(
    manager: &ConfigManager,
    workspace: impl AsRef<Path>,
    task: TaskConfig,
    on_login: impl Fn(LoginPrompt) + Send + Sync + 'static,
) -> Result<AgentHandle, StartError> {
    let runtime =
        tokio::runtime::Handle::try_current().map_err(|_| RuntimeError::NoTokioRuntime)?;
    let manager = manager.clone();
    let workspace = workspace.as_ref().to_path_buf();
    let (session, credential_root) = runtime
        .spawn_blocking(move || {
            let snapshot = manager.snapshot()?;
            let session = PreparedSession::from_snapshot(&snapshot, &workspace, &task)?;
            Ok::<_, StartError>((session, credential_root(&snapshot)?))
        })
        .await??;
    let endpoint = chatgpt_subscription::connect("bone-agent", credential_root, on_login).await?;
    session.spawn(endpoint)
}

async fn prepare(
    manager: &ConfigManager,
    workspace: PathBuf,
    task: TaskConfig,
) -> Result<PreparedSession, StartError> {
    let runtime =
        tokio::runtime::Handle::try_current().map_err(|_| RuntimeError::NoTokioRuntime)?;
    let manager = manager.clone();
    runtime
        .spawn_blocking(move || {
            PreparedSession::from_snapshot(&manager.snapshot()?, &workspace, &task)
        })
        .await?
}

struct PreparedSession {
    system: SystemConfig,
    solver: ModelSettings,
    environment: ToolEnvironment,
}

impl PreparedSession {
    fn from_snapshot(
        snapshot: &ConfigSnapshot,
        workspace: &Path,
        task: &TaskConfig,
    ) -> Result<Self, StartError> {
        let system = SystemConfig::from_snapshot(snapshot)?;
        let solver = system.solver_for(task).map_err(StartError::Task)?;
        let limits = snapshot.get::<ToolLimits>()?.unwrap_or_default();
        let environment = ToolEnvironment::with_limits(workspace, limits)?;
        Ok(Self {
            system,
            solver,
            environment,
        })
    }

    fn spawn(self, endpoint: Endpoint) -> Result<AgentHandle, StartError> {
        let model = ModelAdapter::new(
            endpoint.model(&self.system.coordinator.model)?,
            endpoint.model(&self.solver.model)?,
        )
        .with_efforts(self.system.coordinator.effort, self.solver.effort);
        Ok(Runtime::spawn(
            Arc::new(model),
            read_only_tools(&self.environment),
            KernelConfig {
                soft_deadline: Duration::from_secs(u64::from(self.system.soft_deadline_seconds)),
                review_timeout: self.system.coordinator.timeout(),
                work_timeout: self.solver.timeout(),
            },
            RuntimeConfig {
                shutdown_grace_period: Duration::from_secs(u64::from(
                    self.system.shutdown_grace_seconds,
                )),
            },
        )?)
    }
}

fn credential_root(snapshot: &ConfigSnapshot) -> Result<PathBuf, StartError> {
    Ok(snapshot
        .get::<LlmConfig>()?
        .unwrap_or_default()
        .resolve_credential_root()?)
}

#[cfg(test)]
mod tests {
    use std::future::Future;

    use bone_config::ConfigSection;
    use bone_llm::testing;
    use bytes::Bytes;
    use rig_core::{
        http_client::{
            self, HttpClientExt, LazyBody, MultipartForm, Request, Response, StreamingResponse,
        },
        providers::openai,
        test_utils::{MockHttpResponse, SequencedHttpClient},
        wasm_compat::WasmCompatSend,
    };
    use serde_json::{Value, json};
    use tokio::sync::Barrier;

    use super::*;
    use crate::{
        Autonomy, EffectSummary, JobOutput, JobRequest, JobState, Next, Notice, Operation,
        ToolCall, WorkResult,
    };

    fn configured_store(directory: &Path) -> ConfigManager {
        let path = directory.join("config.json");
        std::fs::write(
            &path,
            json!({
                "agent.system": {
                    "coordinator": {"model": "reviewer", "timeout_seconds": 11},
                    "default_solver": {"model": "solver", "effort": "high", "timeout_seconds": 17},
                    "soft_deadline_seconds": 3,
                    "shutdown_grace_seconds": 2
                },
                "llm.system": {"credential_root": directory.join("credentials")},
                "tools.local": {"max_read_lines": 1},
                "another.frontend": {"theme": "dark"}
            })
            .to_string(),
        )
        .unwrap();
        config_builder().unwrap().build(path).unwrap()
    }

    fn read_responses(sessions: usize) -> Vec<MockHttpResponse> {
        (0..sessions)
            .map(|_| WorkResult {
                autonomy: Autonomy::Run,
                operation: Some(Operation::Tool(ToolCall::new(
                    "read",
                    json!({"path": "note.txt"}),
                ))),
                ..Default::default()
            })
            .chain((0..sessions).map(|_| WorkResult {
                reply: Some("Read completed".into()),
                next: Next::Finish,
                ..Default::default()
            }))
            .enumerate()
            .map(|(index, work)| {
                MockHttpResponse::success(json!({
                "id": format!("resp_{index}"), "object": "response", "created_at": 0,
                "status": "completed", "model": "solver", "tools": [],
                "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2},
                "output": [{
                    "type": "function_call", "id": format!("fc_{index}"),
                    "call_id": format!("call_{index}"), "name": "submit_work",
                    "arguments": serde_json::to_string(&work).unwrap(), "status": "completed"
                }]
            }).to_string())
            })
            .collect()
    }

    fn scripted_endpoint() -> (Endpoint, SequencedHttpClient) {
        let transport = SequencedHttpClient::new(read_responses(1));
        let client = openai::Client::builder()
            .api_key("test-only-key")
            .http_client(transport.clone())
            .build()
            .unwrap();
        (
            testing::openai_responses_endpoint("application-test", client).unwrap(),
            transport,
        )
    }

    #[derive(Clone, Debug)]
    struct ConcurrentHttpClient {
        inner: SequencedHttpClient,
        requests_ready: Arc<Barrier>,
    }

    impl ConcurrentHttpClient {
        fn new(responses: impl IntoIterator<Item = MockHttpResponse>) -> Self {
            Self {
                inner: SequencedHttpClient::new(responses),
                requests_ready: Arc::new(Barrier::new(2)),
            }
        }

        fn requests(&self) -> usize {
            self.inner.requests().len()
        }
    }

    impl Default for ConcurrentHttpClient {
        fn default() -> Self {
            Self {
                inner: SequencedHttpClient::default(),
                requests_ready: Arc::new(Barrier::new(1)),
            }
        }
    }

    impl HttpClientExt for ConcurrentHttpClient {
        fn send<T, U>(
            &self,
            request: Request<T>,
        ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
        where
            T: Into<Bytes> + WasmCompatSend,
            U: From<Bytes> + WasmCompatSend + 'static,
        {
            let response = self.inner.send(request);
            let requests_ready = Arc::clone(&self.requests_ready);
            async move {
                requests_ready.wait().await;
                response.await
            }
        }

        fn send_multipart<U>(
            &self,
            request: Request<MultipartForm>,
        ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
        where
            U: From<Bytes> + WasmCompatSend + 'static,
        {
            self.inner.send_multipart(request)
        }

        fn send_streaming<T>(
            &self,
            request: Request<T>,
        ) -> impl Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
        where
            T: Into<Bytes> + WasmCompatSend,
        {
            self.inner.send_streaming(request)
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_agent_only_configuration_uses_llm_tool_and_runtime_defaults() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        std::fs::write(
            &path,
            json!({
                "agent.system": {
                    "coordinator": {"model": "reviewer"},
                    "default_solver": {"model": "solver"}
                }
            })
            .to_string(),
        )
        .unwrap();
        let manager = config_builder().unwrap().build(path).unwrap();

        let snapshot = manager.snapshot().unwrap();
        let session =
            PreparedSession::from_snapshot(&snapshot, directory.path(), &TaskConfig::default())
                .unwrap();

        assert_eq!(session.environment.limits(), &ToolLimits::default());
        assert_eq!(session.system.soft_deadline_seconds, 30);
        assert_eq!(session.system.shutdown_grace_seconds, 5);
        assert_eq!(
            credential_root(&snapshot).unwrap(),
            LlmConfig::default().resolve_credential_root().unwrap()
        );
    }

    async fn read_with(handle: AgentHandle, expected_lines: u64) {
        let mut observation = handle.observe().await.unwrap();
        handle.post("Read note.txt").await.unwrap();
        let mut effects = Vec::new();
        loop {
            let step = tokio::time::timeout(Duration::from_secs(5), observation.events.recv())
                .await
                .unwrap()
                .unwrap();
            effects.extend(step.effects.clone());
            if step
                .effects
                .iter()
                .any(|effect| matches!(effect, EffectSummary::Publish(Notice::Finished { .. })))
            {
                break;
            }
        }
        let snapshot = handle.snapshot().await.unwrap();
        let artifact = snapshot
            .jobs
            .iter()
            .find_map(|job| match &job.state {
                JobState::Finished(outcome) => match &outcome.result {
                    Ok(JobOutput::Artifact(value)) => Some(value),
                    _ => None,
                },
                _ => None,
            })
            .unwrap();
        assert_eq!(artifact["end_line"], expected_lines);
        assert_eq!(artifact["truncated"], expected_lines < 2);
        assert!(effects.iter().any(|effect| matches!(
            effect,
            EffectSummary::Start { request: JobRequest::Work { .. }, timeout: Some(timeout), .. }
                if *timeout == Duration::from_secs(17)
        )));
        assert!(effects.iter().any(|effect| matches!(
            effect,
            EffectSummary::WakeAfter { delay, .. } if *delay == Duration::from_secs(3)
        )));
        assert!(handle.shutdown().await.unwrap().unresolved_jobs.is_empty());
    }

    async fn run_read(session: PreparedSession, expected_lines: u64) {
        let (endpoint, transport) = scripted_endpoint();
        read_with(session.spawn(endpoint).unwrap(), expected_lines).await;
        let request: Value = serde_json::from_slice(&transport.requests()[0].body).unwrap();
        assert_eq!(request["model"], "solver");
        assert_eq!(request["reasoning"]["effort"], "high");
    }

    #[tokio::test]
    async fn shared_configuration_drives_tools_and_deadlines_and_existing_sessions_keep_their_limits()
     {
        let directory = tempfile::tempdir().unwrap();
        let manager = configured_store(directory.path());
        std::fs::write(directory.path().join("note.txt"), "one\ntwo\n").unwrap();
        let snapshot = manager.snapshot().unwrap();
        assert_eq!(
            snapshot.unrecognized_sections().collect::<Vec<_>>(),
            ["another.frontend"]
        );
        let first =
            PreparedSession::from_snapshot(&snapshot, directory.path(), &TaskConfig::default())
                .unwrap();
        assert_eq!(
            credential_root(&snapshot).unwrap(),
            directory.path().join("credentials")
        );
        manager
            .set_value(
                ToolLimits::KEY,
                json!({"max_read_lines": 2}),
                snapshot.revision(),
            )
            .unwrap();
        let second = PreparedSession::from_snapshot(
            &manager.snapshot().unwrap(),
            directory.path(),
            &TaskConfig::default(),
        )
        .unwrap();
        run_read(first, 1).await;
        run_read(second, 2).await;
        assert_eq!(
            manager.snapshot().unwrap().value("another.frontend"),
            Some(&json!({"theme": "dark"}))
        );
        assert!(!directory.path().join("credentials").exists());
    }

    #[tokio::test]
    async fn host_reuses_its_endpoint_and_reads_fresh_configuration_for_each_session() {
        let directory = tempfile::tempdir().unwrap();
        let manager = configured_store(directory.path());
        std::fs::write(directory.path().join("note.txt"), "one\ntwo\n").unwrap();
        let transport = ConcurrentHttpClient::new(read_responses(2));
        let client = openai::Client::builder()
            .api_key("test-only-key")
            .http_client(transport.clone())
            .build()
            .unwrap();
        let endpoint = testing::openai_responses_endpoint("application-test", client).unwrap();
        let host = AgentHost {
            manager: manager.clone(),
            endpoint,
        };

        let first = host
            .start(directory.path(), TaskConfig::default())
            .await
            .unwrap();
        let snapshot = manager.snapshot().unwrap();
        manager
            .set_value(
                ToolLimits::KEY,
                json!({"max_read_lines": 2}),
                snapshot.revision(),
            )
            .unwrap();
        let second = host
            .start(directory.path(), TaskConfig::default())
            .await
            .unwrap();

        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(read_with(first, 1), read_with(second, 2));
        })
        .await
        .expect("both sessions must reach the shared transport concurrently");
        assert_eq!(transport.requests(), 4);
    }

    #[tokio::test]
    async fn connecting_a_host_requires_valid_agent_settings_before_authorization() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        std::fs::write(
            &path,
            json!({
                "llm.system": {"credential_root": directory.path().join("credentials")}
            })
            .to_string(),
        )
        .unwrap();
        let manager = config_builder().unwrap().build(path).unwrap();

        assert!(matches!(
            connect(&manager, |_: LoginPrompt| panic!("invalid host must not request login")).await,
            Err(StartError::Configuration(ConfigError::InvalidSection { section, .. }))
                if section == SystemConfig::KEY
        ));
        assert!(!directory.path().join("credentials").exists());
    }

    #[tokio::test]
    async fn invalid_tasks_workspaces_and_configuration_fail_before_connecting() {
        let directory = tempfile::tempdir().unwrap();
        let manager = configured_store(directory.path());
        // Any accidental connection fails locally instead of contacting a service.
        // Its Connection error would fail the more specific assertions below.
        std::fs::write(directory.path().join("credentials"), "not a directory").unwrap();
        let on_login = |_: LoginPrompt| panic!("invalid session must not request login");
        assert!(matches!(
            start(
                &manager,
                directory.path(),
                TaskConfig {
                    model: Some(" ".into()),
                    ..Default::default()
                },
                on_login
            )
            .await,
            Err(StartError::Task(_))
        ));
        assert!(matches!(
            start(
                &manager,
                directory.path().join("missing"),
                TaskConfig::default(),
                on_login
            )
            .await,
            Err(StartError::Tools(_))
        ));
        assert!(matches!(
            start(&manager, directory.path(), TaskConfig::default(), on_login).await,
            Err(StartError::Connection(
                chatgpt_subscription::Error::CredentialStoreUnavailable
            ))
        ));
        let path = directory.path().join("config.json");
        let valid: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for (section, field, value) in [
            ("tools.local", "max_read_lines", json!(0)),
            ("agent.system", "soft_deadline_seconds", json!(0)),
            ("llm.system", "credential_root", json!("relative")),
        ] {
            let mut invalid = valid.clone();
            invalid[section][field] = value;
            std::fs::write(&path, invalid.to_string()).unwrap();
            assert!(matches!(
                start(&manager, directory.path(), TaskConfig::default(), on_login).await,
                Err(StartError::Configuration(
                    ConfigError::InvalidSection { .. }
                ))
            ));
        }
    }
}
