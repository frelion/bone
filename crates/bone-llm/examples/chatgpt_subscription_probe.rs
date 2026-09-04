use std::{env, error::Error, io};

use bone_llm::{
    InputItem, InputSource, Request, StreamEvent, ToolChoice, ToolDefinition,
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
    let credential_root = chatgpt_subscription::default_credential_root()?;
    let endpoint =
        chatgpt_subscription::connect("chatgpt-subscription", credential_root, |prompt| {
            println!("Sign in at {}", prompt.verification_uri);
            println!("Enter code: {}", prompt.user_code);
            println!("Do not share this device code.");
        })
        .await?;
    let model = endpoint.model(&model_id)?;
    let mut request = Request::new([InputItem::external(
        InputSource::User,
        if tool_mode {
            "Call inspect_path exactly once for /tmp/bone-llm-probe. Do not invent its result."
        } else {
            "Reply with one short greeting."
        },
    )])
    .instructions("Follow the user request exactly.");

    if tool_mode {
        request = request
            .tools([ToolDefinition::new(
                "inspect_path",
                "Return metadata for one filesystem path.",
                json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"],
                    "additionalProperties": false
                }),
            )])
            .tool_choice(ToolChoice::Specific(vec!["inspect_path".to_owned()]));
    }

    println!("endpoint: {}", endpoint.id());
    println!("protocol: {}", endpoint.protocol());
    println!("model: {model_id}");
    println!("mode: {}", if tool_mode { "tool" } else { "text" });

    let mut stream = model.stream(request).await?;
    let mut completed = None;
    while let Some(item) = stream.next().await {
        match item? {
            StreamEvent::TextDelta(delta) => print!("{delta}"),
            StreamEvent::Completed(response) => {
                println!(
                    "\n[completed] provider={} model={:?} usage={:?}",
                    response.origin().provider(),
                    response.origin().reported_model_id(),
                    response.usage()
                );
                for call in response.tool_calls() {
                    println!(
                        "[tool.call] name={} arguments={}",
                        call.name(),
                        serde_json::to_string(call.arguments())?
                    );
                }
                completed = Some(response);
            }
            _ => {}
        }
    }

    if completed.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "stream ended without a completed response",
        )
        .into());
    }
    Ok(())
}
