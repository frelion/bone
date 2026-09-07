use std::{
    env,
    error::Error,
    io::{self, IsTerminal},
    path::PathBuf,
    process::ExitCode,
};

use bone_agent::{
    AgentHandle, AgentHost, JobRequest, Notice, Observation, RecordEntry, RecordKind,
};
use bone_app::{
    AppStorageError, ChatGptCredentials, ModelSelection, SettingsError, SettingsService,
    TuiDisplaySettings, TuiError, WorkspaceApplication, WorkspaceApplicationError, WorkspaceError,
    open_default_store, run_storage_repair, run_workspace, write_events,
};
use bone_llm::service::chatgpt_subscription::{self, DeviceCodePrompt};
use bone_store::StoreError;
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
    let workspace = env::current_dir()?;
    let credentials = ChatGptCredentials::default_for_current_user()?;
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
            match run_workspace(
                application,
                settings,
                credentials.clone(),
                selected_model.clone(),
            )
            .await
            {
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
    let explicit_solver = selected_model
        .map(|model| ModelSelection::new(model, None, None))
        .transpose()?;
    let runtime = settings.resolve_one_shot(explicit_solver)?;
    let auth = credentials.acquire()?;
    let endpoint = chatgpt_subscription::connect("bone-agent", auth, show_login).await?;
    let agent = AgentHost::new(endpoint).start(&workspace, runtime)?;
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
    events: Option<PathBuf>,
    message: String,
}

impl Arguments {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, std::io::Error> {
        let mut parsed = Self::default();
        let mut args = args.into_iter();
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
  bone --model <id>            Select the first interactive session's solver
  bone --model <id> <message>  Select the one-shot solver
  bone --events <path> <message>  Write one session's live events as JSON Lines
  bone -- <message>            Treat the remaining arguments as task text

Interactive use:
  Start BONE from the directory you intend to work in. That exact directory is
  the Workspace boundary. BONE creates private user-data/configuration state
  automatically; normal users never need to create or edit a configuration file.
  No .bone directory is created in your project.

  First choose a model in the TUI:
    /model <id>                current conversation
    /model default <id>        current Workspace default
    /model global <id>         user-wide default
    /model inherit             remove current conversation override

  A typed one-line /command is local. Pasted or multiline text is always a
  normal model-visible message. Write //text to send slash-prefixed text.

Session and setup commands:
  /help                        List the primary commands
  /status, /workspace          Inspect this session and Workspace
  /new, /sessions, /resume     Create or navigate saved conversations
  /rename <title>, /archive    Organize the current conversation
  /login                       Connect or retry model authorization
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
  private. BONE stores its SQLite data in a private user-data area and Rig's
  provider-managed ChatGPT cache in a separate private config area.

Configuration application:
  Settings are persisted immediately and model selection follows Session >
  Workspace > User. An Agent runtime pins its complete resolved configuration when it is
  created, so an already attached runtime keeps its model. New conversations and
  future recreated runtimes use the saved selection; per-turn hot switching is
  not claimed by this version.

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
        Arguments, TuiDisplaySettings, TuiError, one_shot, settings_repair_reason,
        tui_storage_repair_reason, workspace_repair_reason,
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
