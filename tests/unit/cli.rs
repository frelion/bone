use super::*;

#[test]
fn bare_cli_uses_the_explicit_tui_defaults() {
    let bare = Cli::try_parse_from(["bone"]).unwrap();
    let explicit = Cli::try_parse_from(["bone", "tui"]).unwrap();
    let Command::Tui { run: bare_run } = bare.command.unwrap_or_default() else {
        panic!("bare invocation must select TUI");
    };
    let Some(Command::Tui { run: explicit_run }) = explicit.command else {
        panic!("explicit TUI must remain available");
    };
    assert_eq!(bare_run, explicit_run);
}

#[test]
fn implicit_tui_preserves_global_options_and_explicit_commands() {
    let cli = Cli::try_parse_from([
        "bone",
        "--data-dir",
        "/tmp/bone-cli-test",
        "--profile",
        "subscription",
        "--model",
        "chatgpt:gpt-6-luna",
    ])
    .unwrap();
    assert_eq!(cli.data_dir, Some(PathBuf::from("/tmp/bone-cli-test")));
    assert_eq!(cli.profile.as_deref(), Some("subscription"));
    assert_eq!(cli.model.as_deref(), Some("chatgpt:gpt-6-luna"));
    assert!(matches!(
        cli.command.unwrap_or_default(),
        Command::Tui { .. }
    ));

    let cli = Cli::try_parse_from([
        "bone",
        "run",
        "inspect the project",
        "--read-only",
        "--max-calls",
        "7",
        "--profile",
        "work",
    ])
    .unwrap();
    assert_eq!(cli.profile.as_deref(), Some("work"));
    let Some(Command::Run { prompt, run, .. }) = cli.command else {
        panic!("explicit run must retain its arguments");
    };
    assert_eq!(prompt, "inspect the project");
    assert!(run.read_only);
    assert_eq!(run.max_calls, 7);
    assert!(matches!(
        Cli::try_parse_from(["bone", "tools", "--json"])
            .unwrap()
            .command,
        Some(Command::Tools { json: true, .. })
    ));
    assert!(Cli::try_parse_from(["bone", "unknown-command"]).is_err());
    assert!(Cli::try_parse_from(["bone", "run"]).is_err());
    assert_eq!(
        Cli::try_parse_from(["bone", "--help"])
            .err()
            .unwrap()
            .kind(),
        clap::error::ErrorKind::DisplayHelp
    );
    assert_eq!(
        Cli::try_parse_from(["bone", "--version"])
            .err()
            .unwrap()
            .kind(),
        clap::error::ErrorKind::DisplayVersion
    );
}

#[test]
fn model_override_preserves_selected_subscription_credentials_and_limits() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
            directory.path().join("config.toml"),
            "default_profile = 'subscription'\n[profiles.subscription]\nmodel = 'chatgpt:gpt-6-luna'\nreuse_codex_login = true\nmax_tokens = 8192\n",
        ).unwrap();
    for explicit in [false, true] {
        let mut args = vec!["bone", "--model", "chatgpt:gpt-6-luna"];
        if explicit {
            args.extend(["--profile", "subscription"]);
        }
        args.push("login");
        let cli = Cli::parse_from(args);
        let (name, profile) = select_profile(&cli, directory.path()).unwrap();
        assert_eq!(name, "subscription");
        assert!(profile.reuse_codex_login);
        assert_eq!(profile.max_tokens, Some(8192));
    }
    let cli = Cli::parse_from(["bone", "--model", "openai:gpt-6-luna", "login"]);
    let (_, profile) = select_profile(&cli, directory.path()).unwrap();
    assert!(!profile.reuse_codex_login);
    assert!(profile.credential_env.is_none());
}

#[tokio::test]
async fn resume_reports_the_new_input_that_preempted_an_older_call() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let data = directory.path().join("data");
    let profile = Profile::from_model("ollama:fixture").unwrap();
    let mut engine = Engine::open(
        &data,
        &workspace,
        None,
        profile.clone(),
        "fixture".into(),
        RunOptions::default(),
    )
    .unwrap();
    let old = engine.post("old work", None).unwrap();
    engine.step().await.unwrap(); // Drain the input notification.
    engine.step().await.unwrap(); // Persist the model intent without awaiting it.
    assert_eq!(
        engine
            .state()
            .jobs
            .values()
            .next()
            .unwrap()
            .active_input
            .as_ref(),
        Some(&old)
    );
    let latest = engine
        .post("first explain, before continuing", None)
        .unwrap();
    engine.stop().unwrap();
    let session = engine.state().id.clone();
    drop(engine);
    let mut recovered = Engine::open(
        &data,
        &workspace,
        Some(&session),
        profile,
        "fixture".into(),
        RunOptions::default(),
    )
    .unwrap();
    recovered.resume().unwrap();
    assert_eq!(resumed_input(&recovered).unwrap(), latest);
}
