#[test]
fn cli_requires_system_configuration_even_when_a_solver_is_selected() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    let events = directory.path().join("events.jsonl");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_bone"))
        .env("BONE_CONFIG", &path)
        .env("BONE_MODEL", "solver-from-environment")
        .arg("--events")
        .arg(&events)
        .args(["--model", "solver-from-task", "hello"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("agent.system"), "{error}");
    assert!(error.contains("section is required"), "{error}");
    assert!(!error.contains("authorization required"));
    assert!(
        !events.exists(),
        "failed startup must not leave an empty event log"
    );
    assert!(
        !path.exists(),
        "startup must not create a default system configuration"
    );
}

#[test]
fn cli_help_and_invalid_options_do_not_require_configuration_or_credentials() {
    let directory = tempfile::tempdir().unwrap();
    for (args, succeeds) in [
        (vec!["--help"], true),
        (vec!["--model"], false),
        (vec!["--coordinator-model", "another-model"], false),
    ] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_bone"))
            .env(
                "BONE_CONFIG",
                directory.path().join("not-created/config.json"),
            )
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.success(), succeeds);
    }
    assert!(!directory.path().join("not-created").exists());
}
