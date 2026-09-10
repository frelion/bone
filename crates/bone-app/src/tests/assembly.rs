use bone_adapters::llm::{
    protocol::openai_responses::{Reasoning, ReasoningEffort},
    testing::openai_responses_endpoint,
};
use rig_core::{
    providers::openai,
    test_utils::{MockHttpResponse, RecordingHttpClient},
};
use serde_json::Value;

use super::*;

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
async fn provider_assembly_completes_an_input_and_preserves_history_after_reopen() {
    let temporary = tempfile::tempdir().unwrap();
    let data = temporary.path().join("data");
    let workspace_root = temporary.path().join("workspace");
    std::fs::create_dir(&workspace_root).unwrap();

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
    let transports = [
        RecordingHttpClient::default(),
        RecordingHttpClient::new(submission_response(
            "submit_work",
            json!(WorkProposal::new(WorkStep::Finish(Completion::new(
                "offline assembly completed"
            )))),
        )),
    ];
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
        .collect();
    let providers = ProviderConnector::with_endpoints(endpoints);
    let app = App::with_provider_connector(AppOptions::new(&data), providers.clone())
        .await
        .unwrap();
    for profile in &profiles {
        app.save_profile(profile.clone()).await.unwrap();
    }
    let workspace = app.open_workspace(&workspace_root).await.unwrap();
    let session = app
        .create_session(workspace.id, "Offline production assembly")
        .await
        .unwrap();

    // Queue first so the fixed coordinator response uses the actual public ID.
    let receipt = session
        .submit(SubmitInput::new("complete this offline"))
        .await
        .unwrap();
    transports[0].set_response(MockHttpResponse::success(submission_response(
        "submit_coordination",
        json!(KernelDecision::Assign(vec![RouteDelivery {
            inputs: vec![bone_core::InputId(receipt.input.0)],
            target: RouteTarget::New,
            handoff: "offline assembly".into(),
        }])),
    )));

    let selections = [
        (&profiles[0], ReasoningEffort::High),
        (&profiles[1], ReasoningEffort::Low),
    ]
    .map(|(profile, effort)| {
        let mut selection =
            ModelSelection::new(profile.id.clone(), format!("{}-model", profile.id)).unwrap();
        selection.options = Some(ModelOptions::OpenAiResponses {
            reasoning: Reasoning::new().effort(effort),
        });
        selection
    });
    app.update_config(
        ConfigScope::User,
        ConfigChange::Coordinator(Some(selections[0].clone())),
    )
    .await
    .unwrap();
    app.update_config(
        ConfigScope::User,
        ConfigChange::Worker(Some(selections[1].clone())),
    )
    .await
    .unwrap();
    assert_completed(wait_for_input(&session, receipt.input).await);

    for (transport, role, submission, effort) in [
        (&transports[0], "coordinator", "submit_coordination", "high"),
        (&transports[1], "worker", "submit_work", "low"),
    ] {
        let requests = transport.requests();
        assert_eq!(requests.len(), 1, "expected one {role} call");
        let request = &requests[0];
        assert_eq!(request.uri, format!("https://{role}.example/v1/responses"));
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["model"], format!("{role}-model"));
        assert_eq!(body["reasoning"]["effort"], effort);
        assert_eq!(body["tools"][0]["name"], submission);
        assert_eq!(
            body["tool_choice"],
            json!({"type": "function", "name": submission})
        );
        let context = body["input"][0]["content"][0]["text"]
            .as_str()
            .expect("agent context should be sent as text");
        let expected_context = if role == "coordinator" {
            "complete this offline"
        } else {
            "offline assembly"
        };
        assert!(
            context.contains(expected_context),
            "{role} context omitted `{expected_context}`"
        );
    }

    let history = session.history(SessionSeq(0), 256).await.unwrap();
    assert!(!history.has_more);
    assert!(history.items.iter().any(|entry| matches!(
        &entry.event,
        SessionEvent::RuntimeStarted { config, .. }
            if config.coordinator.profile == profiles[0]
                && config.worker.profile == profiles[1]
                && config.coordinator.selection == selections[0]
                && config.worker.selection == selections[1]
    )));
    let session_id = session.id();
    app.shutdown().await.unwrap();
    drop(session);
    drop(app);

    let reopened = App::with_provider_connector(AppOptions::new(&data), providers)
        .await
        .unwrap();
    let session = reopened.session(session_id).await.unwrap();
    let restored = session.history(SessionSeq(0), 256).await.unwrap();
    assert!(!restored.has_more);
    assert!(restored.items.starts_with(&history.items));
    assert_eq!(
        restored
            .items
            .iter()
            .filter(|entry| matches!(
                entry.event,
                SessionEvent::InputFinished { input, outcome: InputOutcome::Completed, .. }
                    if input == receipt.input
            ))
            .count(),
        1
    );

    let next_input = receipt.input.0 + 1;
    transports[0].set_response(MockHttpResponse::success(submission_response(
        "submit_coordination",
        json!(KernelDecision::Assign(vec![RouteDelivery {
            inputs: vec![bone_core::InputId(next_input)],
            target: RouteTarget::New,
            handoff: "continue after durable restart".into(),
        }])),
    )));
    let second = session
        .submit(SubmitInput::new("continue after reopening"))
        .await
        .unwrap();
    assert_eq!(second.input.0, next_input);
    assert_completed(wait_for_input(&session, second.input).await);

    let coordinator_requests = transports[0].requests();
    assert_eq!(coordinator_requests.len(), 2);
    let body: Value = serde_json::from_slice(&coordinator_requests[1].body).unwrap();
    let context = body["input"][0]["content"][0]["text"]
        .as_str()
        .expect("restored coordinator context should be text");
    assert!(context.contains("continue after reopening"));
    assert!(
        context.contains("offline assembly completed"),
        "durable Session memory omitted the prior root outcome"
    );
    assert_eq!(transports[1].requests().len(), 2);
    reopened.shutdown().await.unwrap();
}
