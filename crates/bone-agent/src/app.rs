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

/// Start a session from one fresh configuration snapshot.
///
/// Task settings, local tools, and paths are validated before connecting the
/// model service. Later configuration changes apply to later sessions.
/// The subscription connector permits one active session per credential root;
/// await the previous handle's shutdown before starting another with that root.
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
    let session = runtime
        .spawn_blocking(move || {
            PreparedSession::from_snapshot(&manager.snapshot()?, &workspace, &task)
        })
        .await??;
    let endpoint =
        chatgpt_subscription::connect("bone-agent", &session.credential_root, on_login).await?;
    session.spawn(endpoint)
}

struct PreparedSession {
    system: SystemConfig,
    solver: ModelSettings,
    environment: ToolEnvironment,
    credential_root: PathBuf,
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
        let credential_root = snapshot
            .get::<LlmConfig>()?
            .unwrap_or_default()
            .resolve_credential_root()?;
        Ok(Self {
            system,
            solver,
            environment,
            credential_root,
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

#[cfg(test)]
mod tests {
    use bone_config::ConfigSection;
    use bone_llm::testing;
    use rig_core::{
        providers::openai,
        test_utils::{MockHttpResponse, SequencedHttpClient},
    };
    use serde_json::{Value, json};

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

    fn scripted_endpoint() -> (Endpoint, SequencedHttpClient) {
        let responses =
            [
                WorkResult {
                    autonomy: Autonomy::Run,
                    operation: Some(Operation::Tool(ToolCall::new(
                        "read",
                        json!({"path": "note.txt"}),
                    ))),
                    ..Default::default()
                },
                WorkResult {
                    reply: Some("Read completed".into()),
                    next: Next::Finish,
                    ..Default::default()
                },
            ]
            .into_iter()
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
            });
        let transport = SequencedHttpClient::new(responses);
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

        let session = PreparedSession::from_snapshot(
            &manager.snapshot().unwrap(),
            directory.path(),
            &TaskConfig::default(),
        )
        .unwrap();

        assert_eq!(session.environment.limits(), &ToolLimits::default());
        assert_eq!(session.system.soft_deadline_seconds, 30);
        assert_eq!(session.system.shutdown_grace_seconds, 5);
        assert_eq!(
            session.credential_root,
            LlmConfig::default().resolve_credential_root().unwrap()
        );
    }

    async fn run_read(session: PreparedSession, expected_lines: u64) {
        let (endpoint, transport) = scripted_endpoint();
        let handle = session.spawn(endpoint).unwrap();
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
        let request: Value = serde_json::from_slice(&transport.requests()[0].body).unwrap();
        assert_eq!(request["model"], "solver");
        assert_eq!(request["reasoning"]["effort"], "high");
        assert!(handle.shutdown().await.unwrap().unresolved_jobs.is_empty());
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
        assert_eq!(first.credential_root, directory.path().join("credentials"));
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
