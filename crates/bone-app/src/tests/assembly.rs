use std::{
    collections::VecDeque,
    future::Future,
    sync::{Arc, Mutex},
};

use bone_adapters::llm::{
    protocol::openai_responses::{Reasoning, ReasoningEffort},
    testing::openai_responses_endpoint,
};
use bytes::Bytes;
use rig_core::{
    http_client::{
        self, HttpClientExt, LazyBody, MultipartForm, Request, Response, StreamingResponse,
    },
    providers::openai,
    test_utils::{CapturedHttpRequest, MockStreamingClient},
    wasm_compat::WasmCompatSend,
};
use serde_json::Value;

use super::*;

/// Script one completed Responses turn that calls `name` with `arguments`.
///
/// Streaming is the only model-call mode, so a scripted turn is the SSE frames
/// the protocol consumes: the `response.output_item.done` frame is what
/// reconstructs the tool call, and the terminal `response.completed` frame is
/// what proves the provider ended the turn.
fn submission_stream(name: &str, arguments: Value) -> String {
    let response: Value = serde_json::from_str(&submission_response(name, arguments))
        .expect("the scripted submission is valid JSON");
    let call = response["output"][0].clone();
    format!(
        "data: {}\n\ndata: {}\n\n",
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "sequence_number": 1,
            "item": call,
        }),
        json!({
            "type": "response.completed",
            "sequence_number": 2,
            "response": response,
        }),
    )
}

/// A transport that serves one scripted SSE turn per streaming call and keeps
/// the requests the provider client actually sent.
///
/// Rig's sequenced double answers only the unary path, which no longer exists
/// on BONE's model-call path, so this double replays the streaming wire while
/// preserving the same per-call script and request record the assembly
/// assertions read.
#[derive(Clone, Debug, Default)]
struct SequencedStreamingHttpClient {
    unary: MockStreamingClient,
    requests: Arc<Mutex<Vec<CapturedHttpRequest>>>,
    responses: Arc<Mutex<VecDeque<Bytes>>>,
}

impl SequencedStreamingHttpClient {
    /// Create a client that serves the supplied SSE turns in order.
    fn new(responses: impl IntoIterator<Item = String>) -> Self {
        Self {
            unary: MockStreamingClient::default(),
            requests: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(responses.into_iter().map(Bytes::from).collect())),
        }
    }

    /// Return the requests captured so far.
    fn requests(&self) -> Vec<CapturedHttpRequest> {
        match self.requests.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Return the number of scripted turns that have not been consumed.
    fn remaining_responses(&self) -> usize {
        match self.responses.lock() {
            Ok(guard) => guard.len(),
            Err(poisoned) => poisoned.into_inner().len(),
        }
    }

    fn next_response(&self) -> Option<Bytes> {
        match self.responses.lock() {
            Ok(mut guard) => guard.pop_front(),
            Err(poisoned) => poisoned.into_inner().pop_front(),
        }
    }
}

impl HttpClientExt for SequencedStreamingHttpClient {
    fn send<T, U>(
        &self,
        request: Request<T>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        T: Into<Bytes> + WasmCompatSend,
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        // The model-call path never sends a unary request, so the unscripted
        // double is exactly the rejection a reintroduced unary call deserves.
        self.unary.send(request)
    }

    fn send_multipart<U>(
        &self,
        request: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        self.unary.send_multipart(request)
    }

    fn send_streaming<T>(
        &self,
        request: Request<T>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
    where
        T: Into<Bytes> + WasmCompatSend,
    {
        let (parts, body) = request.into_parts();
        let captured = CapturedHttpRequest {
            uri: parts.uri.to_string(),
            headers: parts.headers.clone(),
            body: body.into(),
        };
        match self.requests.lock() {
            Ok(mut guard) => guard.push(captured),
            Err(poisoned) => poisoned.into_inner().push(captured),
        }
        let response = MockStreamingClient {
            sse_bytes: self.next_response().unwrap_or_default(),
        };
        let body: Vec<u8> = Vec::new();
        // The scripted turn is owned by the returned future: the double
        // borrows it for exactly this one call.
        async move {
            response
                .send_streaming(Request::from_parts(parts, body))
                .await
        }
    }
}

fn submission_response(name: &str, arguments: Value) -> String {
    json!({
        "id": "resp_offline",
        "object": "response",
        "created_at": 0,
        "status": "completed",
        "model": "offline-model",
        "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2},
        "output": [{
            "type": "function_call",
            "id": "fc_offline",
            "call_id": "call_offline",
            "name": name,
            "arguments": arguments.to_string(),
            "status": "completed"
        }],
        "tools": []
    })
    .to_string()
}

#[tokio::test]
async fn conversation_owns_replies_across_greeting_job_and_reopened_followup() {
    let temporary = tempfile::tempdir().unwrap();
    let data = temporary.path().join("data");
    let root = temporary.path().join("workspace");
    std::fs::create_dir(&root).unwrap();
    let reply = |id, text: &str| ConversationStep::Reply {
        inputs: vec![bone_core::InputId(id)],
        text: text.into(),
        outcome: InputOutcome::Completed,
    };
    let mut task = Assignment::new(JobSpec::new(
        "inspect the parser",
        "read-only",
        "report findings",
    ));
    task.inputs = vec![bone_core::InputId(2)];
    task.tools = Some(ToolSelection::ReadOnly);
    let transports = [
        SequencedStreamingHttpClient::new(
            [
                reply(1, "Hello!"),
                ConversationStep::Start(vec![task]),
                reply(2, "The parser is correct."),
                reply(3, "As we found earlier, the parser is correct."),
            ]
            .into_iter()
            .map(|step| submission_stream("submit_conversation", json!({"step": step}))),
        ),
        SequencedStreamingHttpClient::new([submission_stream(
            "submit_work",
            json!(WorkProposal::new(WorkStep::Finish(Completion::new(
                "internal parser findings"
            )))),
        )]),
    ];
    let profiles = ["coordinator", "worker"].map(|name| {
        Profile::new(
            ProfileId::new(name).unwrap(),
            name,
            EndpointConfig::OpenAiResponses {
                base_url: Some(format!("https://{name}.example/v1")),
            },
        )
        .unwrap()
    });
    let endpoints = profiles
        .iter()
        .zip(&transports)
        .map(|(profile, transport)| {
            let client = openai::Client::builder()
                .api_key("offline-test-key")
                .base_url(profile.endpoint.base_url().unwrap())
                .http_client(transport.clone())
                .build()
                .unwrap();
            (
                profile.id.clone(),
                openai_responses_endpoint(profile.id.as_str(), client).unwrap(),
            )
        })
        .collect::<std::collections::HashMap<_, _>>();
    let providers = ProviderConnector::with_endpoints(endpoints.clone());
    let app = App::with_provider_connector(AppOptions::isolated(&data), providers)
        .await
        .unwrap();
    for profile in &profiles {
        app.save_profile(profile.clone()).await.unwrap();
    }
    let workspace = app.open_workspace(&root).await.unwrap();
    let session = app
        .create_session(workspace.id, "Conversation assembly")
        .await
        .unwrap();
    for (index, profile) in profiles.iter().enumerate() {
        let mut model =
            ModelSelection::new(profile.id.clone(), format!("{}-model", profile.id)).unwrap();
        model.options = Some(ModelOptions::OpenAiResponses {
            reasoning: Reasoning::new().effort(if index == 0 {
                ReasoningEffort::High
            } else {
                ReasoningEffort::Low
            }),
        });
        let change = if index == 0 {
            ConfigChange::Coordinator(Some(model))
        } else {
            ConfigChange::Worker(Some(model))
        };
        app.update_config(ConfigScope::User, change).await.unwrap();
    }
    session.reload_config().await.unwrap();
    let greeting = session.submit(SubmitInput::new("Hello")).await.unwrap();
    assert_eq!(greeting.input.0, 1);
    assert_completed(wait_for_input(&session, greeting.input).await);
    assert!(session.snapshot().await.unwrap().jobs.is_empty());
    assert_eq!(transports[0].requests().len(), 1);
    assert!(transports[1].requests().is_empty());
    let task = session
        .submit(SubmitInput::new(
            "Inspect the parser without modifying files",
        ))
        .await
        .unwrap();
    assert_eq!(task.input.0, 2);
    assert_completed(wait_for_input(&session, task.input).await);
    let snapshot = session.snapshot().await.unwrap();
    assert_eq!(snapshot.jobs.len(), 1);
    assert_eq!(
        snapshot.jobs[0].allowed_tools,
        ["glob", "grep", "read", "session_history"]
            .map(str::to_owned)
            .into_iter()
            .collect()
    );
    let history = session.history(SessionSeq(0), 256).await.unwrap();
    let replies = history
        .items
        .iter()
        .filter_map(|entry| match &entry.event {
            SessionEvent::Reply { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(replies, ["Hello!", "The parser is correct."]);
    assert!(history.items.iter().any(|entry| matches!(&entry.event, SessionEvent::JobFinished { summary, .. } if summary == "internal parser findings")));
    for (index, role, submission, effort) in [
        (0, "coordinator", "submit_conversation", "high"),
        (1, "worker", "submit_work", "low"),
    ] {
        let requests = transports[index].requests();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(
            requests[0].uri,
            format!("https://{role}.example/v1/responses")
        );
        assert_eq!(body["reasoning"]["effort"], effort);
        assert_eq!(body["tools"][0]["name"], submission);
    }
    let id = session.id();
    app.shutdown().await.unwrap();
    drop(session);
    drop(app);
    let reopened = App::with_provider_connector(
        AppOptions::isolated(&data),
        ProviderConnector::with_endpoints(endpoints),
    )
    .await
    .unwrap();
    let session = reopened.session(id).await.unwrap();
    let followup = session
        .submit(SubmitInput::new("What did we conclude earlier?"))
        .await
        .unwrap();
    assert_eq!(followup.input.0, 3);
    assert_completed(wait_for_input(&session, followup.input).await);
    assert_eq!(transports[0].requests().len(), 4);
    assert_eq!(transports[1].requests().len(), 1);
    let requests = transports[0].requests();
    let body: Value = serde_json::from_slice(&requests.last().unwrap().body).unwrap();
    let context = body["input"][0]["content"][0]["text"].as_str().unwrap();
    assert!(context.contains("The parser is correct."));
    assert!(context.contains("What did we conclude earlier?"));
    let restored = session.history(SessionSeq(0), 256).await.unwrap();
    assert!(restored.items.starts_with(&history.items));
    assert_eq!(
        restored
            .items
            .iter()
            .filter(|entry| matches!(entry.event, SessionEvent::Reply { .. }))
            .count(),
        3
    );
    assert_eq!(transports[0].remaining_responses(), 0);
    assert_eq!(transports[1].remaining_responses(), 0);
    reopened.shutdown().await.unwrap();
}
