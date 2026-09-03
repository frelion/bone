use std::{env, error::Error, io::Write, process::ExitCode};

use bone_agent::{Agent, AgentReply};
use bone_provider::service::chatgpt_subscription;
use bone_tools::ToolEnvironment;
use tokio::io::{AsyncBufReadExt, BufReader};

const INSTRUCTIONS: &str = "\
You are a careful coding agent working in the current workspace. Use actions for \
work that requires investigation or verification. Inspect files with tools instead \
of guessing. Keep the final answer concise and distinguish facts from inference.";

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
    if matches!(env::args().nth(1).as_deref(), Some("-h" | "--help")) {
        print_help();
        return Ok(());
    }

    let model_id = required_env("BONE_MODEL")?;
    let tools = ToolEnvironment::new(env::current_dir()?)?;
    let endpoint = chatgpt_subscription::connect("bone-cli", |prompt| {
        eprintln!(
            "ChatGPT authorization required.\nOpen: {}\nCode: {}\nDo not share this code.\n",
            prompt.verification_uri, prompt.user_code
        );
    })
    .await?;
    let model = endpoint.model(model_id)?;
    let mut agent = Agent::new(model)
        .instructions(INSTRUCTIONS)
        .tool(tools.read())?
        .tool(tools.glob())?
        .tool(tools.grep())?;

    let input = env::args().skip(1).collect::<Vec<_>>().join(" ");
    if !input.is_empty() {
        show(agent.chat(input).await?);
        return Ok(());
    }

    println!("BONE agent · {}", tools.workspace_root().display());
    println!("Type /exit to quit.\n");
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        print!("you> ");
        std::io::stdout().flush()?;
        let Some(line) = lines.next_line().await? else {
            break;
        };
        let input = line.trim();
        if input == "/exit" {
            break;
        }
        if input.is_empty() {
            continue;
        }

        match agent.chat(input).await {
            Ok(reply) => show(reply),
            Err(error) => eprintln!("agent error: {error}"),
        }
    }

    Ok(())
}

fn show(reply: AgentReply) {
    for action in reply.actions() {
        let result = if action.output().is_some() {
            "completed"
        } else {
            "failed"
        };
        eprintln!(
            "[action · {result} · {} turn(s)] {}",
            action.turns().len(),
            action.intent()
        );
    }
    println!("\nagent> {}\n", reply.text());
}

fn required_env(name: &str) -> Result<String, std::io::Error> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value.trim().to_owned()),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("missing {name}; run `bone --help` for setup"),
        )),
    }
}

fn print_help() {
    println!(
        "\
Run the BONE agent in the current workspace.

Usage:
  bone                         Start an interactive conversation
  bone <message>               Send one message and exit

Required environment:
  BONE_MODEL                    Model available to your ChatGPT subscription

Authentication:
  Uses the experimental ChatGPT subscription connector on Unix. First use may
  show a device login URL and code; later runs reuse BONE's independent cache.
  The first-run code is written to stderr; do not redirect it to persistent logs.

The first slice exposes only read, glob, and grep tools. Run it from the intended
workspace; content read by tools is sent to the model."
    );
}
