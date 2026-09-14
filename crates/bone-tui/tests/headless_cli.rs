use std::process::Command;

#[test]
fn run_help_documents_the_machine_contract() {
    let output = Command::new(env!("CARGO_BIN_EXE_bone"))
        .args(["run", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("--trajectory PATH"));
    assert!(stdout.contains("Exit codes: 0 completed"));
}

#[test]
fn missing_model_is_a_structured_failure_without_credentials() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let result = temporary.path().join("artifacts/result.json");
    let trajectory = temporary.path().join("artifacts/trajectory.json");

    let output = Command::new(env!("CARGO_BIN_EXE_bone"))
        .arg("run")
        .arg("--workspace")
        .arg(&workspace)
        .arg("--data-dir")
        .arg(temporary.path().join("data"))
        .args(["--prompt", "inspect this project", "--timeout-seconds", "5"])
        .arg("--result")
        .arg(&result)
        .arg("--trajectory")
        .arg(&trajectory)
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(3),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["schema_version"], 1);
    assert_eq!(stdout["status"], "failed");
    assert_eq!(stdout["exit_code"], 3);
    assert!(stdout["message"].as_str().unwrap().contains("NeedsModel"));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(result).unwrap()).unwrap(),
        stdout
    );
    let events: serde_json::Value =
        serde_json::from_slice(&std::fs::read(trajectory).unwrap()).unwrap();
    assert!(events.as_array().is_some_and(|events| !events.is_empty()));
}

#[test]
fn invalid_invocation_uses_the_usage_exit_code() {
    let output = Command::new(env!("CARGO_BIN_EXE_bone"))
        .args(["run", "--prompt", "one", "--prompt-file", "two"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("use exactly one")
    );
}

#[test]
fn explicit_model_uses_a_process_only_key_and_redacts_it_from_output() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let secret = "headless-contract-secret";

    let output = Command::new(env!("CARGO_BIN_EXE_bone"))
        .arg("run")
        .arg("--workspace")
        .arg(&workspace)
        .arg("--data-dir")
        .arg(temporary.path().join("data"))
        .args([
            "--prompt",
            "inspect this project",
            "--model",
            "offline-model",
            "--base-url",
            "https://127.0.0.1:1/v1",
            "--api-key-env",
            "BONE_HEADLESS_TEST_KEY",
            "--timeout-seconds",
            "10",
        ])
        .env("BONE_HEADLESS_TEST_KEY", secret)
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(3),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stdout.contains(secret));
    assert!(!stderr.contains(secret));
    let result: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(result["status"], "failed");
    assert_ne!(result["message"], "Configuration(NeedsModel)");
    assert_ne!(
        result["message"],
        "Configuration(Invalid(\"agent tool timeout must exceed the largest Bash timeout\"))"
    );
}
