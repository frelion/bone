use std::{
    ffi::OsString,
    io::Read,
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, Instant},
};

use bone_app::{
    App, AppOptions, AppProblem, ConfigChange, ConfigScope, EndpointConfig, HistoryEntry, InputId,
    InputOutcome, InputState, ModelSelection, Profile, ProfileId, Session, SessionEvent,
    SessionSeq, SubmitInput,
};
use serde::Serialize;

pub const ROOT_HELP: &str = "\
BONE - a durable terminal coding agent\n\
\n\
USAGE:\n\
    bone [--workspace PATH] [--data-dir PATH]\n\
    bone run [OPTIONS]\n\
\n\
COMMANDS:\n\
    run     Complete one task without the interactive UI\n\
    credentials set  Save an API key read from stdin\n\
\n\
Run `bone run --help` for automation and benchmark options.\n";

const RUN_HELP: &str = "\
Run one software-engineering task headlessly and emit a JSON result.\n\
\n\
USAGE:\n\
    bone run --prompt TEXT [OPTIONS]\n\
    bone run --prompt-file PATH [OPTIONS]\n\
    command | bone run --prompt-file - [OPTIONS]\n\
\n\
OPTIONS:\n\
    --workspace PATH          Project directory (default: current directory)\n\
    --data-dir PATH           BONE state directory (default: platform state directory)\n\
    --prompt TEXT             Task instruction\n\
    --prompt-file PATH        Read task instruction from PATH, or stdin with `-`\n\
    --title TEXT              Session title (default: Headless task)\n\
    --timeout-seconds N       Wall-clock limit (default: 1800)\n\
    --trajectory PATH         Write the complete durable event history as JSON\n\
    --result PATH             Also write the final JSON result to PATH\n\
    --job-budget N            Lifetime descendant limit per root (0 forbids delegation)\n\
    --job-depth N             Maximum Job tree depth including root (1 forbids children)\n\
\n\
MODEL OPTIONS:\n\
    --model ID                Configure a model for this workspace\n\
    --provider NAME           openai-responses (default), openai-chat, anthropic,\n\
                              or chatgpt (uses an existing subscription login)\n\
    --base-url URL            HTTP(S) URL for a compatible provider\n\
\n\
If --model is omitted, `run` uses the model already configured in BONE.\n\
Exit codes: 0 completed, 2 usage, 3 failed, 4 needs input, 124 timeout, 130 interrupted.\n";

const CREDENTIAL_HELP: &str = "\
Save one API key using BONE's credential backend. The key is read from stdin.\n\
\n\
USAGE:\n\
    printf '%s' \"$API_KEY\" | bone credentials set --provider NAME [OPTIONS]\n\
\n\
OPTIONS:\n\
    --provider NAME    openai-responses, openai-chat, or anthropic\n\
    --profile ID       Profile ID (default: headless provider profile)\n\
    --base-url URL     HTTP(S) URL for a compatible provider\n";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Provider {
    OpenAiResponses,
    OpenAiChat,
    Anthropic,
    ChatGpt,
}

impl Provider {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "openai-responses" => Ok(Self::OpenAiResponses),
            "openai-chat" => Ok(Self::OpenAiChat),
            "anthropic" => Ok(Self::Anthropic),
            "chatgpt" => Ok(Self::ChatGpt),
            _ => Err(format!(
                "unknown provider `{value}`; expected openai-responses, openai-chat, anthropic, or chatgpt"
            )),
        }
    }

    fn profile_id(self) -> &'static str {
        match self {
            Self::OpenAiResponses => "headless-openai-responses",
            Self::OpenAiChat => "headless-openai-chat",
            Self::Anthropic => "headless-anthropic",
            Self::ChatGpt => "chatgpt",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::OpenAiResponses => "Headless OpenAI Responses",
            Self::OpenAiChat => "Headless OpenAI Chat Completions",
            Self::Anthropic => "Headless Anthropic Messages",
            Self::ChatGpt => "ChatGPT subscription",
        }
    }

    fn endpoint(self, base_url: Option<String>) -> EndpointConfig {
        match self {
            Self::OpenAiResponses => EndpointConfig::OpenAiResponses { base_url },
            Self::OpenAiChat => EndpointConfig::OpenAiChatCompletions { base_url },
            Self::Anthropic => EndpointConfig::AnthropicMessages { base_url },
            Self::ChatGpt => EndpointConfig::ChatGptSubscription,
        }
    }
}

#[derive(Debug)]
struct Options {
    workspace: PathBuf,
    data_dir: Option<PathBuf>,
    prompt: String,
    title: String,
    timeout: Duration,
    trajectory: Option<PathBuf>,
    result: Option<PathBuf>,
    job_budget: Option<usize>,
    job_depth: Option<usize>,
    model: Option<String>,
    provider: Provider,
    base_url: Option<String>,
}

#[derive(Debug)]
enum ParseResult {
    Run(Box<Options>),
    Help,
}

#[derive(Debug)]
enum Completion {
    Terminal(InputState),
    NeedsInput(String),
    Problem(AppProblem),
    Timeout,
    Interrupted,
}

#[derive(Serialize)]
struct RunReport {
    schema_version: u8,
    status: &'static str,
    exit_code: u8,
    session_id: String,
    input_id: u64,
    workspace: PathBuf,
    duration_ms: u128,
    summary: Option<String>,
    message: Option<String>,
    changed_files: Vec<String>,
    trajectory: Option<PathBuf>,
}

pub async fn run(args: &[OsString]) -> ExitCode {
    let options = match parse(args) {
        Ok(ParseResult::Help) => {
            print!("{RUN_HELP}");
            return ExitCode::SUCCESS;
        }
        Ok(ParseResult::Run(options)) => *options,
        Err(message) => {
            eprintln!("bone run: {message}\n\n{RUN_HELP}");
            return ExitCode::from(2);
        }
    };

    match execute(options).await {
        Ok(code) => ExitCode::from(code),
        Err(message) => {
            eprintln!("bone run: {message}");
            ExitCode::FAILURE
        }
    }
}

pub async fn credentials(args: &[OsString]) -> ExitCode {
    match provision_credentials(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("bone credentials: {message}\n\n{CREDENTIAL_HELP}");
            ExitCode::from(2)
        }
    }
}

async fn provision_credentials(args: &[OsString]) -> Result<(), String> {
    if args
        .first()
        .is_some_and(|arg| arg == "--help" || arg == "-h")
    {
        print!("{CREDENTIAL_HELP}");
        return Ok(());
    }
    if args.first().is_none_or(|arg| arg != "set") {
        return Err("expected `set`".into());
    }
    let mut provider = None;
    let mut profile = None;
    let mut base_url = None;
    let mut index = 1;
    while index < args.len() {
        let option = args[index]
            .to_str()
            .ok_or_else(|| "arguments must be UTF-8".to_owned())?;
        index += 1;
        let value = || {
            args.get(index)
                .and_then(|value| value.to_str())
                .ok_or_else(|| format!("{option} needs a value"))
        };
        match option {
            "--provider" => provider = Some(Provider::parse(value()?)?),
            "--profile" => profile = Some(value()?.to_owned()),
            "--base-url" => base_url = Some(value()?.to_owned()),
            "--help" | "-h" => {
                print!("{CREDENTIAL_HELP}");
                return Ok(());
            }
            other => return Err(format!("unknown option `{other}`")),
        }
        index += 1;
    }
    let provider = provider.ok_or_else(|| "--provider is required".to_owned())?;
    if provider == Provider::ChatGpt {
        return Err("chatgpt uses `bone` interactive login, not an API key".into());
    }
    let profile = Profile::new(
        ProfileId::new(profile.unwrap_or_else(|| provider.profile_id().to_owned()))
            .map_err(|error| error.to_string())?,
        provider.label(),
        provider.endpoint(base_url),
    )
    .map_err(|error| error.to_string())?;
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|error| format!("could not read API key from stdin: {error}"))?;
    while input.ends_with(['\n', '\r']) {
        input.pop();
    }
    let key = bone_app::ApiKey::new(input).map_err(|error| error.to_string())?;
    App::provision_api_key(
        AppOptions::default_bone_home().map_err(|error| error.to_string())?,
        profile,
        key,
    )
    .await
    .map_err(|error| error.to_string())
}

async fn execute(options: Options) -> Result<u8, String> {
    let started = Instant::now();
    let app_options = match &options.data_dir {
        Some(path) => AppOptions::with_paths(
            path,
            AppOptions::default_bone_home().map_err(|error| error.to_string())?,
        ),
        None => AppOptions::platform_default().map_err(|error| error.to_string())?,
    };
    let app = App::open(app_options)
        .await
        .map_err(|error| format!("could not open application state: {error}"))?;
    let workspace = app
        .open_workspace(&options.workspace)
        .await
        .map_err(|error| format!("could not open workspace: {error}"))?;

    if let Some(model) = &options.model {
        configure_model(&app, workspace.id, &options, model).await?;
    }
    if options.job_budget.is_some() || options.job_depth.is_some() {
        let resolved = app
            .resolved_workspace_config(workspace.id)
            .await
            .map_err(|error| format!("could not resolve workspace config: {error}"))?;
        let config = resolved
            .desired
            .map_err(|error| format!("invalid workspace config: {error:?}"))?;
        let mut limits = config.limits;
        if let Some(budget) = options.job_budget {
            limits.job_budget = budget;
        }
        if let Some(depth) = options.job_depth {
            limits.job_depth = depth;
        }
        app.update_config(
            ConfigScope::Workspace(workspace.id),
            ConfigChange::Limits(Some(limits)),
        )
        .await
        .map_err(|error| format!("could not configure Job limits: {error}"))?;
    }

    let session = app
        .create_session(workspace.id, options.title.clone())
        .await
        .map_err(|error| format!("could not create session: {error}"))?;
    let receipt = session
        .submit(SubmitInput::new(options.prompt.clone()))
        .await
        .map_err(|error| format!("could not submit task: {error}"))?;

    eprintln!(
        "BONE headless session {} is working in {}",
        session.id(),
        workspace.root.display()
    );
    let completion = wait_for_completion(&session, receipt.input, options.timeout).await;
    if matches!(completion, Completion::Timeout | Completion::Interrupted) {
        let _ = session.stop().await;
    }

    let history = read_all_history(&session).await?;
    if let Some(path) = &options.trajectory {
        write_json(path, &history)?;
    }
    let summary = final_result(&history, receipt.input);
    let changed_files = read_changed_files(&app, workspace.id).await?;
    let (status, exit_code, message) = classify(completion);
    let report = RunReport {
        schema_version: 1,
        status,
        exit_code,
        session_id: session.id().to_string(),
        input_id: receipt.input.0,
        workspace: workspace.root,
        duration_ms: started.elapsed().as_millis(),
        summary,
        message,
        changed_files,
        trajectory: options.trajectory,
    };
    let rendered = serde_json::to_string_pretty(&report)
        .map_err(|error| format!("could not encode result: {error}"))?;
    if let Some(path) = &options.result {
        write_bytes(path, rendered.as_bytes())?;
    }
    println!("{rendered}");

    app.shutdown()
        .await
        .map_err(|error| format!("could not shut down cleanly: {error}"))?;
    Ok(exit_code)
}

async fn configure_model(
    app: &App,
    workspace: bone_app::WorkspaceId,
    options: &Options,
    model: &str,
) -> Result<(), String> {
    if options.provider == Provider::ChatGpt && options.base_url.is_some() {
        return Err("chatgpt does not accept --base-url".into());
    }
    let profile_id = ProfileId::new(options.provider.profile_id()).map_err(|e| e.to_string())?;
    let profile = if options.provider == Provider::ChatGpt {
        Profile::chatgpt()
    } else {
        Profile::new(
            profile_id.clone(),
            options.provider.label(),
            options.provider.endpoint(options.base_url.clone()),
        )
        .map_err(|error| format!("invalid provider configuration: {error}"))?
    };
    app.save_profile(profile)
        .await
        .map_err(|error| format!("could not save provider profile: {error}"))?;
    let selection = ModelSelection::new(profile_id, model)
        .map_err(|error| format!("invalid model: {error}"))?;
    app.update_config(
        ConfigScope::Workspace(workspace),
        ConfigChange::Model(Some(selection)),
    )
    .await
    .map_err(|error| format!("could not select model: {error}"))?;
    Ok(())
}

async fn wait_for_completion(session: &Session, input: InputId, timeout: Duration) -> Completion {
    let mut view = session.observe();
    let mut history_cursor = SessionSeq(0);
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    loop {
        let snapshot = view.borrow().clone();
        if let Some(current) = snapshot.inputs.iter().find(|item| item.id == input) {
            match &current.state {
                InputState::WaitingForUser { text, .. } => {
                    return Completion::NeedsInput(text.clone());
                }
                InputState::ConversationFailed { message, .. } => {
                    return Completion::Terminal(InputState::Rejected {
                        message: message.clone(),
                    });
                }
                state if state.terminal() => return Completion::Terminal(state.clone()),
                _ => {}
            }
        }
        match read_history_since(session, &mut history_cursor).await {
            Ok(history) => {
                if let Some(state) = terminal_state(&history, input) {
                    return Completion::Terminal(state);
                }
            }
            Err(message) => return Completion::Problem(AppProblem::Storage(message)),
        }
        if let Some(problem) = &snapshot.problem {
            return Completion::Problem(problem.clone());
        }
        tokio::select! {
            _ = &mut deadline => return Completion::Timeout,
            signal = tokio::signal::ctrl_c() => {
                if signal.is_ok() {
                    return Completion::Interrupted;
                }
            }
            changed = view.changed() => {
                if changed.is_err() {
                    return Completion::Problem(AppProblem::Agent("session closed unexpectedly".into()));
                }
            }
        }
    }
}

fn terminal_state(history: &[HistoryEntry], input: InputId) -> Option<InputState> {
    history.iter().rev().find_map(|entry| match &entry.event {
        SessionEvent::InputFinished {
            runtime,
            input: finished,
            outcome,
        } if *finished == input => Some(InputState::Finished {
            runtime: *runtime,
            outcome: outcome.clone(),
        }),
        SessionEvent::InputRejected {
            input: rejected,
            message,
        } if *rejected == input => Some(InputState::Rejected {
            message: message.clone(),
        }),
        SessionEvent::InputCancelled { input: cancelled } if *cancelled == input => {
            Some(InputState::Cancelled)
        }
        SessionEvent::Interrupted { runtime, inputs } if inputs.contains(&input) => {
            Some(InputState::Interrupted { runtime: *runtime })
        }
        _ => None,
    })
}

async fn read_all_history(session: &Session) -> Result<Vec<HistoryEntry>, String> {
    let mut cursor = SessionSeq(0);
    read_history_since(session, &mut cursor).await
}

async fn read_history_since(
    session: &Session,
    cursor: &mut SessionSeq,
) -> Result<Vec<HistoryEntry>, String> {
    let mut items = Vec::new();
    loop {
        let page = session
            .history(*cursor, 256)
            .await
            .map_err(|error| format!("could not read session history: {error}"))?;
        *cursor = page.next_cursor;
        items.extend(page.items);
        if !page.has_more {
            return Ok(items);
        }
    }
}

fn final_result(history: &[HistoryEntry], input: InputId) -> Option<String> {
    history.iter().rev().find_map(|entry| match &entry.event {
        SessionEvent::Reply { inputs, text } if inputs.contains(&input) => Some(text.clone()),
        _ => None,
    })
}

async fn read_changed_files(
    app: &App,
    workspace: bone_app::WorkspaceId,
) -> Result<Vec<String>, String> {
    let mut cursor = None;
    let mut files = Vec::new();
    loop {
        let page = app
            .workspace_changes(workspace, cursor, 256)
            .await
            .map_err(|error| format!("could not inspect workspace changes: {error}"))?;
        files.extend(page.files.into_iter().map(|file| file.path));
        cursor = page.next_cursor;
        if cursor.is_none() {
            return Ok(files);
        }
    }
}

fn classify(completion: Completion) -> (&'static str, u8, Option<String>) {
    match completion {
        Completion::Terminal(InputState::Finished {
            outcome: InputOutcome::Completed,
            ..
        }) => ("completed", 0, None),
        Completion::Terminal(InputState::Finished {
            outcome: InputOutcome::Failed,
            ..
        }) => ("failed", 3, Some("agent reported failure".into())),
        Completion::Terminal(InputState::Finished {
            outcome: InputOutcome::Cancelled,
            ..
        })
        | Completion::Terminal(InputState::Cancelled) => {
            ("cancelled", 3, Some("task was cancelled".into()))
        }
        Completion::Terminal(InputState::Rejected { message }) => ("failed", 3, Some(message)),
        Completion::Terminal(InputState::Interrupted { .. }) => {
            ("interrupted", 130, Some("runtime interrupted".into()))
        }
        Completion::Terminal(other) => (
            "failed",
            3,
            Some(format!("unexpected terminal state: {other:?}")),
        ),
        Completion::NeedsInput(question) => ("needs_input", 4, Some(question)),
        Completion::Problem(AppProblem::Credential(problem)) => {
            use bone_app::CredentialProblemKind::*;
            let message = match problem.kind {
                Missing => format!(
                    "profile {} needs an API key in $BONE_HOME/credentials.toml; save it through the TUI first",
                    problem.profile
                ),
                Unavailable => format!(
                    "$BONE_HOME/credentials.toml is unavailable or unsafe for profile {}",
                    problem.profile
                ),
                EndpointMismatch => format!(
                    "the API key for profile {} belongs to a different endpoint; re-enter it through the TUI",
                    problem.profile
                ),
            };
            ("failed", 3, Some(message))
        }
        Completion::Problem(problem) => ("failed", 3, Some(format!("{problem:?}"))),
        Completion::Timeout => ("timeout", 124, Some("task exceeded its time limit".into())),
        Completion::Interrupted => ("interrupted", 130, Some("interrupted by user".into())),
    }
}

fn parse(args: &[OsString]) -> Result<ParseResult, String> {
    let mut workspace = None;
    let mut data_dir = None;
    let mut prompt = None;
    let mut prompt_file = None;
    let mut title = "Headless task".to_owned();
    let mut timeout = Duration::from_secs(1800);
    let mut trajectory = None;
    let mut result = None;
    let mut job_budget = None;
    let mut job_depth = None;
    let mut model = None;
    let mut provider = Provider::OpenAiResponses;
    let mut base_url = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index]
            .to_str()
            .ok_or_else(|| "arguments must be valid UTF-8".to_owned())?;
        if flag == "--help" || flag == "-h" {
            return Ok(ParseResult::Help);
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{flag} requires a value"))?
            .to_str()
            .ok_or_else(|| format!("value for {flag} must be valid UTF-8"))?;
        match flag {
            "--workspace" => workspace = Some(PathBuf::from(value)),
            "--data-dir" => data_dir = Some(PathBuf::from(value)),
            "--prompt" => prompt = Some(value.to_owned()),
            "--prompt-file" => prompt_file = Some(value.to_owned()),
            "--title" => title = value.to_owned(),
            "--job-budget" => {
                job_budget = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| "--job-budget must be a nonnegative integer")?,
                )
            }
            "--job-depth" => {
                let depth = value
                    .parse::<usize>()
                    .map_err(|_| "--job-depth must be a positive integer")?;
                if depth == 0 {
                    return Err("--job-depth must be greater than zero".into());
                }
                job_depth = Some(depth);
            }
            "--timeout-seconds" => {
                let seconds = value
                    .parse::<u64>()
                    .map_err(|_| "--timeout-seconds must be a positive integer".to_owned())?;
                if seconds == 0 {
                    return Err("--timeout-seconds must be greater than zero".into());
                }
                timeout = Duration::from_secs(seconds);
            }
            "--trajectory" => trajectory = Some(PathBuf::from(value)),
            "--result" => result = Some(PathBuf::from(value)),
            "--model" => model = Some(value.to_owned()),
            "--provider" => provider = Provider::parse(value)?,
            "--base-url" => base_url = Some(value.to_owned()),
            _ => return Err(format!("unknown option `{flag}`")),
        }
        index += 2;
    }
    if prompt.is_some() && prompt_file.is_some() {
        return Err("use exactly one of --prompt or --prompt-file".into());
    }
    let prompt = match (prompt, prompt_file) {
        (Some(prompt), None) => prompt,
        (None, Some(path)) => read_prompt(&path)?,
        (None, None) => return Err("one of --prompt or --prompt-file is required".into()),
        (Some(_), Some(_)) => unreachable!(),
    };
    if prompt.trim().is_empty() {
        return Err("task prompt cannot be empty".into());
    }
    if base_url.is_some() && model.is_none() {
        return Err("--base-url requires --model".into());
    }
    let workspace = match workspace {
        Some(path) => path,
        None => std::env::current_dir().map_err(|error| error.to_string())?,
    };
    Ok(ParseResult::Run(Box::new(Options {
        workspace,
        data_dir,
        prompt,
        title,
        timeout,
        trajectory,
        result,
        job_budget,
        job_depth,
        model,
        provider,
        base_url,
    })))
}

fn read_prompt(path: &str) -> Result<String, String> {
    if path == "-" {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|error| format!("could not read prompt from stdin: {error}"))?;
        Ok(text)
    } else {
        std::fs::read_to_string(path)
            .map_err(|error| format!("could not read prompt file `{path}`: {error}"))
    }
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("could not encode `{}`: {error}", path.display()))?;
    write_bytes(path, &bytes)
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create `{}`: {error}", parent.display()))?;
    }
    std::fs::write(path, bytes)
        .map_err(|error| format!("could not write `{}`: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_uses_the_matching_conversation_reply_and_explicit_input_completion() {
        let runtime = bone_app::RuntimeId::new();
        let event = |sequence, event| HistoryEntry {
            sequence: SessionSeq(sequence),
            occurred_at: 0,
            event,
        };
        let mut history = vec![
            event(
                1,
                SessionEvent::Reply {
                    inputs: vec![InputId(1)],
                    text: "final user answer".into(),
                },
            ),
            event(
                2,
                SessionEvent::JobFinished {
                    job: bone_app::JobRef { runtime, id: 1 },
                    outcome: bone_app::OutcomeKind::Completed,
                    summary: "internal child summary".into(),
                    remaining: vec![],
                },
            ),
            event(
                3,
                SessionEvent::Reply {
                    inputs: vec![InputId(2)],
                    text: "other request".into(),
                },
            ),
        ];
        assert_eq!(
            final_result(&history, InputId(1)).as_deref(),
            Some("final user answer")
        );
        assert!(terminal_state(&history, InputId(1)).is_none());
        history.push(event(
            4,
            SessionEvent::InputFinished {
                runtime,
                input: InputId(1),
                outcome: InputOutcome::Completed,
            },
        ));
        assert!(matches!(
            terminal_state(&history, InputId(1)),
            Some(InputState::Finished { .. })
        ));
    }

    #[test]
    fn parses_benchmark_options() {
        let parsed = parse(&[
            "--prompt".into(),
            "fix the tests".into(),
            "--model".into(),
            "gpt-test".into(),
            "--provider".into(),
            "openai-chat".into(),
            "--timeout-seconds".into(),
            "60".into(),
        ])
        .unwrap();
        let ParseResult::Run(options) = parsed else {
            panic!("expected runnable options")
        };
        assert_eq!(options.prompt, "fix the tests");
        assert_eq!(options.model.as_deref(), Some("gpt-test"));
        assert_eq!(options.provider, Provider::OpenAiChat);
        assert_eq!(options.timeout, Duration::from_secs(60));
    }

    #[test]
    fn parses_explicit_job_limits_and_rejects_invalid_bounds() {
        let args =
            ["--prompt", "do it", "--job-budget", "0", "--job-depth", "1"].map(OsString::from);
        let ParseResult::Run(options) = parse(&args).unwrap() else {
            panic!("expected run")
        };
        assert_eq!(options.job_budget, Some(0));
        assert_eq!(options.job_depth, Some(1));
        for (flag, value) in [
            ("--job-budget", "-1"),
            ("--job-depth", "0"),
            ("--job-depth", "no"),
        ] {
            let args = ["--prompt", "do it", flag, value].map(OsString::from);
            assert!(parse(&args).is_err());
        }
    }

    #[test]
    fn rejects_ambiguous_prompt_sources() {
        let error = parse(&[
            "--prompt".into(),
            "task".into(),
            "--prompt-file".into(),
            "task.md".into(),
        ])
        .unwrap_err();
        assert!(error.contains("exactly one"));
    }

    #[test]
    fn classifies_public_exit_contract() {
        let (status, code, _) = classify(Completion::Timeout);
        assert_eq!((status, code), ("timeout", 124));
        let (status, code, _) = classify(Completion::NeedsInput("which file?".into()));
        assert_eq!((status, code), ("needs_input", 4));
    }
}
