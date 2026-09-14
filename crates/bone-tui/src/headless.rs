use std::{
    ffi::OsString,
    io::Read,
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, Instant},
};

use bone_app::{
    AgentLimits, ApiKey, App, AppOptions, AppProblem, ConfigChange, ConfigScope, EndpointConfig,
    HistoryEntry, InputId, InputOutcome, InputState, ModelSelection, Profile, ProfileId, Session,
    SessionEvent, SessionSeq, SubmitInput, ToolLimits, ToolMode, ToolSettings,
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
    --read-only               Disable workspace-writing tools\n\
\n\
MODEL OPTIONS:\n\
    --model ID                Configure a model for this workspace\n\
    --provider NAME           openai-responses (default), openai-chat, anthropic,\n\
                              or chatgpt (uses an existing subscription login)\n\
    --base-url URL            HTTPS URL for a compatible provider\n\
    --api-key-env NAME        Environment variable containing the API key\n\
                              (defaults to OPENAI_API_KEY or ANTHROPIC_API_KEY)\n\
\n\
If --model is omitted, `run` uses the model already configured in BONE.\n\
Exit codes: 0 completed, 2 usage, 3 failed, 4 needs input, 124 timeout, 130 interrupted.\n";

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

    fn default_key_env(self) -> &'static str {
        match self {
            Self::Anthropic => "ANTHROPIC_API_KEY",
            Self::OpenAiResponses | Self::OpenAiChat => "OPENAI_API_KEY",
            Self::ChatGpt => "",
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
    read_only: bool,
    model: Option<String>,
    provider: Provider,
    base_url: Option<String>,
    api_key_env: Option<String>,
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
    remaining: Vec<String>,
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

async fn execute(options: Options) -> Result<u8, String> {
    let started = Instant::now();
    let app_options = match &options.data_dir {
        Some(path) => AppOptions::new(path),
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
    let tool_limits = ToolLimits::default();
    if !options.read_only {
        // A previous failed launch may have persisted write mode with limits that
        // cannot be resolved together. Temporarily restoring read-only mode lets
        // us preserve every effective agent limit except the incompatible timeout.
        app.update_config(
            ConfigScope::Workspace(workspace.id),
            ConfigChange::Tools(Some(ToolSettings {
                mode: ToolMode::ReadOnly,
                limits: tool_limits.clone(),
            })),
        )
        .await
        .map_err(|error| format!("could not prepare workspace tools: {error}"))?;
        let resolved = app
            .resolved_workspace_config(workspace.id)
            .await
            .map_err(|error| format!("could not resolve workspace config: {error}"))?;
        if let Ok(config) = resolved.desired {
            let mut agent_limits = config.limits;
            if raise_tool_timeout(&mut agent_limits, &tool_limits) {
                app.update_config(
                    ConfigScope::Workspace(workspace.id),
                    ConfigChange::Limits(Some(agent_limits)),
                )
                .await
                .map_err(|error| format!("could not configure agent limits: {error}"))?;
            }
        }
    }
    app.update_config(
        ConfigScope::Workspace(workspace.id),
        ConfigChange::Tools(Some(ToolSettings {
            mode: if options.read_only {
                ToolMode::ReadOnly
            } else {
                ToolMode::WorkspaceWrite
            },
            limits: tool_limits,
        })),
    )
    .await
    .map_err(|error| format!("could not configure workspace tools: {error}"))?;

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
    let (summary, remaining) = final_result(&history);
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
        remaining,
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

fn raise_tool_timeout(agent: &mut AgentLimits, tools: &ToolLimits) -> bool {
    if agent.tool_timeout > tools.max_bash_timeout {
        return false;
    }
    agent.tool_timeout = tools.max_bash_timeout + Duration::from_secs(1);
    true
}

async fn configure_model(
    app: &App,
    workspace: bone_app::WorkspaceId,
    options: &Options,
    model: &str,
) -> Result<(), String> {
    if options.provider == Provider::ChatGpt
        && (options.base_url.is_some() || options.api_key_env.is_some())
    {
        return Err("chatgpt does not accept --base-url or --api-key-env".into());
    }
    let api_key = if options.provider == Provider::ChatGpt {
        None
    } else {
        let key_env = options
            .api_key_env
            .as_deref()
            .unwrap_or_else(|| options.provider.default_key_env());
        let key = std::env::var(key_env)
            .map_err(|_| format!("{key_env} is required when --model is specified"))?;
        Some(ApiKey::new(key).map_err(|error| error.to_string())?)
    };
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
    if let Some(key) = api_key {
        app.set_volatile_api_key(profile_id.clone(), key)
            .await
            .map_err(|error| format!("could not configure provider credential: {error}"))?;
    }
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
                InputState::RoutingFailed { message, .. } => {
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

fn final_result(history: &[HistoryEntry]) -> (Option<String>, Vec<String>) {
    history
        .iter()
        .rev()
        .find_map(|entry| match &entry.event {
            SessionEvent::JobFinished {
                summary, remaining, ..
            } => Some((Some(summary.clone()), remaining.clone())),
            _ => None,
        })
        .unwrap_or_default()
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
    let mut read_only = false;
    let mut model = None;
    let mut provider = Provider::OpenAiResponses;
    let mut base_url = None;
    let mut api_key_env = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index]
            .to_str()
            .ok_or_else(|| "arguments must be valid UTF-8".to_owned())?;
        if flag == "--help" || flag == "-h" {
            return Ok(ParseResult::Help);
        }
        if flag == "--read-only" {
            read_only = true;
            index += 1;
            continue;
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
            "--api-key-env" => api_key_env = Some(value.to_owned()),
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
    if (base_url.is_some() || api_key_env.is_some()) && model.is_none() {
        return Err("--base-url and --api-key-env require --model".into());
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
        read_only,
        model,
        provider,
        base_url,
        api_key_env,
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
        assert!(!options.read_only);
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

    #[test]
    fn write_tools_only_raise_an_incompatible_agent_timeout() {
        let tools = ToolLimits::default();
        let mut limits = AgentLimits {
            background_workers: 7,
            ..AgentLimits::default()
        };
        assert!(raise_tool_timeout(&mut limits, &tools));
        assert_eq!(
            limits.tool_timeout,
            tools.max_bash_timeout + Duration::from_secs(1)
        );
        assert_eq!(limits.background_workers, 7);
        assert!(!raise_tool_timeout(&mut limits, &tools));
    }
}
