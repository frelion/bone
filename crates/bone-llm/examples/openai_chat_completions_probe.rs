use std::{env, error::Error, io};

use bone_llm::{
    InputItem, InputSource, Request, Response, StreamEvent, ToolCallDelta, ToolChoice,
    ToolDefinition, protocol::openai_chat_completions,
};
use futures_util::StreamExt;
use serde_json::json;

#[derive(Clone, Copy)]
enum Mode {
    Text,
    Tool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let Some(mode) = mode()? else {
        print_help();
        return Ok(());
    };

    let api_key = required_env("OPENAI_API_KEY")?;
    let model_id = required_env("BONE_OPENAI_CHAT_MODEL")?;
    let endpoint = match env::var("OPENAI_BASE_URL") {
        Ok(base_url) if !base_url.trim().is_empty() => {
            openai_chat_completions::compatible("openai-chat-probe", api_key, base_url)?
        }
        _ => openai_chat_completions::official("openai-chat-probe", api_key)?,
    };
    let model = endpoint.model(&model_id)?;

    let mut request = Request::new([InputItem::external(InputSource::User, mode.prompt())])
        .instructions("This is a protocol-boundary probe. Follow the user request exactly.")
        .max_output_tokens(1_024);

    if matches!(mode, Mode::Tool) {
        request = request
            .tools([inspect_path_definition()])
            .tool_choice(ToolChoice::Specific(vec!["inspect_path".to_owned()]));
    }

    println!("endpoint: {}", endpoint.id());
    println!("protocol: {}", endpoint.protocol());
    println!("model: {model_id}");
    println!("mode: {}", mode.name());
    if matches!(mode, Mode::Tool) {
        println!("note: the probe displays the tool call but does not execute it");
    }

    println!("\n[bone.request]\n{request:#?}");

    let mut stream = model.stream(request).await?;
    let mut completed = None;

    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => {
                if let Some(response) = print_event(event)? {
                    completed = Some(response);
                }
            }
            Err(error) => return Err(error.into()),
        }
    }

    let response = completed.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "stream ended without a completed response",
        )
    })?;
    println!("\n[aggregated.output]\n{:#?}", response.items());

    if matches!(mode, Mode::Tool) {
        let tool_calls = response.tool_calls().collect::<Vec<_>>();
        if !matches!(
            tool_calls.as_slice(),
            [tool_call]
                if tool_call.name() == "inspect_path"
                    && tool_call.arguments()["path"] == "/tmp/bone-llm-probe"
        ) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "tool probe expected exactly one inspect_path call for /tmp/bone-llm-probe",
            )
            .into());
        }
    }

    Ok(())
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Tool => "tool",
        }
    }

    fn prompt(self) -> &'static str {
        match self {
            Self::Text => "In one short sentence, explain why the sky appears blue.",
            Self::Tool => {
                "Call inspect_path exactly once for /tmp/bone-llm-probe. Do not invent its result."
            }
        }
    }
}

fn mode() -> Result<Option<Mode>, Box<dyn Error>> {
    match env::args().nth(1).as_deref() {
        None | Some("text") => Ok(Some(Mode::Text)),
        Some("tool") => Ok(Some(Mode::Tool)),
        Some("-h" | "--help") => Ok(None),
        Some(other) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unknown mode {other:?}; expected `text` or `tool`"),
        )
        .into()),
    }
}

fn required_env(name: &str) -> Result<String, io::Error> {
    env::var(name).map_err(|_| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("missing {name}; see `--help` for usage"),
        )
    })
}

fn inspect_path_definition() -> ToolDefinition {
    ToolDefinition::new(
        "inspect_path",
        "Return metadata for one filesystem path.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to inspect"
                }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
    )
}

fn print_event(event: StreamEvent) -> Result<Option<Response>, serde_json::Error> {
    match event {
        StreamEvent::TextDelta(text) => {
            println!("[text.delta] {text:?}");
        }
        StreamEvent::ToolCallDelta { id, delta } => match delta {
            ToolCallDelta::Name(name) => {
                println!("[tool.delta] id={id:?} name={name:?}");
            }
            ToolCallDelta::Arguments(arguments) => {
                println!("[tool.delta] id={id:?} arguments={arguments:?}");
            }
            _ => {}
        },
        StreamEvent::Completed(response) => {
            let origin = response.origin();
            println!(
                "[completed] provider={:?} model={:?} finish_reason={:?} usage={:?}",
                origin.provider(),
                origin.reported_model_id(),
                response.finish_reason(),
                response.usage()
            );
            for call in response.tool_calls() {
                println!(
                    "[tool.call] call_id={:?} name={:?} arguments={}",
                    call.id(),
                    call.name(),
                    serde_json::to_string(call.arguments())?
                );
            }
            return Ok(Some(response));
        }
        _ => {}
    }

    Ok(None)
}

fn print_help() {
    println!(
        "\
Inspect BONE's OpenAI Chat Completions boundary.

Usage:
  cargo run -p bone-llm --example openai_chat_completions_probe -- [text|tool]

Required environment:
  OPENAI_API_KEY            API key shared by the selected OpenAI-compatible service
  BONE_OPENAI_CHAT_MODEL    Chat Completions-capable model identifier

Optional environment:
  OPENAI_BASE_URL           OpenAI Chat Completions-compatible API root

Modes:
  text   Observe text, terminal, and aggregated events (default)
  tool   Force one inspect_path call and display it without executing it

This probe calls /chat/completions. It never falls back to /responses."
    );
}
