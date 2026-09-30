//! Local native Rig protocol contracts. These fixtures use only synthetic credentials.
use std::{
    io::{BufRead, BufReader},
    path::Path,
    process::{Child, Command, Stdio},
};

use bone::{
    config::{ModelReference, Profile},
    model,
};
use futures_util::StreamExt;
use rig_core::{
    completion::{CompletionRequest, ToolDefinition},
    providers::{
        chatgpt,
        openai::OpenAIConfig,
        registry::{ProviderConfig, ProviderId, ProviderRef},
    },
};
use serde_json::{Value, json};

struct Fixture {
    process: Child,
    directory: tempfile::TempDir,
    profile: Profile,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

fn fixture(turns: Value) -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let script = directory.path().join("script.json");
    std::fs::write(
        &script,
        serde_json::to_vec(&json!({"turns": turns})).unwrap(),
    )
    .unwrap();
    let mut process = Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/scripted_responses.py"))
        .arg("--script")
        .arg(&script)
        .arg("--requests")
        .arg(directory.path().join("requests.jsonl"))
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(process.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let port: u16 = line.trim().parse().expect("local fixture prints its port");
    let native = OpenAIConfig::with_key(&chatgpt::DIALECT, "")
        .with_base_url(format!("http://127.0.0.1:{port}/v1"));
    let profile = Profile {
        model: ModelReference::Registry(
            ProviderRef::configured(ProviderConfig::OpenAi(native), "fixture").unwrap(),
        ),
        credential_env: None,
        reuse_codex_login: false,
        additional_params: Some(json!({"reasoning":{"effort":"low"}})),
        max_tokens: None,
    };
    let auth = model::auth_file(directory.path(), "fixture").unwrap();
    std::fs::create_dir_all(auth.parent().unwrap()).unwrap();
    std::fs::write(auth, r#"{"access_token":"bone-synthetic-token","expires_at":4102444800,"account_id":"bone-test-account"}"#).unwrap();
    Fixture {
        process,
        directory,
        profile,
    }
}

#[tokio::test]
async fn subscription_native_call_and_stream_preserve_text_tools_usage_and_parameters() {
    let fixture = fixture(json!([
        {"contains":["fixture_prompt","fixture_tool","reasoning"],"output":[{"type":"function_call","call_id":"fixture_call","name":"fixture_tool","arguments":"{\"value\":7}"}]},
        {"contains":["stream_prompt"],"text":"native stream answer"}
    ]));
    let connection = model::connect(
        &fixture.profile,
        fixture.directory.path(),
        "fixture",
        "job-native",
        "call-native",
    )
    .await
    .unwrap();
    let request = fixture.profile.apply(CompletionRequest::new("fixture_prompt").tools(vec![ToolDefinition { name: "fixture_tool".into(), description: "fixture tool".into(), parameters: json!({"type":"object","properties":{"value":{"type":"integer"}},"required":["value"]}) }]));
    let response = connection.model.call(request).await.unwrap();
    let tools: Vec<_> = response.tool_calls().collect();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].function.arguments, json!({"value":7}));
    assert!(response.message().is_some());
    assert!(response.usage.is_reported());
    drop(connection);
    let connection = model::connect(
        &fixture.profile,
        fixture.directory.path(),
        "fixture",
        "job-native",
        "call-stream",
    )
    .await
    .unwrap();
    let mut stream = connection
        .model
        .stream(CompletionRequest::new("stream_prompt"))
        .unwrap();
    let mut events = 0;
    while let Some(item) = stream.next().await {
        item.unwrap();
        events += 1;
    }
    let response = stream.finish().await.unwrap();
    assert!(events > 0);
    assert_eq!(response.text(), "native stream answer");
    assert!(response.usage.is_reported());
    let requests =
        std::fs::read_to_string(fixture.directory.path().join("requests.jsonl")).unwrap();
    let first: Value = serde_json::from_str(requests.lines().next().unwrap()).unwrap();
    assert_eq!(first["body"]["reasoning"]["effort"], "low");
    assert_eq!(requests.lines().count(), 2);
    assert!(!requests.contains("bone-synthetic-token"));
}

#[tokio::test]
async fn unauthorized_native_call_exits_once_with_explicit_relogin_instruction() {
    let fixture = fixture(json!([{ "http_status":401 }]));
    let connection = model::connect(
        &fixture.profile,
        fixture.directory.path(),
        "fixture",
        "job",
        "call",
    )
    .await
    .unwrap();
    let error = connection
        .model
        .call(CompletionRequest::new("fixture"))
        .await
        .unwrap_err();
    assert_eq!(error.report().http_status, Some(401));
    let error = model::call_error("fixture", error).to_string();
    assert!(error.contains("bone login --profile fixture"));
    let requests =
        std::fs::read_to_string(fixture.directory.path().join("requests.jsonl")).unwrap();
    assert_eq!(requests.lines().count(), 1);
}

#[test]
fn every_registered_provider_constructs_a_native_erased_completion_model() {
    let http = rig_core::http_client::DynHttpClient::new(rig_reqwest::shared());
    let mut count = 0;
    for id in ProviderId::all() {
        let reference = ProviderRef::registered(id, "fixture-model").unwrap();
        let native = reference.completion_model_with("synthetic-provider-token", http.clone());
        assert_eq!(native.name(), id.vendor());
        assert_eq!(native.id(), Some("fixture-model"));
        assert!(model::providers().contains(&id.to_string()));
        count += 1;
    }
    assert!(count > 20);
    for companion in ["cohere", "ollama"] {
        assert!(model::providers().contains(&companion.into()));
    }
}
