//! Concrete localhost Responses fixtures shared by independent contract tests.
#![allow(dead_code)]
use bone::config::{Config, ModelReference, Profile};
use rig_core::providers::{
    openai::{OpenAIConfig, Route},
    registry::{ProviderConfig, ProviderRef},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

mod server;
pub use server::Server;
pub fn python_command() -> Command {
    server::python_command()
}
pub fn read_requests(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

pub fn local_profile(port: u16) -> Profile {
    let mut native = OpenAIConfig::new("")
        .with_base_url(format!("http://127.0.0.1:{port}/v1"))
        .with_route(Route::Responses);
    native.dialect = rig_core::providers::openai::wire::LLAMACPP;
    native.auth = native.dialect.quirks.auth;
    Profile {
        model: ModelReference::Registry(
            ProviderRef::configured(ProviderConfig::OpenAi(native), "fixture").unwrap(),
        ),
        credential_env: Some(format!("BONE_TEST_EMPTY_{}", uuid::Uuid::new_v4().simple())),
        reuse_codex_login: false,
        additional_params: None,
        max_tokens: None,
    }
}

pub struct Fixture {
    _server: Server,
    pub root: tempfile::TempDir,
    pub data: PathBuf,
    pub workspace: PathBuf,
    pub requests: PathBuf,
    pub responses: PathBuf,
    pub profile: Profile,
}

impl Fixture {
    pub fn save_config(&self) {
        let config = Config {
            default_profile: "fixture".into(),
            profiles: BTreeMap::from([("fixture".into(), self.profile.clone())]),
        };
        std::fs::write(
            self.data.join("config.toml"),
            toml::to_string(&config).unwrap(),
        )
        .unwrap();
    }

    pub fn requests(&self) -> Vec<Value> {
        read_requests(&self.requests)
    }

    pub fn turns(turns: Value) -> Self {
        let mut fixture = Self::script(json!({"turns": turns}), "tests/scripted_responses.py");
        fixture.profile.additional_params = Some(json!({"reasoning":{"effort":"low"}}));
        fixture.save_config();
        fixture
    }

    pub fn script(script_body: Value, server_path: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let script = root.path().join("responses.json");
        let requests = root.path().join("requests.jsonl");
        std::fs::write(&script, serde_json::to_vec(&script_body).unwrap()).unwrap();
        let (server, port) = Server::script(server_path, &script, &requests);
        let native = OpenAIConfig::new("")
            .with_base_url(format!("http://127.0.0.1:{port}/v1"))
            .with_route(Route::Responses);
        let profile = Profile {
            model: ModelReference::Registry(
                ProviderRef::configured(ProviderConfig::OpenAi(native), "fixture").unwrap(),
            ),
            credential_env: Some("BONE_TEST_DUMMY_KEY".into()),
            reuse_codex_login: false,
            additional_params: None,
            max_tokens: None,
        };
        let config = Config {
            default_profile: "fixture".into(),
            profiles: BTreeMap::from([("fixture".into(), profile.clone())]),
        };
        std::fs::write(data.join("config.toml"), toml::to_string(&config).unwrap()).unwrap();
        Self {
            root,
            data,
            workspace,
            requests,
            responses: script,
            profile,
            _server: server,
        }
    }

    pub fn command(&self) -> Command {
        self.command_at(&self.data)
    }

    pub fn command_at(&self, data: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_bone"));
        command
            .env("BONE_TEST_DUMMY_KEY", "fixture-dummy")
            .env_remove("OPENAI_API_KEY")
            .env_remove("CHATGPT_ACCESS_TOKEN")
            .env_remove("BONE_MODEL")
            .arg("--data-dir")
            .arg(data);
        command
    }

    pub fn run(&self, prompt: &str, session: Option<&str>, extra: &[&str]) -> (Output, Value) {
        let mut command = self.command();
        command
            .arg("run")
            .arg(prompt)
            .arg("--workspace")
            .arg(&self.workspace)
            .arg("--profile")
            .arg("fixture")
            .arg("--json");
        if let Some(id) = session {
            command.arg("--session").arg(id);
        }
        command.args(extra);
        let output = bounded_output(command);
        let document = serde_json::from_slice(&output.stdout).expect("CLI JSON document");
        (output, document)
    }

    pub fn history(&self, session: &str) -> Vec<Value> {
        let mut command = self.command();
        command.arg("history").arg(session).arg("--json");
        let output = bounded_output(command);
        assert!(output.status.success(), "history failed");
        serde_json::from_slice(&output.stdout).expect("history event array")
    }

    pub fn request_count(&self) -> usize {
        std::fs::read_to_string(&self.requests)
            .unwrap_or_default()
            .lines()
            .count()
    }

    pub fn local_profile(&self) -> Profile {
        let mut profile = self.profile.clone();
        let ModelReference::Registry(reference) = &profile.model else {
            panic!("native registry fixture");
        };
        let ProviderConfig::OpenAi(mut native) = reference.config("") else {
            panic!("OpenAI wire fixture");
        };
        // Optional local auth lets direct Engine tests avoid mutating process
        // environment while other acceptance tests execute concurrently.
        native.dialect = rig_core::providers::openai::wire::LLAMACPP;
        native.auth = native.dialect.quirks.auth;
        profile.credential_env = Some(format!("BONE_TEST_EMPTY_{}", uuid::Uuid::new_v4().simple()));
        profile.model = ModelReference::Registry(
            ProviderRef::configured(ProviderConfig::OpenAi(native), "fixture").unwrap(),
        );
        profile
    }

    pub fn engine(
        &self,
        session: Option<&str>,
        options: bone::runtime::RunOptions,
    ) -> bone::runtime::Engine {
        let profile = self.local_profile();
        bone::runtime::Engine::open(
            &self.data,
            &self.workspace,
            session,
            profile,
            "fixture".into(),
            options,
        )
        .unwrap()
    }
}

pub fn bounded_output(command: Command) -> Output {
    output_with_input(command, None, Duration::from_secs(40))
}

pub fn output_with_input(mut command: Command, input: Option<&str>, timeout: Duration) -> Output {
    if input.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = input {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    finish_child(child, timeout)
}

pub fn finish_child(mut child: Child, timeout: Duration) -> Output {
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("fixture CLI exceeded {timeout:?}");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

pub async fn drive_until(
    engine: &mut bone::runtime::Engine,
    timeout: Duration,
    mut predicate: impl FnMut(&bone::runtime::Engine) -> bool,
) {
    tokio::time::timeout(timeout, async {
        while !predicate(engine) {
            engine.step().await.unwrap();
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("scripted runtime did not reach the expected boundary");
}
