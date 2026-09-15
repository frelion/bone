use std::{
    io::Write,
    process::{Command, Stdio},
};

#[test]
fn run_help_documents_the_machine_contract() {
    let output = Command::new(env!("CARGO_BIN_EXE_bone"))
        .args(["run", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("--trajectory PATH"));
    assert!(stdout.contains("--trust-project-config"));
    assert!(!stdout.contains("--api-key-env"));
    assert!(stdout.contains("Exit codes: 0 completed"));
}

#[test]
fn credentials_command_provisions_through_stdin() {
    let temporary = tempfile::tempdir().unwrap();
    let home = temporary.path().join("home");
    let mut child = Command::new(env!("CARGO_BIN_EXE_bone"))
        .args([
            "credentials",
            "set",
            "--provider",
            "openai-responses",
            "--profile",
            "headless-openai-responses",
            "--base-url",
            "https://example.test/v1/",
        ])
        .env("BONE_HOME", &home)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"stdin-only-secret\n")
        .unwrap();
    assert!(child.wait().unwrap().success());

    let credentials = home.join("credentials.toml");
    let contents = std::fs::read_to_string(&credentials).unwrap();
    assert!(contents.contains("headless-openai-responses"));
    assert!(contents.contains("stdin-only-secret"));
    assert!(contents.contains("endpoint_fingerprint"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(credentials).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
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
        .env("BONE_HOME", temporary.path().join("home"))
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
fn explicit_model_uses_the_user_credential_file_contract() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
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
            "--timeout-seconds",
            "10",
        ])
        .env("BONE_HOME", temporary.path().join("home"))
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
    let result: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(result["status"], "failed");
    assert!(
        result["message"]
            .as_str()
            .unwrap()
            .contains("$BONE_HOME/credentials.toml")
    );
    assert_ne!(result["message"], "Configuration(NeedsModel)");
    assert_ne!(
        result["message"],
        "Configuration(Invalid(\"agent tool timeout must exceed the largest Bash timeout\"))"
    );
}

#[test]
fn project_config_requires_an_explicit_headless_trust_flag() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir_all(workspace.join(".bone")).unwrap();
    std::fs::write(
        workspace.join(".bone/config.toml"),
        "schema_version = 1\n[overrides]\n",
    )
    .unwrap();
    let data = temporary.path().join("data");
    let home = temporary.path().join("home");

    let rejected = Command::new(env!("CARGO_BIN_EXE_bone"))
        .arg("run")
        .arg("--workspace")
        .arg(&workspace)
        .arg("--data-dir")
        .arg(&data)
        .args(["--prompt", "inspect", "--timeout-seconds", "1"])
        .env("BONE_HOME", &home)
        .output()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(1));
    assert!(
        String::from_utf8(rejected.stderr)
            .unwrap()
            .contains("--trust-project-config")
    );

    let trusted = Command::new(env!("CARGO_BIN_EXE_bone"))
        .arg("run")
        .arg("--workspace")
        .arg(&workspace)
        .arg("--data-dir")
        .arg(&data)
        .args([
            "--prompt",
            "inspect",
            "--timeout-seconds",
            "1",
            "--trust-project-config",
        ])
        .env("BONE_HOME", &home)
        .output()
        .unwrap();
    assert_eq!(trusted.status.code(), Some(3));
    assert!(
        !String::from_utf8(trusted.stderr)
            .unwrap()
            .contains("not trusted")
    );
}
