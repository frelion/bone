use std::{
    env,
    error::Error,
    io::{self, IsTerminal, Write},
    path::PathBuf,
    process::ExitCode,
};

use bone_agent::{AgentHandle, JobRequest, Notice, Observation, RecordEntry, RecordKind};
use bone_app::{
    ApiKey, ApiKeyCredentialError, ApiKeyCredentials, AppStorageError, LlmProfile, LlmProfileId,
    ModelSelection, ProviderConnector, SettingsError, SettingsService, TuiDisplaySettings,
    TuiError, WorkspaceApplication, WorkspaceApplicationError, WorkspaceError, open_default_store,
    run_storage_repair, run_workspace, write_events,
};
use bone_llm::{EndpointConfig, service::chatgpt_subscription::DeviceCodePrompt};
use bone_store::StoreError;
use crossterm::{
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};
use tokio::sync::broadcast::error::RecvError;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("bone: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn Error>> {
    let arguments = Arguments::parse(env::args().skip(1))?;
    if arguments.help {
        print_help();
        return Ok(());
    }
    if let Some(command) = arguments.credentials.as_ref() {
        return run_credentials(command);
    }
    if let Some(command) = arguments.provider.as_ref() {
        return run_provider(command);
    }
    if arguments.message.is_empty() && arguments.events.is_some() {
        return Err(
            invalid_input("--events is currently available only with one-shot messages").into(),
        );
    }

    let selected_model = match arguments.model {
        Some(model) => Some(model),
        None => match env::var("BONE_MODEL") {
            Ok(model) => Some(model.trim().to_owned()),
            Err(env::VarError::NotPresent) => None,
            Err(_) => return Err(invalid_input("BONE_MODEL must be valid Unicode").into()),
        },
    };
    let selected_profile = match arguments.profile {
        Some(profile) => Some(profile),
        None => match env::var("BONE_PROFILE") {
            Ok(profile) => Some(profile.trim().to_owned()),
            Err(env::VarError::NotPresent) => None,
            Err(_) => return Err(invalid_input("BONE_PROFILE must be valid Unicode").into()),
        },
    };
    let initial_model = initial_model_selection(selected_model, selected_profile)?;
    let workspace = env::current_dir()?;
    let input = arguments.message;
    if input.is_empty() {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err(invalid_input(
                "interactive mode requires a terminal; pass a message as an argument",
            )
            .into());
        }
        // Workspace/session boot intentionally precedes settings, login, and
        // Agent connection. If the sole SQLite store cannot open, keep an
        // interactive user in an explicit no-reset repair screen rather than
        // dropping them back to a raw startup error.
        let reports = loop {
            let store = loop {
                match open_default_store() {
                    Ok(store) => break store,
                    Err(error)
                        if run_storage_repair(default_store_repair_reason(&error)).await? => {}
                    Err(_) => return Ok(()),
                }
            };
            let application = match WorkspaceApplication::open_with_store(&workspace, store.clone())
            {
                Ok(application) => application,
                Err(error) => {
                    let Some(reason) = workspace_repair_reason(&error) else {
                        return Err(error.into());
                    };
                    if run_storage_repair(reason).await? {
                        continue;
                    }
                    return Ok(());
                }
            };
            // Settings are durable state too. Do not stringify an initialization
            // failure and let the Workspace TUI pretend it can run without
            // settings: malformed or invalid settings must follow the same
            // no-reset repair path as every other SQLite-backed startup read.
            let settings = match SettingsService::open(store) {
                Ok(settings) => settings,
                Err(error) => {
                    if run_storage_repair(settings_repair_reason(&error)).await? {
                        continue;
                    }
                    return Ok(());
                }
            };
            if let Some(selection) = initial_model.as_ref() {
                match settings.validate_model_selection(selection) {
                    Ok(()) => {}
                    Err(SettingsError::UnknownLlmProfile(_)) => {
                        return Err(
                            invalid_input("selected provider profile does not exist").into()
                        );
                    }
                    Err(error) => {
                        if run_storage_repair(settings_repair_reason(&error)).await? {
                            continue;
                        }
                        return Ok(());
                    }
                }
            }
            match run_workspace(application, settings, initial_model.clone()).await {
                Ok(reports) => break reports,
                Err(error) => {
                    let Some(reason) = tui_storage_repair_reason(&error) else {
                        return Err(error.into());
                    };
                    if !run_storage_repair(reason).await? {
                        return Ok(());
                    }
                }
            }
        };
        report_unresolved(
            reports
                .iter()
                .flat_map(|report| &report.unresolved_jobs)
                .count(),
        );
        return Ok(());
    }

    // One-shot mode remains a strict automation surface. It uses the same
    // auto-created settings document, but an unconfigured model is reported
    // as an ordinary startup error rather than starting an interactive repair
    // shell on a non-terminal stream.
    let store = open_default_store()?;
    let settings = SettingsService::open(store.clone())?;
    let display = settings.display_settings()?;
    let runtime = settings.resolve_one_shot(initial_model)?;
    let connector = ProviderConnector::new();
    let host = connector.connect_agent(&runtime, show_login).await?;
    let agent = host.start(&workspace, runtime.agent.clone())?;
    let event_log = match arguments.events {
        Some(path) => {
            let file = match tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .await
            {
                Ok(file) => file,
                Err(error) => {
                    agent.shutdown().await?;
                    return Err(error.into());
                }
            };
            Some(tokio::spawn(write_events(agent.observe().await?, file)))
        }
        None => None,
    };

    let observation = agent.observe().await?;
    let result = one_shot(&agent, observation, input, &display).await;

    // Close even when stdin or the model failed. Shutdown collects late results.
    let shutdown = agent.shutdown().await;
    // The actor closes the event stream after collecting late results.
    let logged = match event_log {
        Some(task) => task.await?,
        None => Ok(()),
    };
    let report = shutdown?;
    report_unresolved(report.unresolved_jobs.len());
    result?;
    logged?;
    Ok(())
}

fn initial_model_selection(
    model: Option<String>,
    profile: Option<String>,
) -> io::Result<Option<ModelSelection>> {
    if profile.is_some() && model.is_none() {
        return Err(invalid_input(
            "--profile or BONE_PROFILE requires --model or BONE_MODEL",
        ));
    }
    model
        .map(|model| {
            let profile = profile
                .map(|profile| {
                    LlmProfileId::new(profile)
                        .map_err(|_| invalid_input("--profile requires a valid profile ID"))
                })
                .transpose()?
                .unwrap_or_else(LlmProfileId::chatgpt);
            ModelSelection::new(profile, model, None, None)
                .map_err(|_| invalid_input("--model requires a model ID"))
        })
        .transpose()
}

fn run_credentials(command: &CredentialsCommand) -> Result<(), Box<dyn Error>> {
    let profile_id = LlmProfileId::new(command.profile.clone())
        .map_err(|_| invalid_input("credentials requires a valid profile ID"))?;
    let store = open_default_store()
        .map_err(|_| io::Error::other("provider profile settings are unavailable"))?;
    let settings = SettingsService::open(store).map_err(credential_settings_error)?;
    let profile = settings
        .llm_profile(&profile_id)
        .map_err(credential_settings_error)?;
    if matches!(&profile.endpoint, EndpointConfig::ChatGptSubscription) {
        return Err(
            invalid_input("ChatGPT subscription profiles use /login, not an API key").into(),
        );
    }
    let credentials = ApiKeyCredentials::for_profile(&profile).map_err(credential_store_error)?;

    match command.action {
        CredentialsAction::Set => {
            require_credential_terminal()?;
            let api_key = prompt_for_api_key()?;
            credentials.save(&api_key).map_err(credential_store_error)?;
            eprintln!("API key saved for profile `{}`.", profile.id);
        }
        CredentialsAction::Clear => {
            credentials.clear().map_err(credential_store_error)?;
            eprintln!("API key cleared for profile `{}`.", profile.id);
        }
    }
    Ok(())
}

/// Manage the non-secret endpoint profile catalog from a shell. API keys stay
/// out of this path and must still be entered through the masked credential
/// prompt.
fn run_provider(command: &ProviderCliCommand) -> Result<(), Box<dyn Error>> {
    let store = open_default_store()
        .map_err(|_| io::Error::other("provider profile settings are unavailable"))?;
    let settings = SettingsService::open(store)
        .map_err(|_| io::Error::other("provider profile settings are unavailable"))?;
    match &command.action {
        ProviderCliAction::List => {
            for profile in settings.llm_profiles()?.profiles {
                println!("{}\t{}", profile.id, profile_description(&profile.endpoint));
            }
        }
        ProviderCliAction::Add {
            id,
            protocol,
            base_url,
        } => {
            let id =
                LlmProfileId::new(id.clone()).map_err(|error| invalid_input(error.to_string()))?;
            let endpoint = match protocol {
                ProviderCliProtocol::OpenAiResponses => EndpointConfig::OpenAiResponses {
                    base_url: base_url.clone(),
                },
                ProviderCliProtocol::OpenAiChatCompletions => {
                    EndpointConfig::OpenAiChatCompletions {
                        base_url: base_url.clone(),
                    }
                }
                ProviderCliProtocol::AnthropicMessages => EndpointConfig::AnthropicMessages {
                    base_url: base_url.clone(),
                },
            };
            let profile = LlmProfile::new(id.clone(), id.as_str(), endpoint)
                .map_err(|error| invalid_input(error.to_string()))?;
            settings
                .add_llm_profile(profile)
                .map_err(|error| invalid_input(error.to_string()))?;
            println!(
                "Saved provider profile `{id}`. Set its API key with `bone credentials set {id}`."
            );
        }
    }
    Ok(())
}

fn profile_description(endpoint: &EndpointConfig) -> &'static str {
    match endpoint {
        EndpointConfig::ChatGptSubscription => "chatgpt-subscription",
        EndpointConfig::OpenAiResponses { .. } => "openai-responses",
        EndpointConfig::OpenAiChatCompletions { .. } => "openai-chat-completions",
        EndpointConfig::AnthropicMessages { .. } => "anthropic-messages",
    }
}

fn credential_settings_error(error: SettingsError) -> io::Error {
    match error {
        SettingsError::UnknownLlmProfile(_) => invalid_input("provider profile does not exist"),
        _ => io::Error::other("provider profile settings are unavailable"),
    }
}

fn credential_store_error(_error: ApiKeyCredentialError) -> io::Error {
    io::Error::other("API-key credential storage is unavailable")
}

fn require_credential_terminal() -> io::Result<()> {
    if io::stdin().is_terminal() && io::stderr().is_terminal() {
        Ok(())
    } else {
        Err(invalid_input(
            "credentials set requires an interactive terminal; API keys are never accepted as arguments",
        ))
    }
}

fn prompt_for_api_key() -> io::Result<ApiKey> {
    let mut stderr = io::stderr();
    write!(stderr, "API key: ")?;
    stderr.flush()?;

    let terminal_input = match TerminalInput::enable(&mut stderr) {
        Ok(terminal_input) => terminal_input,
        Err(error) => {
            writeln!(stderr)?;
            return Err(error);
        }
    };
    let input = read_masked_api_key(&mut stderr);
    drop(terminal_input);
    writeln!(stderr)?;
    stderr.flush()?;

    let input = input?;
    ApiKey::new(input).map_err(|_| invalid_input("API key must not be empty"))
}

fn read_masked_api_key(stderr: &mut impl Write) -> io::Result<String> {
    let mut input = String::new();
    loop {
        match event::read()? {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                match key.code {
                    KeyCode::Enter => return Ok(input),
                    KeyCode::Esc => {
                        return Err(io::Error::new(
                            io::ErrorKind::Interrupted,
                            "API-key input cancelled",
                        ));
                    }
                    KeyCode::Char('c' | 'd') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Err(io::Error::new(
                            io::ErrorKind::Interrupted,
                            "API-key input cancelled",
                        ));
                    }
                    KeyCode::Backspace => {
                        if input.pop().is_some() {
                            write!(stderr, "\x08 \x08")?;
                            stderr.flush()?;
                        }
                    }
                    KeyCode::Char(character)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        input.push(character);
                        write!(stderr, "*")?;
                        stderr.flush()?;
                    }
                    _ => {}
                }
            }
            Event::Paste(text) => {
                for character in text
                    .chars()
                    .filter(|character| !matches!(character, '\r' | '\n'))
                {
                    input.push(character);
                    write!(stderr, "*")?;
                }
                stderr.flush()?;
            }
            _ => {}
        }
    }
}

/// Restores both input modes even when reading or validating the key fails.
struct TerminalInput;

impl TerminalInput {
    fn enable(stderr: &mut impl Write) -> io::Result<Self> {
        enable_raw_mode()?;
        if let Err(error) = execute!(stderr, EnableBracketedPaste) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        Ok(Self)
    }
}

impl Drop for TerminalInput {
    fn drop(&mut self) {
        let _ = execute!(io::stderr(), DisableBracketedPaste);
        let _ = disable_raw_mode();
    }
}

fn settings_repair_reason(_error: &SettingsError) -> &'static str {
    // This function is deliberately used only for `SettingsService::open`.
    // At that boundary every variant means BONE could not safely initialize
    // its one durable settings document (store access, decode, or validation),
    // rather than an ordinary runtime/model command failure.
    "The saved BONE settings could not be opened, decoded, or validated safely."
}

fn storage_repair_reason(error: &StoreError) -> &'static str {
    match error {
        StoreError::Busy => "Another BONE process is using the local store.",
        StoreError::Corrupt { .. } => {
            "The local BONE SQLite database may be damaged or is not a SQLite database."
        }
        StoreError::UnsupportedSchema { .. } => {
            "This BONE store uses an unsupported schema version."
        }
        StoreError::UnsafeStorage { .. } => {
            "The local BONE storage location did not pass its privacy and safety checks."
        }
        _ => "BONE could not safely open its local storage.",
    }
}

fn default_store_repair_reason(error: &AppStorageError) -> &'static str {
    match error {
        AppStorageError::Store(error) => storage_repair_reason(error),
        AppStorageError::MissingDefaultDataRoot => {
            "BONE could not determine a safe local data directory."
        }
    }
}

fn workspace_repair_reason(error: &WorkspaceApplicationError) -> Option<&'static str> {
    match error {
        WorkspaceApplicationError::Store(_)
        | WorkspaceApplicationError::AppStorage(_)
        | WorkspaceApplicationError::Registry(_)
        | WorkspaceApplicationError::Sessions(_)
        | WorkspaceApplicationError::Lease(_) => {
            Some("The local Workspace and conversation state could not be opened safely.")
        }
        WorkspaceApplicationError::Workspace(WorkspaceError::Registry(_)) => {
            Some("The local Workspace registry could not be opened safely.")
        }
        WorkspaceApplicationError::ConcurrentDraftOpen => {
            Some("BONE could not safely select a writable saved conversation.")
        }
        WorkspaceApplicationError::Workspace(_) => None,
    }
}

fn tui_storage_repair_reason(error: &TuiError) -> Option<&'static str> {
    match error {
        TuiError::Workspace(error) => workspace_repair_reason(error),
        TuiError::SessionStore(_) => {
            Some("The local conversation data could not be read or updated safely.")
        }
        TuiError::Settings(_) => {
            Some("The saved BONE settings could not be opened, decoded, or validated safely.")
        }
        TuiError::Io(_) | TuiError::Agent(_) | TuiError::Start(_) | TuiError::Input(_) => None,
    }
}

fn show_login(prompt: DeviceCodePrompt) {
    eprintln!(
        "ChatGPT authorization required.\nOpen: {}\nCode: {}\nDo not share this code.\n",
        prompt.verification_uri, prompt.user_code
    );
}

fn report_unresolved(count: usize) {
    if count > 0 {
        eprintln!("{count} job(s) remain unresolved after shutdown.");
    }
}

async fn one_shot(
    agent: &AgentHandle,
    mut observation: Observation,
    input: String,
    display: &TuiDisplaySettings,
) -> Result<(), Box<dyn Error>> {
    let mut record_cursor = observation.snapshot.record_cursor;
    agent.post(input).await?;
    let mut last_error = None;
    loop {
        match observation.events.recv().await {
            Ok(step) if step.sequence == observation.sequence + 1 => {
                observation.sequence = step.sequence;
                if let Some(result) =
                    consume_records(&step.records, &mut record_cursor, &mut last_error, display)
                {
                    return result.map_err(Into::into);
                }
            }
            Ok(_) | Err(RecvError::Lagged(_)) => {
                observation = agent.observe().await?;
                let unseen = observation
                    .snapshot
                    .record
                    .iter()
                    .filter(|entry| entry.cursor > record_cursor)
                    .cloned()
                    .collect::<Vec<_>>();
                if let Some(result) =
                    consume_records(&unseen, &mut record_cursor, &mut last_error, display)
                {
                    return result.map_err(Into::into);
                }
            }
            Err(RecvError::Closed) => {
                return Err(
                    std::io::Error::other("agent closed before completing the request").into(),
                );
            }
        }
    }
}

fn consume_records(
    records: &[RecordEntry],
    cursor: &mut u64,
    last_error: &mut Option<String>,
    display: &TuiDisplaySettings,
) -> Option<Result<(), io::Error>> {
    for entry in records {
        *cursor = entry.cursor;
        let RecordKind::Notice(notice) = &entry.kind else {
            continue;
        };
        show(notice, display);
        match notice {
            Notice::Finished { .. } | Notice::Stopped => return Some(Ok(())),
            Notice::Error { message } => *last_error = Some(message.clone()),
            Notice::Paused => {
                return Some(match last_error.take() {
                    Some(message) => Err(io::Error::other(message)),
                    None => Ok(()),
                });
            }
            _ => {}
        }
    }
    None
}

fn show(notice: &Notice, display: &TuiDisplaySettings) {
    match notice {
        Notice::Reply { text, .. } => println!("\nagent> {text}\n"),
        Notice::JobStarted { id, request } if display.show_progress => {
            let kind = match request {
                JobRequest::Work { .. } => "solver",
                JobRequest::ReviewInput { .. } => "input review",
                JobRequest::Tool(call) => call.name.as_str(),
            };
            eprintln!("[{kind} started · job {}]", id.0);
        }
        Notice::JobProgress { progress, .. } if display.show_progress => {
            eprintln!("[{}]", progress.message);
        }
        Notice::JobFinished { id, .. } if display.show_progress => {
            eprintln!("[job {} finished]", id.0);
        }
        Notice::Error { message } => eprintln!("agent error: {message}"),
        Notice::Paused => eprintln!("[work paused]"),
        Notice::Stopped => eprintln!("[work stopped]"),
        Notice::Finished { cleanup } if !cleanup.is_empty() => {
            eprintln!(
                "[answer complete; cleaning up {} read-only job(s)]",
                cleanup.len()
            );
        }
        _ => {}
    }
}

#[derive(Default)]
struct Arguments {
    help: bool,
    model: Option<String>,
    profile: Option<String>,
    events: Option<PathBuf>,
    message: String,
    credentials: Option<CredentialsCommand>,
    provider: Option<ProviderCliCommand>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CredentialsAction {
    Set,
    Clear,
}

#[derive(Debug, Eq, PartialEq)]
struct CredentialsCommand {
    action: CredentialsAction,
    profile: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProviderCliProtocol {
    OpenAiResponses,
    OpenAiChatCompletions,
    AnthropicMessages,
}

#[derive(Debug, Eq, PartialEq)]
enum ProviderCliAction {
    List,
    Add {
        id: String,
        protocol: ProviderCliProtocol,
        base_url: Option<String>,
    },
}

#[derive(Debug, Eq, PartialEq)]
struct ProviderCliCommand {
    action: ProviderCliAction,
}

impl Arguments {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, std::io::Error> {
        let mut args = args.into_iter().peekable();
        if args
            .peek()
            .is_some_and(|argument| argument == "credentials")
        {
            args.next();
            return Self::parse_credentials(args);
        }
        if args.peek().is_some_and(|argument| argument == "provider") {
            args.next();
            return Self::parse_provider(args);
        }
        let mut parsed = Self::default();
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "-h" | "--help" => {
                    parsed.help = true;
                    break;
                }
                "--model" => {
                    if parsed.model.is_some() {
                        return Err(invalid_input("--model may be provided only once"));
                    }
                    let model = args
                        .next()
                        .ok_or_else(|| invalid_input("--model requires a model ID"))?;
                    if model.trim().is_empty() || model.starts_with('-') {
                        return Err(invalid_input("--model requires a model ID"));
                    }
                    parsed.model = Some(model.trim().to_owned());
                }
                "--profile" => {
                    if parsed.profile.is_some() {
                        return Err(invalid_input("--profile may be provided only once"));
                    }
                    let profile = args
                        .next()
                        .ok_or_else(|| invalid_input("--profile requires a profile ID"))?;
                    if profile.trim().is_empty() || profile.starts_with('-') {
                        return Err(invalid_input("--profile requires a profile ID"));
                    }
                    parsed.profile = Some(profile.trim().to_owned());
                }
                "--events" => {
                    if parsed.events.is_some() {
                        return Err(invalid_input("--events may be provided only once"));
                    }
                    let path = args
                        .next()
                        .ok_or_else(|| invalid_input("--events requires a new output file path"))?;
                    if path.trim().is_empty() || path.starts_with('-') {
                        return Err(invalid_input("--events requires a new output file path"));
                    }
                    parsed.events = Some(PathBuf::from(path));
                }
                "--" => {
                    parsed.message = args.collect::<Vec<_>>().join(" ");
                    break;
                }
                option if option.starts_with('-') => {
                    return Err(invalid_input("unknown option; run `bone --help`"));
                }
                _ => {
                    parsed.message = std::iter::once(argument)
                        .chain(args)
                        .collect::<Vec<_>>()
                        .join(" ");
                    break;
                }
            }
        }
        Ok(parsed)
    }

    fn parse_credentials(mut args: impl Iterator<Item = String>) -> Result<Self, std::io::Error> {
        let Some(action) = args.next() else {
            return Err(invalid_input(
                "credentials requires `set <profile>` or `clear <profile>`",
            ));
        };
        if matches!(action.as_str(), "-h" | "--help") {
            if args.next().is_some() {
                return Err(invalid_input("credentials help does not accept arguments"));
            }
            return Ok(Self {
                help: true,
                ..Self::default()
            });
        }
        let action = match action.as_str() {
            "set" => CredentialsAction::Set,
            "clear" => CredentialsAction::Clear,
            _ => {
                return Err(invalid_input(
                    "credentials requires `set <profile>` or `clear <profile>`",
                ));
            }
        };
        let profile = args
            .next()
            .ok_or_else(|| invalid_input("credentials requires a profile ID"))?;
        if profile.trim().is_empty() || profile.starts_with('-') || args.next().is_some() {
            return Err(invalid_input(
                "credentials accepts only an action and profile; API keys are entered interactively",
            ));
        }
        Ok(Self {
            credentials: Some(CredentialsCommand {
                action,
                profile: profile.trim().to_owned(),
            }),
            ..Self::default()
        })
    }

    fn parse_provider(mut args: impl Iterator<Item = String>) -> Result<Self, std::io::Error> {
        let Some(action) = args.next() else {
            return Err(invalid_input(
                "provider requires `list` or `add <id> <responses|chat|anthropic> [https-url]`",
            ));
        };
        if matches!(action.as_str(), "-h" | "--help") {
            if args.next().is_some() {
                return Err(invalid_input("provider help does not accept arguments"));
            }
            return Ok(Self {
                help: true,
                ..Self::default()
            });
        }
        let command = match action.as_str() {
            "list" if args.next().is_none() => ProviderCliCommand {
                action: ProviderCliAction::List,
            },
            "add" => {
                let id = args.next().ok_or_else(|| {
                    invalid_input(
                        "provider add requires <id> <responses|chat|anthropic> [https-url]",
                    )
                })?;
                let protocol = match args.next().as_deref() {
                    Some("responses") => ProviderCliProtocol::OpenAiResponses,
                    Some("chat") => ProviderCliProtocol::OpenAiChatCompletions,
                    Some("anthropic") => ProviderCliProtocol::AnthropicMessages,
                    _ => {
                        return Err(invalid_input(
                            "provider protocol must be responses, chat, or anthropic",
                        ));
                    }
                };
                let base_url = args.next();
                if args.next().is_some() {
                    return Err(invalid_input(
                        "provider add accepts one optional HTTPS base URL",
                    ));
                }
                ProviderCliCommand {
                    action: ProviderCliAction::Add {
                        id,
                        protocol,
                        base_url,
                    },
                }
            }
            _ => {
                return Err(invalid_input(
                    "provider requires `list` or `add <id> <responses|chat|anthropic> [https-url]`",
                ));
            }
        };
        Ok(Self {
            provider: Some(command),
            ..Self::default()
        })
    }
}

fn invalid_input(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message.into())
}

fn print_help() {
    println!(
        "\
Run BONE in the current launch-directory workspace.

Usage:
  bone                         Start the multi-session terminal workspace
  bone <message>               Complete one request, then shut down
  bone --model <id>            Select the first interactive session's solver from the default profile
  bone --profile <id> --model <id>  Select a saved provider profile and model
  bone --model <id> <message>  Select the one-shot solver
  bone --profile <id> --model <id> <message>  Select a saved provider in one-shot mode
  bone --events <path> <message>  Write one session's live events as JSON Lines
  bone -- <message>            Treat the remaining arguments as task text
  bone provider list            List saved provider profiles
  bone provider add <id> <responses|chat|anthropic> [https-url]
                                Save a non-secret API-key provider profile
  bone credentials set <profile>    Prompt for an API key and save it securely
  bone credentials clear <profile>  Remove a saved API key

Interactive use:
  Start BONE from the directory you intend to work in. That exact directory is
  the Workspace boundary. BONE creates private user-data/configuration state
  automatically; normal users never need to create or edit a configuration file.
  No .bone directory is created in your project.

  First choose a model in the TUI:
    /provider                 list saved profiles
    /provider add <id> <responses|chat|anthropic> [https-url]
    /model [profile] <id> [controls]
                               current conversation
    /model default [profile] <id> [controls]
                               current Workspace default
    /model global [profile] <id> [controls]
                               user-wide default
    /model coordinator [profile] <id> [controls]
                               user-wide coordinator
    /model inherit             remove current conversation override

  Controls: --timeout <seconds>; for OpenAI Responses profiles only,
  --reasoning-effort <none|minimal|low|medium|high|xhigh|max>,
  --reasoning-summary <auto|concise|detailed>, --reasoning-mode pro, and
  --reasoning-context <auto|all_turns|current_turn>.

  A typed one-line /command is local. Pasted or multiline text is always a
  normal model-visible message. Write //text to send slash-prefixed text.

Session and setup commands:
  /help                        List the primary commands
  /status, /workspace          Inspect this session and Workspace
  /new, /sessions, /resume     Create or navigate saved conversations
  /rename <title>, /archive    Organize the current conversation
  /login                       Connect or retry model authorization
  /logout                      Remove the local ChatGPT login cache when unused
  /config, /config doctor      Open settings guidance or inspect storage state

Keyboard:
  Ctrl-N                       Create a conversation
  Ctrl-Left / Ctrl-Right       Move between the session rail and composer
  Up / Down                    Switch while the session rail is focused
  Enter                        Send, or return from the session rail
  Ctrl-J                       Insert a newline
  PageUp / PageDown            Read conversation history
  Esc or /stop                 Stop work; Esc also leaves the session rail
  Ctrl-C or /exit              Shut down and exit

Authentication:
  /login starts the ChatGPT device authorization flow when needed. Keep its code
  private. For a saved API-key profile, first create it with `bone provider add`
  (or `/provider add` in the TUI), then use `bone credentials set <profile>`;
  BONE prompts without echoing the key and stores it in the operating system's
  credential manager, not SQLite. BONE stores its SQLite data in a private
  user-data area and Rig's provider-managed ChatGPT cache in a separate private
  config area.

Configuration application:
  Settings are persisted immediately and model selection follows Session >
  Workspace > User. An Agent runtime pins its complete resolved configuration when it is
  created, so an already attached runtime keeps its model. New conversations and
  future recreated runtimes use the saved selection; per-turn hot switching is
  not claimed by this version.
  --profile and --model override BONE_PROFILE and BONE_MODEL independently;
  a profile always requires a model.

  No BONE configuration file needs to be created or edited. The first launch
  creates the private store automatically; choose a model in the TUI before
  starting work. One-shot mode can use --model or a saved user default.

The Agent exposes read, glob, and grep tools. Run it from the intended workspace;
content read by tools is sent to the model. Input remains available while jobs run.

Event observation:
  In one-shot mode, --events writes a baseline snapshot, then each kernel input,
  new records, and emitted instructions. Model starts reference their input's
  record position.
  Existing files are never overwritten. The log includes session inputs and
  outputs; authentication traffic and model-internal reasoning are not captured.
  A slow consumer cannot block the agent; missed steps appear as gap records."
    );
}

#[cfg(test)]
mod tests {
    use std::{future::Future, pin::Pin, sync::Arc};

    use super::{
        Arguments, CredentialsAction, CredentialsCommand, ProviderCliAction, ProviderCliCommand,
        ProviderCliProtocol, TuiDisplaySettings, TuiError, initial_model_selection, one_shot,
        settings_repair_reason, tui_storage_repair_reason, workspace_repair_reason,
    };
    use bone_agent::{
        Autonomy, JobContext, JobOutcome, ModelInput, ModelPort, Next, Notice, RecordKind, Runtime,
        WorkResult,
    };
    use bone_app::SettingsError;
    use bone_app::{SessionId, SessionStoreError, WorkspaceApplicationError, WorkspaceError};

    struct ClarifyingModel;

    impl ModelPort for ClarifyingModel {
        fn infer(
            &self,
            _input: ModelInput,
            _context: JobContext,
        ) -> Pin<Box<dyn Future<Output = JobOutcome> + Send>> {
            Box::pin(async {
                JobOutcome::work(WorkResult {
                    reply: Some("Which file should I inspect?".into()),
                    autonomy: Autonomy::Pause,
                    next: Next::Wait {
                        reconsider_after: None,
                    },
                    ..Default::default()
                })
            })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn one_shot_displays_the_clarification_before_returning_on_pause() {
        let agent = Runtime::spawn(
            Arc::new(ClarifyingModel),
            vec![],
            Default::default(),
            Default::default(),
        )
        .unwrap();
        let observation = agent.observe().await.unwrap();
        one_shot(
            &agent,
            observation,
            "Inspect the file".into(),
            &TuiDisplaySettings::default(),
        )
        .await
        .unwrap();
        assert!(agent.snapshot().await.unwrap().record.iter().any(|entry| {
            matches!(&entry.kind, RecordKind::Notice(Notice::Reply { text, .. })
                if text == "Which file should I inspect?")
        }));
        agent.shutdown().await.unwrap();
    }

    #[test]
    fn task_text_cannot_change_model_selection_after_the_first_positional_argument() {
        let args = Arguments::parse(
            [
                "--model",
                "chosen-solver",
                "Investigate",
                "--model",
                "task-text-model",
            ]
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(args.model.as_deref(), Some("chosen-solver"));
        assert_eq!(args.message, "Investigate --model task-text-model");
    }

    #[test]
    fn an_explicit_separator_preserves_flags_as_task_text() {
        let args =
            Arguments::parse(["--", "--model", "task-text-model"].map(str::to_owned)).unwrap();
        assert!(args.model.is_none());
        assert_eq!(args.message, "--model task-text-model");
    }

    #[test]
    fn credentials_commands_accept_only_an_action_and_profile() {
        let arguments =
            Arguments::parse(["credentials", "set", "openai"].map(str::to_owned)).unwrap();
        assert_eq!(
            arguments.credentials,
            Some(CredentialsCommand {
                action: CredentialsAction::Set,
                profile: "openai".into(),
            })
        );
        assert!(arguments.message.is_empty());

        let clear =
            Arguments::parse(["credentials", "clear", "anthropic"].map(str::to_owned)).unwrap();
        assert_eq!(
            clear.credentials,
            Some(CredentialsCommand {
                action: CredentialsAction::Clear,
                profile: "anthropic".into(),
            })
        );

        for arguments in [
            vec!["credentials"],
            vec!["credentials", "set"],
            vec!["credentials", "unknown", "openai"],
            vec!["credentials", "set", "openai", "never-an-api-key-argument"],
        ] {
            assert!(Arguments::parse(arguments.into_iter().map(str::to_owned)).is_err());
        }
    }

    #[test]
    fn provider_commands_create_a_headless_profile_setup_path() {
        let list = Arguments::parse(["provider", "list"].map(str::to_owned)).unwrap();
        assert_eq!(
            list.provider,
            Some(ProviderCliCommand {
                action: ProviderCliAction::List,
            })
        );

        let add = Arguments::parse(
            [
                "provider",
                "add",
                "work-openai",
                "responses",
                "https://gateway.example/v1",
            ]
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(
            add.provider,
            Some(ProviderCliCommand {
                action: ProviderCliAction::Add {
                    id: "work-openai".into(),
                    protocol: ProviderCliProtocol::OpenAiResponses,
                    base_url: Some("https://gateway.example/v1".into()),
                },
            })
        );

        for arguments in [
            vec!["provider"],
            vec!["provider", "list", "extra"],
            vec!["provider", "add", "openai"],
            vec!["provider", "add", "openai", "unknown"],
            vec!["provider", "add", "openai", "responses", "one", "two"],
        ] {
            assert!(Arguments::parse(arguments.into_iter().map(str::to_owned)).is_err());
        }
    }

    #[test]
    fn profile_selection_is_explicit_and_defaults_to_chatgpt() {
        let arguments = Arguments::parse(
            ["--profile", "openai", "--model", "gpt-5.4", "Investigate"].map(str::to_owned),
        )
        .unwrap();
        let selection = initial_model_selection(arguments.model, arguments.profile)
            .unwrap()
            .unwrap();
        assert_eq!(selection.profile.as_str(), "openai");
        assert_eq!(selection.model, "gpt-5.4");

        let default = initial_model_selection(Some("gpt-5.4".into()), None)
            .unwrap()
            .unwrap();
        assert_eq!(default.profile.as_str(), "chatgpt");
        assert!(initial_model_selection(None, Some("openai".into())).is_err());
    }

    #[test]
    fn only_the_first_positional_credentials_word_selects_the_credential_utility() {
        let arguments =
            Arguments::parse(["Describe", "credentials", "set", "openai"].map(str::to_owned))
                .unwrap();
        assert!(arguments.credentials.is_none());
        assert_eq!(arguments.message, "Describe credentials set openai");
    }

    #[test]
    fn event_output_is_a_host_option_and_task_text_cannot_replace_it() {
        let args = Arguments::parse(
            [
                "--events",
                "session.jsonl",
                "Investigate",
                "--events",
                "task-text.jsonl",
            ]
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(
            args.events.as_deref(),
            Some(std::path::Path::new("session.jsonl"))
        );
        assert_eq!(args.message, "Investigate --events task-text.jsonl");
        for args in [
            vec!["--events"],
            vec!["--events", "--model"],
            vec!["--events", ""],
            vec!["--events", "first.jsonl", "--events", "second.jsonl"],
        ] {
            assert!(Arguments::parse(args.into_iter().map(str::to_owned)).is_err());
        }
    }

    #[test]
    fn durable_startup_failures_route_to_the_safe_storage_repair_screen() {
        let session_error =
            WorkspaceApplicationError::Sessions(SessionStoreError::NotFound(SessionId::new()));
        assert!(workspace_repair_reason(&session_error).is_some());

        let workspace_error = WorkspaceApplicationError::Workspace(WorkspaceError::NotDirectory {
            path: std::path::PathBuf::from("/not-a-directory"),
        });
        assert!(workspace_repair_reason(&workspace_error).is_none());

        let tui_error = TuiError::SessionStore(SessionStoreError::NotFound(SessionId::new()));
        assert!(tui_storage_repair_reason(&tui_error).is_some());
    }

    #[test]
    fn settings_startup_failures_route_to_the_safe_storage_repair_screen() {
        assert!(
            !settings_repair_reason(&SettingsError::NonPositive {
                field: "soft_deadline_seconds",
            })
            .is_empty()
        );
    }
}
