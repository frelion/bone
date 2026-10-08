use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use bone::config::{Config, Profile, default_data_dir};
use bone::runtime::{Engine, RunOptions};
use clap::{Args, FromArgMatches, Parser, Subcommand};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};

mod tui;

#[derive(Parser)]
#[command(
    name = "bone",
    version,
    about = "One agent, continuing internal jobs, native Rig models"
)]
struct Cli {
    #[arg(long, global = true, env = "BONE_DATA_DIR")]
    data_dir: Option<PathBuf>,
    #[arg(long, global = true)]
    profile: Option<String>,
    /// Override the model in the selected profile, e.g. chatgpt:gpt-6-luna.
    #[arg(long, global = true, env = "BONE_MODEL")]
    model: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Handle one user input; jobs are created and managed automatically.
    Run {
        prompt: String,
        #[command(flatten)]
        run: RunArgs,
        #[arg(long)]
        reply_to: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Converse while work continues; /stop, /resume and /quit control the session.
    Chat {
        #[command(flatten)]
        run: RunArgs,
    },
    /// Chat in a terminal interface with live, read-only action inspection.
    Tui {
        #[command(flatten)]
        run: RunArgs,
    },
    /// Continue a paused session with its saved context.
    Resume {
        session_id: String,
        #[command(flatten)]
        run: RunArgs,
        #[arg(long)]
        json: bool,
    },
    Sessions {
        #[arg(long)]
        json: bool,
    },
    History {
        session_id: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        after: Option<String>,
    },
    Providers {
        #[arg(long)]
        json: bool,
    },
    /// Describe usable tool schemas without configuration, authentication or execution.
    Tools {
        #[arg(long)]
        read_only: bool,
        #[arg(long)]
        single_job: bool,
        #[arg(long)]
        json: bool,
    },
    /// Sign in to a ChatGPT subscription profile using Rig's device flow.
    Login,
    /// Print a minimal native provider configuration.
    Config,
}

impl Default for Command {
    fn default() -> Self {
        let matches = RunArgs::augment_args(clap::Command::new("bone")).get_matches_from(["bone"]);
        Self::Tui {
            run: RunArgs::from_arg_matches(&matches).expect("valid default run arguments"),
        }
    }
}

#[derive(Args, Clone, Debug, PartialEq, Eq)]
struct RunArgs {
    #[arg(long)]
    workspace: Option<PathBuf>,
    #[arg(long)]
    session: Option<String>,
    #[arg(long, default_value_t = 64)]
    max_calls: u32,
    #[arg(long, default_value_t = 16)]
    max_jobs: u32,
    #[arg(long, default_value_t = 3)]
    max_parallel: usize,
    #[arg(long, default_value_t = 96_000)]
    context_chars: usize,
    #[arg(long)]
    single_job: bool,
    #[arg(long)]
    no_compaction: bool,
    #[arg(long)]
    read_only: bool,
    #[arg(long, default_value_t = 180)]
    model_timeout_seconds: u64,
    #[arg(long, default_value_t = 900)]
    timeout_seconds: u64,
}

impl RunArgs {
    fn options(&self) -> RunOptions {
        RunOptions {
            max_calls: self.max_calls,
            max_jobs: self.max_jobs,
            max_parallel: self.max_parallel,
            context_chars: self.context_chars,
            single_job: self.single_job,
            no_compaction: self.no_compaction,
            read_only: self.read_only,
            model_timeout_seconds: self.model_timeout_seconds,
        }
    }
}

#[tokio::main]
async fn main() {
    if let Err(error) = execute(Cli::parse()).await {
        eprintln!("bone: {error:#}");
        std::process::exit(1);
    }
}

async fn execute(mut cli: Cli) -> Result<()> {
    let command = cli.command.take().unwrap_or_default();
    if let Command::Tools {
        read_only,
        single_job,
        json,
    } = &command
    {
        let definitions = bone::tool_definitions(*single_job, *read_only);
        if *json {
            println!("{}", serde_json::to_string(&definitions)?);
        } else {
            for definition in definitions {
                println!("{}\t{}", definition.name, definition.description);
            }
        }
        return Ok(());
    }
    let data = cli.data_dir.clone().unwrap_or_else(default_data_dir);
    match &command {
        Command::Tools { .. } => unreachable!("metadata command returned before session setup"),
        Command::Providers { json } => {
            let names = bone::providers();
            if *json {
                println!("{}", serde_json::to_string(&names)?);
            } else {
                for name in names {
                    println!("{name}");
                }
            }
        }
        Command::Config => println!("{}", toml::to_string_pretty(&Config::default())?),
        Command::Sessions { json } => {
            let sessions = bone::sessions(&data)?;
            if *json {
                println!("{}", serde_json::to_string(&sessions)?);
            } else {
                for state in sessions {
                    println!(
                        "{}  {}  {}",
                        state.id,
                        if state.paused { "paused" } else { "saved" },
                        state.workspace.display()
                    );
                }
            }
        }
        Command::History {
            session_id,
            json,
            limit,
            after,
        } => {
            if limit.is_some() || after.is_some() {
                let page =
                    bone::history_page(&data, session_id, after.as_deref(), limit.unwrap_or(100))?;
                if *json {
                    println!("{}", serde_json::to_string(&page)?);
                } else {
                    for event in &page.events {
                        println!("{}  {}  {}", event.timestamp, event.kind, event.data);
                    }
                    println!(
                        "next_cursor: {}",
                        page.next_cursor.as_deref().unwrap_or("null")
                    );
                    println!("has_more: {}", page.has_more);
                }
            } else {
                let events = bone::history(&data, session_id)?;
                if *json {
                    println!("{}", serde_json::to_string(&events)?);
                } else {
                    for event in events {
                        println!("{}  {}  {}", event.timestamp, event.kind, event.data);
                    }
                }
            }
        }
        Command::Login => {
            let (name, profile) = select_profile(&cli, &data)?;
            bone::login(&profile, &data, &name).await?;
            println!(
                "{}: {name}",
                if profile.reuse_codex_login {
                    "Using existing Codex login"
                } else {
                    "Signed in"
                }
            );
        }
        Command::Run {
            prompt,
            run,
            reply_to,
            json,
        } => {
            let mut engine = open(&cli, &data, run, run.session.as_deref())?;
            let input = engine.post(prompt, reply_to.as_deref())?;
            run_to_result(&mut engine, &input, *json, run.timeout_seconds).await?;
        }
        Command::Resume {
            session_id,
            run,
            json,
        } => {
            let mut engine = open(&cli, &data, run, Some(session_id))?;
            engine.resume()?;
            let input = resumed_input(&engine)?;
            run_to_result(&mut engine, &input, *json, run.timeout_seconds).await?;
        }
        Command::Chat { run } => {
            let mut engine = open(&cli, &data, run, run.session.as_deref())?;
            chat(&mut engine, run.timeout_seconds).await?;
        }
        Command::Tui { run } => {
            tui::require_terminal()?;
            let (profile_name, profile) = select_profile(&cli, &data)?;
            let settings = tui::Settings {
                profile_name,
                profile,
            };
            let mut engine = open(&cli, &data, run, run.session.as_deref())?;
            tui::run(&mut engine, &data, settings, run.timeout_seconds).await?;
        }
    }
    Ok(())
}

fn select_profile(cli: &Cli, data: &Path) -> Result<(String, Profile)> {
    if let Some(model) = &cli.model
        && !data.join("config.toml").exists()
    {
        return Ok((
            cli.profile.clone().unwrap_or_else(|| "inline".into()),
            Profile::from_model(model)?,
        ));
    }
    let config = Config::load(data)?;
    let name = cli
        .profile
        .clone()
        .unwrap_or_else(|| config.default_profile.clone());
    let mut profile = config.profile(Some(&name))?.clone();
    if let Some(model) = &cli.model {
        profile = profile.with_model(model)?;
    }
    Ok((name, profile))
}

fn resumed_input(engine: &Engine) -> Result<String> {
    engine
        .state()
        .pending_inputs
        .front()
        .or_else(|| {
            engine
                .state()
                .focus
                .as_ref()
                .and_then(|id| engine.state().jobs.get(id))
                .and_then(|job| job.active_input.as_ref().or(job.inbox.front()))
        })
        .cloned()
        .context("session has no pending user input")
}

fn open(cli: &Cli, data: &Path, run: &RunArgs, session: Option<&str>) -> Result<Engine> {
    let (name, profile) = select_profile(cli, data)?;
    let workspace = if let Some(path) = &run.workspace {
        path.clone()
    } else if let Some(id) = session {
        bone::session(data, id)?.workspace
    } else {
        std::env::current_dir()?
    };
    Engine::open(data, &workspace, session, profile, name, run.options())
}

fn report(engine: &Engine, input: &str, status: &str, text: &str, json_output: bool) -> Result<()> {
    if json_output {
        let question_id = engine
            .result(input)
            .filter(|event| engine.is_unanswered_question(event))
            .map(|event| &event.id);
        println!(
            "{}",
            serde_json::to_string(
                &json!({"session_id":engine.state().id,"input_id":input,"status":status,"text":text,"question_id":question_id,"metrics":engine.metrics(input)})
            )?
        );
    } else {
        if !text.is_empty() {
            println!("{text}");
        }
        eprintln!("Session: {} ({status})", engine.state().id);
    }
    Ok(())
}

async fn run_to_result(
    engine: &mut Engine,
    input: &str,
    json_output: bool,
    seconds: u64,
) -> Result<()> {
    let deadline = tokio::time::sleep(Duration::from_secs(seconds.max(1)));
    tokio::pin!(deadline);
    // On resume ignore historical failures/questions until the new execution produces
    // a result. A fresh user input has no previous result.
    let prior = engine.result(input).map(|e| e.id.clone());
    loop {
        if let Some(event) = engine
            .result(input)
            .filter(|e| Some(&e.id) != prior.as_ref())
        {
            let status = match event.kind.as_str() {
                "delivery" => "completed",
                "input_resolved" => match event.data["outcome"].as_str() {
                    Some("completed") => "completed",
                    Some("superseded") => "superseded",
                    _ => "failed",
                },
                "question" => "waiting",
                "input_paused" => "paused",
                _ => "failed",
            };
            let text = engine.event_text(event)?;
            if !engine.is_quiescent() && !engine.state().paused {
                engine.stop()?;
            }
            collect_stopped_tools(engine).await?;
            report(engine, input, status, &text, json_output)?;
            if status == "failed" {
                bail!("input failed; details are recorded in session history");
            }
            return Ok(());
        }
        if engine.state().paused {
            collect_stopped_tools(engine).await?;
            report(engine, input, "paused", "Work is paused.", json_output)?;
            return Ok(());
        }
        tokio::select! {
            events = engine.step() => {
                let events = events?;
                if events.is_empty() && engine.is_quiescent() {
                    report(engine, input, "waiting", "Work is waiting for input.", json_output)?;
                    return Ok(());
                }
                if events.is_empty() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                engine.stop()?;
                collect_stopped_tools(engine).await?;
                report(engine, input, "paused", "Stopped.", json_output)?;
                return Ok(());
            }
            _ = &mut deadline => {
                engine.stop()?;
                collect_stopped_tools(engine).await?;
                report(engine, input, "paused", "Execution time limit reached.", json_output)?;
                bail!("execution time limit reached");
            }
        }
    }
}

// A stop request cancels execution, but its already started tools still own
// their effects. Collect their terminal results before dropping the session.
async fn collect_stopped_tools(engine: &mut Engine) -> Result<()> {
    while !engine.is_quiescent() {
        engine.step().await?;
    }
    Ok(())
}

async fn chat(engine: &mut Engine, seconds: u64) -> Result<()> {
    eprintln!(
        "BONE · Session {}\nType normally. /stop, /resume, /quit",
        engine.state().id
    );
    let interactive = std::io::stdin().is_terminal();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut input_closed = false;
    let duration = Duration::from_secs(seconds.max(1));
    let deadline = tokio::time::sleep(duration);
    tokio::pin!(deadline);
    let mut deadline_armed = !engine.is_quiescent();
    for question in engine.unanswered_questions() {
        println!("{}", engine.event_text(question)?);
    }
    if interactive {
        print!("> ");
        std::io::stdout().flush()?;
    }
    loop {
        if engine.is_quiescent() {
            deadline_armed = false;
        }
        tokio::select! {
            line = lines.next_line(), if !input_closed => {
                match line? {
                    None => {
                        input_closed = true;
                        if engine.is_quiescent() {
                            break;
                        }
                    }
                    Some(line) => match line.trim() {
                        "/quit" => {
                            engine.stop()?;
                            break;
                        }
                        "/stop" => {
                            engine.stop()?;
                            deadline_armed = false;
                            println!("Stopping. /resume to continue.");
                        }
                        "/resume" => {
                            engine.resume()?;
                            deadline.as_mut().reset(tokio::time::Instant::now() + duration);
                            deadline_armed = true;
                        }
                        "" => {}
                        _ => {
                            engine.post(&line, None)?;
                            deadline.as_mut().reset(tokio::time::Instant::now() + duration);
                            deadline_armed = true;
                        }
                    },
                }
            }
            updates = engine.step(), if !engine.is_quiescent() => {
                let updates = updates?;
                if updates.is_empty() && !engine.is_quiescent() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                for event in updates {
                    if matches!(event.kind.as_str(), "delivery" | "question" | "failure" | "input_paused") {
                        // Internal assignment deliveries are consumed by their waiting job.
                        let public = match event.reply_to.as_deref() {
                            Some(id) => engine.read_event(id)?.data["source"] == Value::String("user".into()),
                            None => false,
                        };
                        let visible = if event.kind == "question" {
                            engine.is_unanswered_question(&event)
                        } else {
                            public
                        };
                        if visible {
                            println!("{}", engine.event_text(&event)?);
                            if interactive {
                                print!("> ");
                                std::io::stdout().flush()?;
                            }
                        }
                    }
                }
                if input_closed && engine.is_quiescent() {
                    break;
                }
            }
            _ = &mut deadline, if deadline_armed => {
                engine.stop()?;
                deadline_armed = false;
                println!("Execution time limit reached. Work is paused; /resume to continue.");
                if input_closed {
                    break;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                engine.stop()?;
                deadline_armed = false;
                println!("Stopping. /resume to continue; /quit to exit.");
                if input_closed {
                    break;
                }
            }
        }
    }
    collect_stopped_tools(engine).await?;
    Ok(())
}

#[cfg(test)]
#[path = "../tests/unit/cli.rs"]
mod tests;
