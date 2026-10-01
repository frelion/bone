use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use bone::config::{Config, Profile, default_data_dir};
use bone::runtime::{Engine, RunOptions};
use clap::{Args, Parser, Subcommand};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};

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
    command: Command,
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
    /// Continue a paused session after inspecting its history.
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
    /// Sign in to a ChatGPT subscription profile using Rig's device flow.
    Login,
    /// Print a minimal native provider configuration.
    Config,
    /// Record an observed outcome for a write interrupted by a crash or cancellation.
    Reconcile {
        session_id: String,
        call_id: String,
        #[arg(long)]
        note: String,
    },
}

#[derive(Args, Clone)]
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

async fn execute(cli: Cli) -> Result<()> {
    let data = cli.data_dir.clone().unwrap_or_else(default_data_dir);
    match &cli.command {
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
        Command::Reconcile {
            session_id,
            call_id,
            note,
        } => {
            let (name, profile) = select_profile(&cli, &data)?;
            let workspace = bone::session(&data, session_id)?.workspace;
            let mut engine = Engine::open(
                &data,
                &workspace,
                Some(session_id),
                profile,
                name,
                RunOptions::default(),
            )?;
            engine.resolve_write(call_id, note)?;
            println!("Recorded the inspected outcome. Resume the session to continue.");
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
            chat(&mut engine).await?;
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
        profile.model = Profile::from_model(model)?.model;
        profile.validate()?;
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
                &json!({"session_id":engine.state().id,"input_id":input,"status":status,"text":text,"question_id":question_id,"metrics":engine.metrics(input),"unknown_writes":engine.state().unknown_writes})
            )?
        );
    } else {
        if !text.is_empty() {
            println!("{text}");
        }
        eprintln!("Session: {} ({status})", engine.state().id);
        for write in engine.state().unknown_writes.values() {
            eprintln!(
                "Unconfirmed write {}: {}. Inspect effects, then use bone reconcile with --note.",
                write.call_id, write.tool_name
            );
        }
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
                "question" => "waiting",
                "input_paused" => "paused",
                _ => "failed",
            };
            let text = engine.event_text(event)?;
            report(engine, input, status, &text, json_output)?;
            if status == "failed" {
                bail!("input failed; details are recorded in session history");
            }
            return Ok(());
        }
        if engine.state().paused {
            report(engine, input, "paused", "Work is paused.", json_output)?;
            return Ok(());
        }
        tokio::select! {
            events=engine.step()=>{
                let events=events?;
                if events.is_empty() && engine.is_quiescent(){
                    let status=if engine.state().unknown_writes.is_empty(){"waiting"}else{"paused"};
                    report(engine,input,status,"Work is waiting for input or reconciliation.",json_output)?;return Ok(());
                }
                if events.is_empty(){tokio::time::sleep(Duration::from_millis(20)).await;}
            }
            _=tokio::signal::ctrl_c()=>{engine.stop()?;report(engine,input,"paused","Stopped.",json_output)?;return Ok(());}
            _=&mut deadline=>{engine.stop()?;report(engine,input,"paused","Execution time limit reached.",json_output)?;bail!("execution time limit reached");}
        }
    }
}

async fn chat(engine: &mut Engine) -> Result<()> {
    eprintln!(
        "BONE · Session {}\nType normally. /stop, /resume, /quit",
        engine.state().id
    );
    let interactive = std::io::stdin().is_terminal();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut input_closed = false;
    for question in engine.unanswered_questions() {
        println!("{}", engine.event_text(question)?);
    }
    if interactive {
        print!("> ");
        std::io::stdout().flush()?;
    }
    loop {
        tokio::select! {
            line=lines.next_line(),if !input_closed=>{
                match line?{
                    None=>{input_closed=true;if engine.is_quiescent(){break;}},
                    Some(line)=>match line.trim(){
                        "/quit"=>{engine.stop()?;break;},
                        "/stop"=>{engine.stop()?;println!("Stopped.");},
                        "/resume"=>{engine.resume()?;},
                        ""=>{},
                        _=>{engine.post(&line,None)?;}
                    }
                }
            }
            updates=engine.step(),if !engine.is_quiescent()=>{
                let updates=updates?;
                if updates.is_empty()&&!engine.is_quiescent(){tokio::time::sleep(Duration::from_millis(20)).await;}
                for event in updates{
                    if matches!(event.kind.as_str(),"delivery"|"question"|"failure"|"input_paused"){
                        // Internal assignment deliveries are consumed by their waiting job.
                        let public=match event.reply_to.as_deref() {
                            Some(id)=>engine.read_event(id)?.data["source"]==Value::String("user".into()),
                            None=>false,
                        };
                        let visible=if event.kind=="question"{engine.is_unanswered_question(&event)}else{public};
                        if visible{println!("{}",engine.event_text(&event)?);if interactive{print!("> ");std::io::stdout().flush()?;}}
                    }
                }
                if input_closed&&engine.is_quiescent(){break;}
            }
            _=tokio::signal::ctrl_c()=>{engine.stop()?;println!("Stopped. /resume to continue; /quit to exit.");if input_closed{break;}}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(select_profile(&cli, directory.path()).is_err());
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
}
