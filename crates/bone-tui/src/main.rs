use std::{
    env,
    error::Error,
    io::{self, IsTerminal},
    path::PathBuf,
    process::ExitCode,
};

use bone_agent::{
    AgentHandle, JobRequest, Notice, Observation, RecordEntry, RecordKind, TaskConfig,
};
use bone_tui::{TuiConfig, write_events};
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

    let config = bone_agent::config_builder()?
        .register::<TuiConfig>()?
        .build(bone_config::default_path()?)?;
    let snapshot = config.snapshot()?;
    let display = snapshot.get::<TuiConfig>()?.unwrap_or_default();
    for section in snapshot.unrecognized_sections() {
        eprintln!("[unrecognized configuration section: {section}]");
    }
    let task = TaskConfig {
        model: match arguments.model {
            Some(model) => Some(model),
            None => match env::var("BONE_MODEL") {
                Ok(model) => Some(model.trim().to_owned()),
                Err(env::VarError::NotPresent) => None,
                Err(_) => return Err(invalid_input("BONE_MODEL must be valid Unicode").into()),
            },
        },
        ..TaskConfig::default()
    };
    let workspace = env::current_dir()?;
    let input = arguments.message;
    if input.is_empty() {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err(invalid_input(
                "interactive mode requires a terminal; pass a message as an argument",
            )
            .into());
        }
        let host = bone_agent::connect(&config, show_login).await?;
        let reports = bone_tui::run(&host, workspace, task, &display).await?;
        report_unresolved(
            reports
                .iter()
                .flat_map(|report| &report.unresolved_jobs)
                .count(),
        );
        return Ok(());
    }

    let agent = bone_agent::start(&config, &workspace, task, show_login).await?;
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

fn show_login(prompt: bone_agent::LoginPrompt) {
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
    display: &TuiConfig,
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
    display: &TuiConfig,
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

fn show(notice: &Notice, display: &TuiConfig) {
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
Run the BONE agent in the current workspace.

Usage:
  bone                         Start the multi-session terminal workspace
  bone <message>               Complete one request, then shut down
  bone --model <id> [message]  Select the solver for this session
  bone --events <path> <message>  Write one session's live events as JSON Lines
  bone -- <message>            Treat the remaining arguments as task text

Interactive commands:
  Ctrl-N                       Create a conversation
  Alt-Up / Alt-Down            Switch conversations
  Enter                        Send the current message
  Ctrl-J                       Insert a newline
  PageUp / PageDown            Read conversation history
  Esc or /stop                 Stop work; keep the conversation open
  Ctrl-C or /exit              Shut down and exit

System configuration:
  Read $XDG_CONFIG_HOME/bone/config.json, or $HOME/.config/bone/config.json.
  BONE_CONFIG may select another absolute system configuration path.
  Create this JSON with model IDs available to your subscription:

  {{
    \"agent.system\": {{
      \"coordinator\": {{\"model\": \"your-coordinator-model\", \"timeout_seconds\": 120}},
      \"default_solver\": {{\"model\": \"your-solver-model\", \"timeout_seconds\": 120}}
    }}
  }}

  Each model accepts optional effort: none, minimal, low, medium, high, xhigh, max.
  Omit effort to use the provider default. Unsupported settings report an error.
  The coordinator is selected only by system configuration. Task input and
  solver selection cannot change it. Agent configuration is fixed per session.
  The solver owns normal work. The coordinator only classifies input received
  while a solver decision is still outstanding; it cannot choose tools or solve.

Solver selection, in priority order:
  --model <id>                 Task/session override
  BONE_MODEL                   Solver override for this invocation
  agent.system.default_solver  System default

  Overrides do not modify the configuration file. Both purposes may use the
  same model. Omitting timeout_seconds uses 120 seconds for that purpose.

Other configuration sections (all optional):
  llm.system      credential_root: absolute OAuth storage directory
  tools.local     workspace tool limits, such as max_read_lines
  tui.display     show_progress: true or false

  Agent and tool settings use a new snapshot when each session starts. Saved
  changes apply to the next session; credential_root applies on the next BONE
  connection. Display settings are read when this frontend starts.
  Unrecognized sections are preserved and reported.

Authentication:
  Uses the experimental ChatGPT subscription connector on Unix. First use may
  show a device login URL and code; later runs reuse BONE's independent cache.
  The first-run code is written to stderr; do not redirect it to persistent logs.

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

    use super::{Arguments, TuiConfig, one_shot};
    use bone_agent::{
        Autonomy, JobContext, JobOutcome, ModelInput, ModelPort, Next, Notice, RecordKind, Runtime,
        WorkResult,
    };

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
            &TuiConfig::default(),
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
}
