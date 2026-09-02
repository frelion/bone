use std::{env, error::Error, io};

use bone_provider::{
    StreamedAssistantContent,
    protocol::openai_responses::{
        self, Reasoning, ReasoningEffort, ReasoningSummaryLevel, reasoning_params,
    },
    rig::{
        completion::ToolDefinition,
        message::{Message, ToolChoice},
    },
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
    let model_id = required_env("BONE_OPENAI_MODEL")?;
    let endpoint = match env::var("OPENAI_BASE_URL") {
        Ok(base_url) if !base_url.trim().is_empty() => {
            openai_responses::compatible("openai-probe", api_key, base_url)?
        }
        _ => openai_responses::official("openai-probe", api_key)?,
    };
    let model = endpoint.model(&model_id)?;

    let mut request = model
        .request(Message::user(mode.prompt()))
        .preamble("This is a protocol-boundary probe. Follow the user request exactly.".to_owned())
        .max_tokens(1_024)
        .additional_params(reasoning_params(
            Reasoning::new()
                .with_effort(ReasoningEffort::Low)
                .with_summary_level(ReasoningSummaryLevel::Auto),
        ));

    if matches!(mode, Mode::Tool) {
        request = request
            .tool(inspect_path_definition())
            .tool_choice(ToolChoice::Specific {
                function_names: vec!["inspect_path".to_owned()],
            });
    }

    println!("endpoint: {}", endpoint.id());
    println!("protocol: {}", endpoint.protocol());
    println!("model: {model_id}");
    println!("mode: {}", mode.name());
    if matches!(mode, Mode::Tool) {
        println!("note: the probe displays the tool call but does not execute it");
    }

    let request = request.build();
    println!(
        "\n[normalized.request]\n{}",
        serde_json::to_string_pretty(&request)?
    );

    let mut stream = model.stream(request).await?;
    println!("rig.provider: {}\n", stream.provider());

    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => print_event(event)?,
            Err(error) => return Err(error.into()),
        }
    }

    println!(
        "\n[aggregated.choice]\n{}",
        serde_json::to_string_pretty(&stream.choice)?
    );

    if stream.response.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "stream ended without Rig's terminal record",
        )
        .into());
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
                "Call inspect_path exactly once for /tmp/bone-provider-probe. Do not invent its result."
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
    ToolDefinition {
        name: "inspect_path".to_owned(),
        description: "Return metadata for one filesystem path.".to_owned(),
        parameters: json!({
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
    }
}

fn print_event(event: StreamedAssistantContent) -> Result<(), serde_json::Error> {
    match event {
        StreamedAssistantContent::Text(text) => {
            println!("[text.delta] {:?}", text.text);
        }
        StreamedAssistantContent::ReasoningDelta {
            id,
            provider_id,
            reasoning,
        } => {
            println!("[reasoning.delta] id={id:?} provider_id={provider_id:?} text={reasoning:?}");
        }
        StreamedAssistantContent::Reasoning { id, reasoning } => {
            println!(
                "[reasoning.complete] id={id:?} provider_id={:?} text={:?}",
                reasoning.id,
                reasoning.display_text()
            );
        }
        StreamedAssistantContent::ToolCallDelta {
            internal_call_id,
            content,
        } => {
            println!("[tool.delta] internal_id={internal_call_id:?} content={content:?}");
        }
        StreamedAssistantContent::ToolCall {
            tool_call,
            internal_call_id,
        } => {
            println!(
                "[tool.call] internal_id={internal_call_id:?} call_id={:?} name={:?} arguments={}",
                tool_call.id.as_str(),
                tool_call.function.name,
                serde_json::to_string(&tool_call.function.arguments)?
            );
        }
        StreamedAssistantContent::Final(final_record) => {
            println!(
                "[final] provider={:?} model={:?} finish_reason={:?} usage={:?}",
                final_record.provider,
                final_record.model,
                final_record.finish_reason,
                final_record.usage
            );
        }
        StreamedAssistantContent::Unknown(payload) => {
            println!("[unknown] {}", serde_json::to_string(payload.value())?);
        }
    }

    Ok(())
}

fn print_help() {
    println!(
        "\
Inspect Rig's normalized OpenAI Responses boundary.

Usage:
  cargo run -p bone-provider --example openai_responses_probe -- [text|tool]

Required environment:
  OPENAI_API_KEY       API key
  BONE_OPENAI_MODEL    Responses-capable model identifier

Optional environment:
  OPENAI_BASE_URL      OpenAI Responses-compatible API root

Modes:
  text   Observe reasoning, text, terminal, and aggregated events (default)
  tool   Force one inspect_path call and display it without executing it"
    );
}
