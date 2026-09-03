use std::{env, error::Error, io};

use bone_model::{
    StreamedAssistantContent,
    rig::{
        completion::ToolDefinition,
        message::{Message, ToolChoice},
    },
    service::chatgpt_subscription,
};
use futures_util::StreamExt;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let model_id = env::var("BONE_CHATGPT_MODEL").map_err(|_| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "set BONE_CHATGPT_MODEL to a model available to the subscription",
        )
    })?;
    let tool_mode = matches!(env::args().nth(1).as_deref(), Some("tool"));
    println!("first use may require ChatGPT device authorization");
    let endpoint = chatgpt_subscription::connect("chatgpt-subscription", |prompt| {
        println!("Sign in at {}", prompt.verification_uri);
        println!("Enter code: {}", prompt.user_code);
        println!("Do not share this device code.");
    })
    .await?;
    let model = endpoint.model(&model_id)?;
    let mut request = model
        .request(Message::user(if tool_mode {
            "Call inspect_path exactly once for /tmp/bone-model-probe. Do not invent its result."
        } else {
            "Reply with one short greeting."
        }))
        .preamble("Follow the user request exactly.".to_owned());

    if tool_mode {
        request = request
            .tool(ToolDefinition {
                name: "inspect_path".to_owned(),
                description: "Return metadata for one filesystem path.".to_owned(),
                parameters: json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"],
                    "additionalProperties": false
                }),
            })
            .tool_choice(ToolChoice::Specific {
                function_names: vec!["inspect_path".to_owned()],
            });
    }

    println!("endpoint: {}", endpoint.id());
    println!("protocol: {}", endpoint.protocol());
    println!("model: {model_id}");
    println!("mode: {}", if tool_mode { "tool" } else { "text" });

    let mut stream = request.stream().await?;
    while let Some(item) = stream.next().await {
        match item? {
            StreamedAssistantContent::Text(delta) => print!("{}", delta.text),
            StreamedAssistantContent::ToolCall { tool_call, .. } => println!(
                "\n[tool.call] name={} arguments={}",
                tool_call.function.name,
                serde_json::to_string(&tool_call.function.arguments)?
            ),
            StreamedAssistantContent::Final(record) => println!(
                "\n[final] provider={} model={:?} usage={:?}",
                record.provider, record.model, record.usage
            ),
            _ => {}
        }
    }

    if stream.response.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "stream ended without a terminal record",
        )
        .into());
    }
    Ok(())
}
