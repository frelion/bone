use std::{env, error::Error, io};

use bone_adapters::llm::{
    InputItem, InputSource, Request, Response, StreamEvent, ToolCallDelta, ToolDefinition,
    protocol::anthropic_messages,
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

    let api_key = required_env("ANTHROPIC_API_KEY")?;
    let model_id = required_env("BONE_ANTHROPIC_MODEL")?;
    let endpoint = match env::var("ANTHROPIC_BASE_URL") {
        Ok(base_url) if !base_url.trim().is_empty() => {
            anthropic_messages::compatible("anthropic-probe", api_key, base_url)?
        }
        _ => anthropic_messages::official("anthropic-probe", api_key)?,
    };
    let model = endpoint.model(&model_id)?;

    let mut request = Request::new([InputItem::external(InputSource::User, mode.prompt())])
        .instructions("This is a protocol-boundary probe. Follow the user request exactly.");

    if matches!(mode, Mode::Tool) {
        request = request
            .tools([inspect_path_definition()])
            .require_tool("inspect_path");
    }

    println!("endpoint: {}", model.endpoint_id());
    println!("protocol: {:?}", model.protocol());
    println!("model: {model_id}");
    println!("mode: {}", mode.name());
    if matches!(mode, Mode::Tool) {
        println!("note: the probe displays the tool call but does not execute it");
    }

    println!("\n[bone.request]\n{request:#?}");

    let mut stream = model.stream(request).await?;
    let mut completed = None;
    let mut streamed = Streamed::default();

    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => {
                if let Some(response) = print_event(event, &mut streamed)? {
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
    streamed.report(&response);

    Ok(())
}

/// Display state for the live stream. Deltas are shown as they arrive; the
/// terminal response stays the record the probe reports.
#[derive(Default)]
struct Streamed {
    text: String,
    reasoning: String,
    text_deltas: usize,
    reasoning_deltas: usize,
}

impl Streamed {
    fn report(&self, response: &Response) {
        let terminal = response.text().unwrap_or_default();
        println!(
            "[streamed.summary] text_deltas={} reasoning_deltas={} text_bytes={} reasoning_bytes={} matches_aggregated_text={}",
            self.text_deltas,
            self.reasoning_deltas,
            self.text.len(),
            self.reasoning.len(),
            self.text == terminal
        );
        if !self.reasoning.is_empty() {
            println!("[streamed.reasoning] {:?}", self.reasoning);
        }
    }
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
                "Call inspect_path exactly once for /tmp/bone-adapters-probe. Do not invent its result."
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

fn print_event(
    event: StreamEvent,
    streamed: &mut Streamed,
) -> Result<Option<Response>, serde_json::Error> {
    match event {
        StreamEvent::TextDelta(text) => {
            streamed.text_deltas += 1;
            streamed.text.push_str(&text);
            println!("[text.delta] {text:?}");
        }
        StreamEvent::ReasoningDelta(reasoning) => {
            streamed.reasoning_deltas += 1;
            streamed.reasoning.push_str(&reasoning);
            println!("[reasoning.delta] {reasoning:?}");
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
Inspect BONE's Anthropic Messages boundary.

Usage:
  cargo run -p bone-adapters --example anthropic_messages_probe -- [text|tool]

Required environment:
  ANTHROPIC_API_KEY      API key
  BONE_ANTHROPIC_MODEL   Messages-capable model identifier

Optional environment:
  ANTHROPIC_BASE_URL     Anthropic Messages-compatible API root

Streaming:
  Text and reasoning deltas print as they arrive; the terminal response is
  reprinted as the aggregated output with its tool calls.

Modes:
  text   Observe text, reasoning, terminal, and aggregated events (default)
  tool   Force one inspect_path call and display it without executing it

This probe calls /v1/messages. It never falls back to another protocol."
    );
}
