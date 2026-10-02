//! CLI contracts against local fixtures; synthetic credentials and temporary state.
use std::process::Command;

use serde_json::Value;
mod support;

fn passive_command(root: &std::path::Path, data: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bone"));
    command
        .current_dir(root)
        .env("HOME", root)
        .env("CODEX_HOME", root.join("nonexistent-codex-home"))
        .env("BONE_MODEL", "deliberately-invalid-provider:model")
        .env_remove("OPENAI_API_KEY")
        .env_remove("CHATGPT_ACCESS_TOKEN")
        .arg("--data-dir")
        .arg(data)
        .args(["--profile", "missing-profile", "tools"]);
    command
}

#[test]
fn tools_json_matches_native_runtime_definitions_without_configuration_or_session() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    std::fs::create_dir(&data).unwrap();
    let invalid_config = "deliberately invalid TOML [ ]";
    std::fs::write(data.join("config.toml"), invalid_config).unwrap();
    for single_job in [false, true] {
        for read_only in [false, true] {
            let mut command = passive_command(root.path(), &data);
            command.arg("--json");
            if single_job {
                command.arg("--single-job");
            }
            if read_only {
                command.arg("--read-only");
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.stderr.is_empty());
            let actual: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(
                actual,
                serde_json::to_value(bone::tool_definitions(single_job, read_only)).unwrap()
            );
            let definitions = actual.as_array().unwrap();
            let has = |name: &str| {
                definitions
                    .iter()
                    .any(|definition| definition["name"] == name)
            };
            assert!(has("search_files"));
            assert!(has("read_file"));
            assert!(has("ask_user"));
            assert_eq!(has("edit_file"), !read_only);
            assert_eq!(has("write_file"), !read_only);
            assert_eq!(has("shell"), !read_only);
            for name in [
                "job_send",
                "job_wait",
                "job_handoff",
                "job_close",
                "job_control",
            ] {
                assert_eq!(has(name), !single_job, "{name}");
            }
        }
    }
    assert_eq!(
        std::fs::read_to_string(data.join("config.toml")).unwrap(),
        invalid_config
    );
    assert_eq!(std::fs::read_dir(&data).unwrap().count(), 1);
    assert!(!root.path().join(".bone").exists());
    assert!(!root.path().join("nonexistent-codex-home").exists());
}

#[test]
fn tools_text_describes_each_native_definition_even_with_unusable_data_path() {
    let root = tempfile::tempdir().unwrap();
    let blocked = root.path().join("not-a-directory");
    std::fs::write(&blocked, "UNCHANGED").unwrap();
    let output = passive_command(root.path(), &blocked).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    for definition in bone::tool_definitions(false, false) {
        assert!(text.contains(&format!("{}\t{}", definition.name, definition.description)));
    }
    assert!(text.contains("search_files"));
    assert!(text.contains("edit_file"));
    assert_eq!(std::fs::read_to_string(blocked).unwrap(), "UNCHANGED");
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn ordinary_read_only_run_executes_source_search_inside_a_persisted_job() {
    use serde_json::json;
    use sha2::{Digest, Sha256};
    let source = format!(
        "fn example() {{}}\n// FLAG_VISIBLE\n{}",
        "// unchanged tail\n".repeat(1000)
    );
    let hash = format!("{:x}", Sha256::digest(source.as_bytes()));
    let fixture = support::Fixture::turns(json!([
        {"output":[{"type":"function_call","name":"search_files","call_id":"readonly-search","arguments":{"query":"FLAG_VISIBLE","path":"src","limit":2}}]},
        {"contains":["readonly-search","src/example.rs","FLAG_VISIBLE",hash],"text":"READ_ONLY_ENGINE_SEARCH_OK"}
    ]));
    std::fs::create_dir(fixture.workspace.join("src")).unwrap();
    std::fs::write(fixture.workspace.join("src/example.rs"), &source).unwrap();
    let (output, result) = fixture.run(
        "Find FLAG_VISIBLE without modifying files.",
        None,
        &[
            "--read-only",
            "--single-job",
            "--max-calls",
            "2",
            "--timeout-seconds",
            "10",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(result["status"], "completed");
    assert_eq!(result["text"], "READ_ONLY_ENGINE_SEARCH_OK");
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("src/example.rs")).unwrap(),
        source
    );
    let history = bone::history(&fixture.data, result["session_id"].as_str().unwrap()).unwrap();
    let input = history.iter().find(|event| event.kind == "input").unwrap();
    let started = history
        .iter()
        .find(|event| event.kind == "tool_started")
        .unwrap();
    assert_eq!(started.data["tool_name"], "search_files");
    assert_eq!(started.data["effect"], "read");
    assert_eq!(started.job_id, input.job_id);
    assert!(started.call_id.is_some());
    assert!(
        !history
            .iter()
            .any(|event| event.kind == "tool_started" && event.data["effect"] == "write")
    );
    let requests = fixture.requests();
    assert_eq!(requests.len(), 2);
    let schemas = requests[0]["body"]["tools"].as_array().unwrap();
    assert!(
        schemas
            .iter()
            .any(|schema| schema["name"] == "search_files")
    );
    assert!(!schemas.iter().any(|schema| matches!(
        schema["name"].as_str(),
        Some("edit_file" | "write_file" | "shell")
    )));
}

#[test]
fn configured_endpoint_uses_rig_native_environment_credentials() {
    let mut fixture = support::Fixture::turns(serde_json::json!([
        {"contains":["NATIVE_FACTORY"],"text":"RIG_NATIVE_FACTORY_OK"}
    ]));
    fixture.profile = fixture.local_profile();
    fixture.profile.credential_env = None;
    fixture.save_config();
    let mut command = fixture.command();
    command
        .env_remove("LLAMACPP_API_KEY")
        .args([
            "run",
            "NATIVE_FACTORY",
            "--profile",
            "fixture",
            "--read-only",
            "--single-job",
            "--max-calls",
            "1",
            "--json",
            "--workspace",
        ])
        .arg(&fixture.workspace);
    let output = support::bounded_output(command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["text"], "RIG_NATIVE_FACTORY_OK");
    assert_eq!(fixture.requests().len(), 1);
}
