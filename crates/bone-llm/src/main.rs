use std::{env, error::Error, io, io::Write, process::ExitCode};

use bone_llm::{
    InputItem, InputSource, Model, Request, StreamEvent, service::chatgpt_subscription,
};
use futures_util::StreamExt;
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("bone-llm: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if matches!(args.first().map(String::as_str), Some("-h" | "--help")) {
        print_help();
        return Ok(());
    }

    let model_id = required_env("BONE_MODEL")?;
    let credential_root = chatgpt_subscription::default_credential_root()?;
    let endpoint = chatgpt_subscription::connect("bone-llm", credential_root, |prompt| {
        eprintln!(
            "ChatGPT authorization required.\nOpen: {}\nCode: {}\nDo not share this code.\n",
            prompt.verification_uri, prompt.user_code
        );
    })
    .await?;
    let model = endpoint.model(&model_id)?;
    let input = args.join(" ");

    if !input.trim().is_empty() {
        let mut history = Vec::new();
        let mut output = io::stdout();
        let turn = complete_turn(&model, &mut history, input, &mut output).await;
        writeln!(output)?;
        turn?;
        return Ok(());
    }

    chat(model).await
}

async fn chat(model: Model) -> Result<(), Box<dyn Error>> {
    let mut history = Vec::new();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut output = io::stdout();

    writeln!(output, "bone-llm · {}", model.id())?;
    writeln!(
        output,
        "Type /clear to clear the conversation or /exit to quit.\n"
    )?;

    loop {
        write!(output, "you> ")?;
        output.flush()?;
        let Some(line) = lines.next_line().await? else {
            break;
        };
        let input = line.trim();

        match input {
            "" => continue,
            "/exit" => break,
            "/clear" => {
                history.clear();
                writeln!(output, "Conversation cleared.\n")?;
            }
            _ => {
                write!(output, "assistant> ")?;
                output.flush()?;
                if let Err(error) =
                    complete_turn(&model, &mut history, input.to_owned(), &mut output).await
                {
                    writeln!(output)?;
                    eprintln!("model error: {error}");
                } else {
                    writeln!(output, "\n")?;
                }
            }
        }
    }

    Ok(())
}

async fn complete_turn<W>(
    model: &Model,
    history: &mut Vec<InputItem>,
    input: String,
    output: &mut W,
) -> Result<(), Box<dyn Error>>
where
    W: Write,
{
    let user = InputItem::external(InputSource::User, input);
    let request = Request::new(history.iter().cloned().chain([user.clone()]));
    let mut stream = model.stream(request).await?;
    let mut completed = None;

    while let Some(event) = stream.next().await {
        match event? {
            StreamEvent::TextDelta(text) => {
                output.write_all(text.as_bytes())?;
                output.flush()?;
            }
            StreamEvent::Completed(response) => completed = Some(response),
            _ => {}
        }
    }

    let response = completed.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "model stream ended without a terminal response",
        )
    })?;
    if response
        .finish_reason()
        .is_some_and(|reason| reason.truncated_output())
    {
        return Err(
            io::Error::new(io::ErrorKind::UnexpectedEof, "model response was truncated").into(),
        );
    }
    if response.items().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "model returned an empty response",
        )
        .into());
    }
    if response.tool_calls().next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "model requested a tool, but bone-llm does not provide tools",
        )
        .into());
    }

    let assistant = response.into_item().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "model response cannot be committed to conversation history",
        )
    })?;
    history.extend([user, assistant]);
    Ok(())
}

fn required_env(name: &str) -> Result<String, io::Error> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value.trim().to_owned()),
        _ => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("missing {name}; run `bone-llm --help` for setup"),
        )),
    }
}

fn print_help() {
    println!(
        "\
Talk directly to a model through BONE's unified model interface.

Usage:
  bone-llm                    Start an interactive conversation
  bone-llm <message>          Send one message and exit

Required environment:
  BONE_MODEL                    Model available to your ChatGPT subscription

Authentication:
  Uses the experimental ChatGPT subscription connector on Unix. First use may
  show a device login URL and code; later runs reuse BONE's independent cache.
  The first-run code is written to stderr; do not redirect it to persistent logs.

Interactive commands:
  /clear                        Clear the in-memory conversation history
  /exit                         Exit bone-llm"
    );
}

#[cfg(all(test, feature = "test-utils"))]
mod tests {
    use bone_llm::{Protocol, testing};
    use rig_core::{
        completion::{FinishReason, Usage},
        message::{AssistantContent, Message},
        streaming::StreamFinal,
        test_utils::{MockCompletionModel, MockStreamEvent},
    };

    use super::*;

    #[tokio::test]
    async fn streams_text_and_replays_only_committed_turns() {
        let mock = MockCompletionModel::from_stream_turns([
            vec![
                MockStreamEvent::message_id("first-id"),
                MockStreamEvent::text("first"),
                MockStreamEvent::final_response_with_default_usage(),
            ],
            vec![
                MockStreamEvent::text("second"),
                MockStreamEvent::final_response_with_default_usage(),
            ],
        ]);
        let model = testing::model(
            "terminal-test",
            Protocol::OpenAiResponses,
            "test-model",
            mock.clone(),
        )
        .unwrap();
        let mut history = Vec::new();
        let mut output = Vec::new();

        complete_turn(&model, &mut history, "hello".to_owned(), &mut output)
            .await
            .unwrap();
        complete_turn(&model, &mut history, "again".to_owned(), &mut output)
            .await
            .unwrap();

        assert_eq!(output, b"firstsecond");
        assert_eq!(history.len(), 4);

        let requests = mock.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].chat_history, vec![Message::user("hello")]);
        assert_eq!(
            requests[1].chat_history,
            vec![
                Message::user("hello"),
                Message::Assistant {
                    id: Some("first-id".to_owned()),
                    content: vec![AssistantContent::text("first")],
                },
                Message::user("again"),
            ]
        );
        assert!(requests.iter().all(|request| request.tools.is_empty()));
        assert!(requests.iter().all(|request| request.preamble.is_none()));
    }

    #[tokio::test]
    async fn stream_error_does_not_commit_partial_turn() {
        let mock = MockCompletionModel::from_stream_turns([[
            MockStreamEvent::text("partial"),
            MockStreamEvent::error("stream failed"),
            MockStreamEvent::text("after"),
            MockStreamEvent::final_response_with_default_usage(),
        ]]);
        let model = testing::model(
            "terminal-test",
            Protocol::OpenAiResponses,
            "test-model",
            mock,
        )
        .unwrap();
        let original = vec![InputItem::external(InputSource::User, "earlier")];
        let mut history = original.clone();
        let mut output = Vec::new();

        let error = complete_turn(&model, &mut history, "new".to_owned(), &mut output)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("stream failed"));
        assert_eq!(output, b"partial");
        assert_eq!(history, original);
    }

    #[tokio::test]
    async fn missing_or_truncated_terminal_does_not_commit_turn() {
        let truncated =
            StreamFinal::new("mock", Usage::new()).with_finish_reason(FinishReason::Length);
        let mock = MockCompletionModel::from_stream_turns([
            vec![MockStreamEvent::text("no terminal")],
            vec![
                MockStreamEvent::text("cut short"),
                MockStreamEvent::FinalResponse(truncated),
            ],
        ]);
        let model = testing::model(
            "terminal-test",
            Protocol::OpenAiResponses,
            "test-model",
            mock,
        )
        .unwrap();
        let mut history = Vec::new();

        let missing = complete_turn(&model, &mut history, "first".to_owned(), &mut Vec::new())
            .await
            .unwrap_err();
        let truncated = complete_turn(&model, &mut history, "second".to_owned(), &mut Vec::new())
            .await
            .unwrap_err();

        assert!(missing.to_string().contains("without a complete response"));
        assert!(truncated.to_string().contains("truncated"));
        assert!(history.is_empty());
    }

    #[tokio::test]
    async fn tool_call_does_not_enter_chat_history() {
        let mock = MockCompletionModel::from_stream_turns([[
            MockStreamEvent::tool_call("call-1", "read", serde_json::json!({})),
            MockStreamEvent::final_response_with_default_usage(),
        ]]);
        let model = testing::model(
            "terminal-test",
            Protocol::OpenAiResponses,
            "test-model",
            mock,
        )
        .unwrap();
        let mut history = Vec::new();

        let error = complete_turn(
            &model,
            &mut history,
            "use a tool".to_owned(),
            &mut Vec::new(),
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("does not provide tools"));
        assert!(history.is_empty());
    }
}
