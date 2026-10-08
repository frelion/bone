//! Persistence fault and interruption regressions, using the public runtime.
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::config::{ModelReference, Profile};
use crate::runtime::{Engine, RunOptions};
use crate::state::{Budget, Event, Job, JobState, SessionState};
use crate::store::Store;
use rig_core::completion::{AssistantContent, CompletionResponse, Message, Usage};
use rig_core::message::{CallId, ToolCall, ToolFunction, ToolName};
use rig_core::providers::openai::{OpenAIConfig, Route};
use rig_core::providers::registry::{ProviderConfig, ProviderRef};
use serde_json::{Value, json};

struct Fixture {
    _directory: tempfile::TempDir,
    data: PathBuf,
    workspace: PathBuf,
    session: String,
    job: String,
    call: String,
    tool_key: String,
}

impl Fixture {
    fn new(calls: Vec<ToolCall>, uncertain: bool) -> Self {
        Self::with_placeholder(calls, uncertain, true)
    }

    fn with_placeholder(calls: Vec<ToolCall>, uncertain: bool, record_placeholder: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("data");
        let workspace = directory.path().join("workspace");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let store = Store::open(data.join("sessions.sqlite")).unwrap();
        let mut state = SessionState::new(&workspace);
        let job = Job::new("Regression");
        let job_id = job.id.clone();
        state.focus = Some(job_id.clone());
        state.jobs.insert(job_id.clone(), job);
        store.create_session(&state).unwrap();
        state.revision = 1;
        let mut input = Event::new(
            &state.id,
            "input",
            json!({"message":Message::user("original instruction"),"source":"user"}),
        );
        input.job_id = Some(job_id.clone());
        input.revision = 1;
        input.root_input = Some(input.id.clone());
        let first = calls[0].clone();
        let response = CompletionResponse::new(
            calls.into_iter().map(AssistantContent::ToolCall).collect(),
            Usage::default(),
            "openai",
            json!({}),
        );
        let mut model = Event::new(&state.id, "model_message", json!({"response":response}));
        model.job_id = Some(job_id.clone());
        model.root_input = Some(input.id.clone());
        model.reply_to = Some(input.id.clone());
        model.call_id = Some("model-call".into());
        model.revision = 1;
        let tool_key = format!("{}:{}", model.id, serde_json::to_string(&first.id).unwrap());
        let call = "external-write-call".to_owned();
        let mut events = vec![input.clone(), model.clone()];
        let job = state.jobs.get_mut(&job_id).unwrap();
        job.history = vec![input.id.clone(), model.id.clone()];
        job.state = if uncertain {
            JobState::Paused
        } else {
            JobState::Ready
        };
        if uncertain {
            job.active_input = Some(input.id.clone());
            state.paused = true;
            let mut started = Event::new(
                &state.id,
                "tool_started",
                json!({"tool_key":tool_key,"effect":"write","tool_name":first.function.name}),
            );
            started.job_id = Some(job_id.clone());
            started.root_input = Some(input.id.clone());
            started.reply_to = Some(input.id.clone());
            started.call_id = Some(call.clone());
            started.revision = 1;
            let mut placeholder = Event::new(
                &state.id,
                "tool_result",
                json!({"message":Message::tool_result(first.id.clone(),first.function.name.clone(),"Effects unknown; inspect before deciding the next action."),"tool_key":tool_key,"tool_name":first.function.name,"uncertain":true}),
            );
            placeholder.job_id = Some(job_id.clone());
            placeholder.root_input = Some(input.id.clone());
            placeholder.reply_to = Some(input.id.clone());
            placeholder.call_id = Some(call.clone());
            placeholder.revision = 1;
            if record_placeholder {
                job.history.push(placeholder.id.clone());
            }
            events.push(started);
            if record_placeholder {
                events.push(placeholder);
            }
        } else {
            job.inbox.push_back(input.id.clone());
        }
        state.budgets.insert(input.id.clone(), Budget::new(8, 4));
        store.commit(&state, &events).unwrap();
        Self {
            _directory: directory,
            data,
            workspace,
            session: state.id,
            job: job_id,
            call,
            tool_key,
        }
    }

    fn engine(&self) -> Engine {
        // Every endpoint is loopback; these tests never use real credentials.
        let config = OpenAIConfig::new("")
            .with_base_url("http://127.0.0.1:9/v1")
            .with_route(Route::Responses);
        let profile = Profile {
            model: ModelReference::Registry(
                ProviderRef::configured(ProviderConfig::OpenAi(config), "fixture").unwrap(),
            ),
            credential_env: None,
            reuse_codex_login: false,
            additional_params: None,
            max_tokens: None,
        };
        Engine::open(
            &self.data,
            &self.workspace,
            Some(&self.session),
            profile,
            "fixture".into(),
            RunOptions::default(),
        )
        .unwrap()
    }

    fn store(&self) -> Store {
        Store::open(self.data.join("sessions.sqlite")).unwrap()
    }
}

fn tool(name: &str, id: &str, args: Value) -> ToolCall {
    ToolCall::new(
        CallId::from_wire(id),
        ToolFunction::new(ToolName::new(name).unwrap(), args),
    )
}

fn write_call() -> ToolCall {
    tool(
        "write_file",
        "provider-write",
        json!({"path":"output.txt","content":"never replay","expected_sha256":null}),
    )
}

async fn read_native_request(socket: &mut tokio::net::TcpStream) -> Value {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    let header_end = loop {
        let count = socket.read(&mut buffer).await.unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(position) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let headers = std::str::from_utf8(&bytes[..header_end]).unwrap();
    let length = headers
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .map(|(_, value)| value.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    assert!(length < 2 * 1024 * 1024);
    while bytes.len() < header_end + length {
        let count = socket.read(&mut buffer).await.unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&buffer[..count]);
    }
    serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap()
}

async fn send_native_response(socket: &mut tokio::net::TcpStream, message: Value) {
    use tokio::io::AsyncWriteExt;
    let body=serde_json::to_vec(&json!({"model":"fixture","created_at":"2026-09-30T00:00:00Z","message":message,"done":true,"done_reason":"stop","prompt_eval_count":3,"eval_count":3})).unwrap();
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    socket.write_all(header.as_bytes()).await.unwrap();
    socket.write_all(&body).await.unwrap();
}

fn native_profile(endpoint: String) -> Profile {
    Profile {
        model: ModelReference::Ollama {
            ollama: rig_core::providers::ollama::OllamaConfig::new().with_base_url(endpoint),
            model: "fixture".into(),
        },
        credential_env: None,
        reuse_codex_login: false,
        additional_params: None,
        max_tokens: None,
    }
}

#[tokio::test]
async fn uncertain_result_survives_resume_and_restart_without_replaying_the_attempt() {
    let fixture = Fixture::new(vec![write_call()], true);
    let mut engine = fixture.engine();
    engine.resume().unwrap();
    // The native terminal fact answers the original proposal even after the
    // instruction epoch changes. Resuming never repeats that operation.
    for _ in 0..2 {
        engine.step().await.unwrap();
    }
    assert!(!fixture.workspace.join("output.txt").exists());
    drop(engine);
    let reopened = fixture.engine();
    let records = reopened.events().unwrap();
    assert_eq!(
        records
            .iter()
            .filter(
                |event| event.kind == "tool_started" && event.data["tool_key"] == fixture.tool_key
            )
            .count(),
        1,
    );
    let results: Vec<_> = records
        .iter()
        .filter(|event| {
            event.kind == "tool_result" && event.call_id.as_deref() == Some(&fixture.call)
        })
        .collect();
    assert_eq!(
        results.len(),
        1,
        "the uncertain result was synthesized twice"
    );
    assert_eq!(results[0].data["uncertain"], true);
    let history = crate::context::build_history(
        &reopened.state().jobs[&fixture.job],
        &records
            .into_iter()
            .map(|event| (event.id.clone(), event))
            .collect(),
    )
    .unwrap();
    assert!(
        serde_json::to_string(&history)
            .unwrap()
            .contains("Effects unknown")
    );
}

#[tokio::test]
async fn tool_start_transaction_failure_leaves_no_effect_or_start_record() {
    let fixture = Fixture::new(vec![write_call()], false);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_native_request(&mut socket).await;
        send_native_response(&mut socket,json!({"role":"assistant","content":"","tool_calls":[{"function":{"name":"write_file","arguments":{"path":"output.txt","content":"never replay","expected_sha256":null}}}]})).await;
    });
    let mut engine = Engine::open(
        &fixture.data,
        &fixture.workspace,
        None,
        native_profile(endpoint),
        "fixture".into(),
        RunOptions::default(),
    )
    .unwrap();
    engine.post("Write output.txt", None).unwrap();
    let connection = rusqlite::Connection::open(fixture.data.join("sessions.sqlite")).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_tool_start BEFORE INSERT ON events WHEN json_extract(NEW.payload, '$.kind') = 'tool_started' BEGIN SELECT RAISE(ABORT, 'injected tool start commit failure'); END;").unwrap();
    let mut rejected = false;
    for _ in 0..6 {
        if tokio::time::timeout(std::time::Duration::from_secs(5), engine.step())
            .await
            .unwrap()
            .is_err()
        {
            rejected = true;
            break;
        }
    }
    assert!(
        rejected,
        "tool start did not reach the injected SQLite fault"
    );
    server.await.unwrap();
    assert!(!fixture.workspace.join("output.txt").exists());
    assert!(
        !fixture
            .store()
            .events(&engine.state().id)
            .unwrap()
            .iter()
            .any(|event| event.kind == "tool_started")
    );
}

#[tokio::test]
async fn completed_effect_with_a_failed_result_commit_recovers_once_and_can_continue_normally() {
    let fixture = Fixture::new(vec![write_call()], false);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for message in [
            json!({"role":"assistant","content":"","tool_calls":[{"function":{"name":"write_file","arguments":{"path":"effect.txt","content":"effect survived the failed commit","expected_sha256":null}}}]}),
            json!({"role":"assistant","content":"","tool_calls":[{"function":{"name":"read_file","arguments":{"path":"effect.txt"}}}]}),
            json!({"role":"assistant","content":"","tool_calls":[{"function":{"name":"write_file","arguments":{"path":"checked.txt","content":"Observed the persisted effect","expected_sha256":null}}}]}),
            json!({"role":"assistant","content":"Inspected the persisted effect and completed the followup write.","tool_calls":[]}),
        ] {
            let (mut socket, _) = listener.accept().await.unwrap();
            requests.push(read_native_request(&mut socket).await);
            send_native_response(&mut socket, message).await;
        }
        requests
    });
    let profile = native_profile(endpoint);
    let mut engine = Engine::open(
        &fixture.data,
        &fixture.workspace,
        None,
        profile.clone(),
        "fixture".into(),
        RunOptions::default(),
    )
    .unwrap();
    let input = engine
        .post("Write the effect once, then inspect and record it.", None)
        .unwrap();
    let session = engine.state().id.clone();
    let db = rusqlite::Connection::open(fixture.data.join("sessions.sqlite")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_result BEFORE INSERT ON events WHEN json_extract(NEW.payload,'$.kind')='tool_result' BEGIN SELECT RAISE(ABORT,'injected completion commit failure'); END;").unwrap();
    let mut rejected = false;
    for _ in 0..8 {
        if tokio::time::timeout(std::time::Duration::from_secs(5), engine.step())
            .await
            .unwrap()
            .is_err()
        {
            rejected = true;
            break;
        }
    }
    assert!(
        rejected,
        "the actual completed write did not reach the result-commit fault"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("effect.txt")).unwrap(),
        "effect survived the failed commit"
    );
    assert!(
        engine
            .post("must not act after a failed commit", None)
            .is_err()
    );
    assert!(
        engine
            .events()
            .unwrap()
            .iter()
            .all(|event| event.kind != "tool_result")
    );
    drop(engine);
    db.execute_batch("DROP TRIGGER reject_result").unwrap();
    let mut engine = Engine::open(
        &fixture.data,
        &fixture.workspace,
        Some(&session),
        profile.clone(),
        "fixture".into(),
        RunOptions::default(),
    )
    .unwrap();
    let recovered: Vec<_> = engine
        .events()
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "tool_result")
        .collect();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].data["uncertain"], true);
    let recovered_id = recovered[0].id.clone();
    let recovered_call = recovered[0].call_id.clone();
    engine.resume().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        while engine.result(&input).is_none() {
            engine.step().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(engine.result(&input).unwrap().kind, "delivery");
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("checked.txt")).unwrap(),
        "Observed the persisted effect"
    );
    let records = engine.events().unwrap();
    assert_eq!(
        records
            .iter()
            .filter(|event| event.kind == "tool_result" && event.call_id == recovered_call)
            .count(),
        1
    );
    assert_eq!(
        records
            .iter()
            .filter(|event| event.kind == "tool_started" && event.data["tool_name"] == "write_file")
            .count(),
        2,
        "the original write was replayed instead of inspected"
    );
    assert!(
        records
            .iter()
            .any(|event| event.id == recovered_id && event.data["uncertain"] == true)
    );
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 4);
    assert!(
        requests[2]
            .to_string()
            .contains("effect survived the failed commit"),
        "the resumed Agent did not receive the actual file evidence"
    );
    drop(engine);
    let reopened = Engine::open(
        &fixture.data,
        &fixture.workspace,
        Some(&session),
        profile,
        "fixture".into(),
        RunOptions::default(),
    )
    .unwrap();
    assert_eq!(
        reopened
            .events()
            .unwrap()
            .iter()
            .filter(|event| event.call_id == recovered_call && event.kind == "tool_result")
            .count(),
        1
    );
}

#[tokio::test]
async fn new_input_follows_results_for_every_call_in_the_previous_batch() {
    let fixture = Fixture::new(
        vec![
            tool("read_file", "batch-a", json!({"path":"a.txt"})),
            tool("list_files", "batch-b", json!({})),
        ],
        false,
    );
    let mut engine = fixture.engine();
    let input = engine
        .post("New instruction supersedes the old batch", None)
        .unwrap();
    for _ in 0..5 {
        if engine.state().jobs[&fixture.job].history.contains(&input)
            && engine
                .events()
                .unwrap()
                .into_iter()
                .filter(|event| event.kind == "tool_result")
                .count()
                >= 2
        {
            break;
        }
        engine.step().await.unwrap();
    }
    let job = &engine.state().jobs[&fixture.job];
    let position = job
        .history
        .iter()
        .position(|id| id == &input)
        .expect("new input reached history");
    let records: BTreeMap<_, _> = engine
        .events()
        .unwrap()
        .into_iter()
        .map(|event| (event.id.clone(), event))
        .collect();
    let result_positions: Vec<_> = job
        .history
        .iter()
        .enumerate()
        .filter(|(_, id)| records[id.as_str()].kind == "tool_result")
        .map(|(index, _)| index)
        .collect();
    assert_eq!(result_positions.len(), 2);
    assert!(
        result_positions.iter().all(|index| *index < position),
        "a newer user turn split the previous native tool batch"
    );
    assert!(
        !engine
            .events()
            .unwrap()
            .into_iter()
            .any(|event| event.kind == "tool_started"),
        "superseded tools executed"
    );
}

#[tokio::test]
async fn a_new_user_input_is_admitted_after_the_previous_input_exhausted_its_budget() {
    let fixture = Fixture::new(
        vec![tool("read_file", "old-call", json!({"path":"a.txt"}))],
        false,
    );
    let store = fixture.store();
    let mut state = store.load_session(&fixture.session).unwrap();
    let job = state.jobs.get_mut(&fixture.job).unwrap();
    let old = job.inbox.pop_front().unwrap();
    job.active_input = Some(old.clone());
    job.state = JobState::Paused;
    state.pending_inputs.push_back(old.clone());
    let budget = state.budgets.get_mut(&old).unwrap();
    budget.calls_used = budget.max_calls;
    store.commit(&state, &[]).unwrap();
    let mut engine = fixture.engine();
    let new = engine
        .post("New instruction with its own fresh allowance", None)
        .unwrap();
    for _ in 0..5 {
        if engine.state().jobs[&fixture.job].active_input.as_deref() == Some(&new) {
            break;
        }
        engine.step().await.unwrap();
    }
    assert_eq!(
        engine.state().jobs[&fixture.job].active_input.as_deref(),
        Some(new.as_str()),
        "the exhausted old input blocked admission of the new input"
    );
    assert!(engine.events().unwrap().into_iter().any(|event|event.kind=="model_started"&&event.root_input.as_deref()==Some(&new)), "the new input never received its own model allowance");
}

#[test]
fn crash_recovery_records_one_owned_native_placeholder_for_the_uncertain_operation() {
    let fixture = Fixture::with_placeholder(vec![write_call()], true, false);
    let engine = fixture.engine();
    let results: Vec<_> = engine
        .events()
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "tool_result")
        .collect();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].call_id.as_deref(), Some(fixture.call.as_str()));
    assert_eq!(results[0].data["uncertain"], true);
    assert!(results[0].root_input.is_some());
    assert!(results[0].reply_to.is_some());
    assert!(!fixture.workspace.join("output.txt").exists());
    drop(engine);
    let reopened = fixture.engine();
    assert_eq!(
        reopened
            .events()
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == "tool_result")
            .count(),
        1
    );
    assert!(!fixture.workspace.join("output.txt").exists());
}

#[tokio::test]
async fn resume_during_a_running_native_model_call_preserves_its_revision_and_call() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::oneshot;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let (received_sender, received) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        let header_end = loop {
            let count = socket.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(position) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                break position + 4;
            }
        };
        let headers = std::str::from_utf8(&bytes[..header_end]).unwrap();
        let length = headers
            .lines()
            .find_map(|line| {
                line.split_once(':')
                    .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .map(|(_, value)| value.trim().parse::<usize>().unwrap())
            })
            .unwrap();
        while bytes.len() < header_end + length {
            let count = socket.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&buffer[..count]);
        }
        let body: Value = serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap();
        assert_eq!(body["model"], "fixture");
        received_sender.send(()).unwrap();
        released.await.unwrap();
        let body=serde_json::to_vec(&json!({"model":"fixture","created_at":"2026-09-30T00:00:00Z","message":{"role":"assistant","content":"Finished without interruption.","tool_calls":[]},"done":true,"done_reason":"stop","prompt_eval_count":3,"eval_count":3})).unwrap();
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        socket.write_all(header.as_bytes()).await.unwrap();
        socket.write_all(&body).await.unwrap();
    });
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    // Ollama uses no credential source, and its native HTTP request is held
    // open by the fixture until after the idempotent resume operation.
    let profile = Profile {
        model: ModelReference::Ollama {
            ollama: rig_core::providers::ollama::OllamaConfig::new().with_base_url(endpoint),
            model: "fixture".into(),
        },
        credential_env: None,
        reuse_codex_login: false,
        additional_params: None,
        max_tokens: None,
    };
    let mut engine = Engine::open(
        &directory.path().join("data"),
        &workspace,
        None,
        profile,
        "fixture".into(),
        RunOptions::default(),
    )
    .unwrap();
    let input = engine.post("Complete this instruction", None).unwrap();
    engine.step().await.unwrap();
    engine.step().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), received)
        .await
        .unwrap()
        .unwrap();
    let revision = engine.state().revision;
    let job = engine.state().focus.clone().unwrap();
    let call = engine.state().jobs[&job].current_call.clone().unwrap();
    assert_eq!(engine.state().jobs[&job].state, JobState::Running);
    engine.resume().unwrap();
    assert_eq!(engine.state().revision, revision);
    assert_eq!(
        engine.state().jobs[&job].current_call.as_deref(),
        Some(call.as_str())
    );
    assert!(
        !engine
            .events()
            .unwrap()
            .into_iter()
            .any(|event| event.kind == "model_cancelled")
    );
    release.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), engine.step())
        .await
        .unwrap()
        .unwrap();
    let result = engine.result(&input).unwrap();
    assert_eq!(result.kind, "delivery");
    assert_eq!(
        engine.event_text(result).unwrap(),
        "Finished without interruption."
    );
    assert_eq!(
        engine
            .events()
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == "model_started")
            .count(),
        1
    );
    server.await.unwrap();
}

#[tokio::test]
async fn consecutive_user_inputs_prioritize_the_new_instruction_through_its_tool_workflow() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for message in [
            json!({"role":"assistant","content":"","tool_calls":[{"function":{"name":"read_file","arguments":{"path":"evidence.txt"}}}]}),
            json!({"role":"assistant","content":"New instruction completed.","tool_calls":[]}),
            json!({"role":"assistant","content":"Earlier work continued.","tool_calls":[]}),
        ] {
            let (mut socket, _) = listener.accept().await.unwrap();
            requests.push(read_native_request(&mut socket).await);
            send_native_response(&mut socket, message).await;
        }
        requests
    });
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("evidence.txt"), "actual tool evidence").unwrap();
    let mut engine = Engine::open(
        &directory.path().join("data"),
        &workspace,
        None,
        native_profile(endpoint),
        "fixture".into(),
        RunOptions::default(),
    )
    .unwrap();
    let old = engine
        .post("Earlier work remains worth continuing", None)
        .unwrap();
    let new = engine
        .post(
            "New instruction takes priority; inspect evidence first",
            None,
        )
        .unwrap();
    for _ in 0..12 {
        if engine
            .result(&new)
            .is_some_and(|event| event.kind == "delivery")
        {
            break;
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), engine.step())
            .await
            .unwrap()
            .unwrap();
        assert!(
            engine.result(&old).is_none(),
            "old work delivered before the new instruction finished"
        );
    }
    assert_eq!(
        engine
            .event_text(engine.result(&new).expect("new input must deliver"))
            .unwrap(),
        "New instruction completed."
    );
    assert!(
        engine
            .state()
            .jobs
            .values()
            .any(|job| job.inbox.contains(&old) || job.active_input.as_ref() == Some(&old)),
        "earlier work was discarded instead of retained for continuation"
    );
    let actions: Vec<_> = engine
        .events()
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "tool_started")
        .collect();
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].root_input.as_deref(), Some(new.as_str()));
    for _ in 0..4 {
        engine.step().await.unwrap();
        assert!(
            engine.result(&old).is_none(),
            "delivering the new instruction silently resumed the earlier user request"
        );
    }
    engine.resume().unwrap();
    for _ in 0..6 {
        if engine
            .result(&old)
            .is_some_and(|event| event.kind == "delivery")
        {
            break;
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), engine.step())
            .await
            .unwrap()
            .unwrap();
    }
    assert_eq!(
        engine
            .event_text(engine.result(&old).expect("old input must continue"))
            .unwrap(),
        "Earlier work continued."
    );
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 3);
    let first = serde_json::to_string(&requests[0]).unwrap();
    assert!(
        first.contains(&format!("status=ACTIVE with ID {new}")),
        "the first native request was assigned to the older input"
    );
    assert!(first.contains("New instruction takes priority"));
    assert!(
        serde_json::to_string(&requests[1])
            .unwrap()
            .contains("actual tool evidence")
    );
}
