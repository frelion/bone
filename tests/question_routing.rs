//! User questions are visible and answerable without choosing an internal job.
//! Every model request stays on a scripted localhost endpoint with synthetic auth.
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

use bone::{
    config::{Config, ModelReference, Profile},
    model,
    runtime::{Engine, RunOptions},
    state::Event,
};
use rig_core::{
    completion::Message,
    providers::{
        chatgpt,
        openai::OpenAIConfig,
        registry::{ProviderConfig, ProviderRef},
    },
};
use serde_json::{Value, json};

struct Fixture {
    _directory: tempfile::TempDir,
    data: PathBuf,
    workspace: PathBuf,
    profile: Profile,
    server: Child,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.server.kill();
        let _ = self.server.wait();
    }
}

impl Fixture {
    fn new(turns: Value) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("data");
        let workspace = directory.path().join("workspace");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let script = directory.path().join("responses.json");
        std::fs::write(
            &script,
            serde_json::to_vec(&json!({"turns":turns})).unwrap(),
        )
        .unwrap();
        let mut server = Command::new("python3")
            .arg("-B")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/scripted_responses.py"))
            .arg("--script")
            .arg(script)
            .arg("--requests")
            .arg(directory.path().join("requests.jsonl"))
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(server.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let port: u16 = line.trim().parse().expect("fixture port");
        let native = OpenAIConfig::with_key(&chatgpt::DIALECT, "")
            .with_base_url(format!("http://127.0.0.1:{port}/v1"));
        let profile = Profile {
            model: ModelReference::Registry(
                ProviderRef::configured(ProviderConfig::OpenAi(native), "fixture").unwrap(),
            ),
            credential_env: None,
            reuse_codex_login: false,
            additional_params: None,
            max_tokens: None,
        };
        let auth = model::auth_file(&data, "fixture").unwrap();
        std::fs::create_dir_all(auth.parent().unwrap()).unwrap();
        std::fs::write(
            auth,
            r#"{"access_token":"question-routing-synthetic-token","expires_at":4102444800}"#,
        )
        .unwrap();
        let config = Config {
            default_profile: "fixture".into(),
            profiles: BTreeMap::from([("fixture".into(), profile.clone())]),
        };
        std::fs::write(data.join("config.toml"), toml::to_string(&config).unwrap()).unwrap();
        Self {
            _directory: directory,
            data,
            workspace,
            profile,
            server,
        }
    }

    fn engine(&self) -> Engine {
        Engine::open(
            &self.data,
            &self.workspace,
            None,
            self.profile.clone(),
            "fixture".into(),
            RunOptions {
                max_parallel: 1,
                max_calls: 16,
                ..RunOptions::default()
            },
        )
        .unwrap()
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_bone"));
        command
            .env_remove("BONE_MODEL")
            .env_remove("CHATGPT_ACCESS_TOKEN")
            .env_remove("OPENAI_API_KEY")
            .arg("--data-dir")
            .arg(&self.data)
            .arg("--profile")
            .arg("fixture");
        command
    }
}

fn tool(id: &str, name: &str, arguments: Value) -> Value {
    json!({"type":"function_call","call_id":id,"name":name,"arguments":arguments})
}

fn child_question_turns() -> Value {
    json!([
        {"match_job_title":"Conversation","output":[tool("send-child","job_send",json!({"title":"Child","message":"CHILD_TASK"}))]},
        {"match_job_title":"Conversation","output":[tool("wait-child","job_wait",json!({"input_ids":"$input_ids"}))]},
        {"match_job_title":"Child","output":[tool("ask-format","ask_user",json!({"question":"Which output format should I use?"}))]},
        {"match_job_title":"Child","contains":["ANSWER_JSON","answer_input"],"text":"Answer received."},
        {"match_job_title":"Child","contains":["ANSWER_JSON","CHILD_TASK"],"text":"Child task done."},
        {"match_job_title":"Conversation","contains":["Child task done."],"text":"Root task done."}
    ])
}

async fn drive_until(engine: &mut Engine, mut predicate: impl FnMut(&Engine) -> bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !predicate(engine) {
            engine.step().await.unwrap();
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("scripted question workflow completes within 15 seconds");
}

fn question(engine: &Engine, input: &str) -> Option<Event> {
    engine
        .result(input)
        .filter(|event| event.kind == "question")
        .cloned()
}

#[tokio::test]
async fn child_question_surfaces_and_plain_reply_completes_its_native_call_and_parent_work() {
    let fixture = Fixture::new(child_question_turns());
    let mut engine = fixture.engine();
    let root = engine.post("ROOT_TASK", None).unwrap();
    let root_job = engine.state.focus.clone().unwrap();
    drive_until(&mut engine, |engine| question(engine, &root).is_some()).await;
    let question = question(&engine, &root).unwrap();
    let child_job = question.job_id.as_ref().unwrap();
    assert_ne!(child_job, &root_job);
    assert_eq!(question.root_input.as_deref(), Some(root.as_str()));
    assert_eq!(engine.state.focus.as_ref(), Some(child_job));
    assert!(engine.is_unanswered_question(&question));

    let answer = engine.post("ANSWER_JSON", None).unwrap();
    let answer_event = engine.events().find(|event| event.id == answer).unwrap();
    assert_eq!(answer_event.job_id.as_ref(), Some(child_job));
    assert_eq!(answer_event.reply_to.as_deref(), Some(question.id.as_str()));
    assert_eq!(answer_event.root_input.as_deref(), Some(answer.as_str()));
    assert!(engine.state.budgets.contains_key(&root));
    assert!(engine.state.budgets.contains_key(&answer));
    assert!(!engine.is_unanswered_question(&question));
    assert!(engine.result(&root).is_none());
    let result = engine
        .events()
        .find(|event| {
            event.kind == "tool_result" && event.data["tool_key"] == question.data["tool_key"]
        })
        .unwrap();
    let native: Message = serde_json::from_value(result.data["message"].clone()).unwrap();
    assert!(matches!(native, Message::User { .. }));
    let serialized = serde_json::to_string(&result.data["message"]).unwrap();
    assert!(serialized.contains(&answer));
    assert!(!serialized.contains("ANSWER_JSON"));
    let revision = engine.state.revision;
    assert!(engine.post("duplicate answer", Some(&question.id)).is_err());
    assert_eq!(engine.state.revision, revision);

    drive_until(&mut engine, |engine| {
        engine
            .result(&root)
            .is_some_and(|event| event.kind == "delivery")
    })
    .await;
    assert_eq!(
        engine.event_text(engine.result(&root).unwrap()),
        "Root task done."
    );
    assert_eq!(
        engine.event_text(engine.result(&answer).unwrap()),
        "Answer received."
    );
    assert!(
        !engine
            .events()
            .any(|event| engine.is_unanswered_question(event))
    );
    assert!(engine.events().any(|event| {
        event.kind == "delivery"
            && event.reply_to == question.reply_to
            && engine.event_text(event) == "Child task done."
    }));
}

#[tokio::test]
async fn plain_reply_chooses_latest_unanswered_question_and_leaves_the_other_open() {
    let fixture = Fixture::new(json!([
        {"match_job_title":"Conversation","output":[
            tool("send-a","job_send",json!({"title":"Child A","message":"TASK_A"})),
            tool("send-b","job_send",json!({"title":"Child B","message":"TASK_B"}))
        ]},
        {"match_job_title":"Conversation","output":[tool("wait-all","job_wait",json!({"input_ids":"$input_ids"}))]},
        {"match_job_title":"Child A","output":[tool("ask-a","ask_user",json!({"question":"Question A?"}))]},
        {"match_job_title":"Child B","output":[tool("ask-b","ask_user",json!({"question":"Question B?"}))]}
    ]));
    let mut engine = fixture.engine();
    let root = engine.post("ROOT_TASK", None).unwrap();
    drive_until(&mut engine, |engine| {
        engine
            .events()
            .filter(|event| engine.is_unanswered_question(event))
            .count()
            == 2
    })
    .await;
    let questions: Vec<Event> = engine
        .events()
        .filter(|event| engine.is_unanswered_question(event))
        .cloned()
        .collect();
    let latest = questions.last().unwrap();
    assert_eq!(engine.result(&root).unwrap().id, latest.id);
    let answer = engine.post("answer latest", None).unwrap();
    assert_eq!(
        engine
            .events()
            .find(|event| event.id == answer)
            .unwrap()
            .reply_to
            .as_deref(),
        Some(latest.id.as_str())
    );
    assert!(!engine.is_unanswered_question(latest));
    assert!(engine.is_unanswered_question(&questions[0]));
    assert_eq!(engine.result(&root).unwrap().id, questions[0].id);
    let older_answer = engine.post("answer other", Some(&questions[0].id)).unwrap();
    assert_eq!(
        engine
            .events()
            .find(|event| event.id == older_answer)
            .unwrap()
            .job_id,
        questions[0].job_id
    );
    assert!(
        !engine
            .events()
            .any(|event| engine.is_unanswered_question(event))
    );
}

fn bounded_output(mut command: Command, input: Option<&str>) -> Output {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    if input.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command.spawn().unwrap();
    if let Some(input) = input {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!("CLI timed out: {}", String::from_utf8_lossy(&output.stderr));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn run_json_returns_child_question_id_and_explicit_reply_answers_it() {
    let fixture = Fixture::new(child_question_turns());
    let mut command = fixture.command();
    command
        .arg("run")
        .arg("ROOT_TASK")
        .arg("--workspace")
        .arg(&fixture.workspace)
        .arg("--max-parallel")
        .arg("1")
        .arg("--json");
    let output = bounded_output(command, None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "waiting");
    assert_eq!(result["text"], "Which output format should I use?");
    let question = result["question_id"].as_str().expect("waiting question ID");
    let mut command = fixture.command();
    command
        .arg("run")
        .arg("ANSWER_JSON")
        .arg("--session")
        .arg(result["session_id"].as_str().unwrap())
        .arg("--reply-to")
        .arg(question)
        .arg("--max-parallel")
        .arg("1")
        .arg("--json");
    let output = bounded_output(command, None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(result["text"], "Answer received.");
    assert!(result["question_id"].is_null());
}

#[test]
fn chat_prints_question_from_delegated_job() {
    let fixture = Fixture::new(child_question_turns());
    let mut command = fixture.command();
    command
        .arg("chat")
        .arg("--workspace")
        .arg(&fixture.workspace)
        .arg("--max-parallel")
        .arg("1");
    let output = bounded_output(command, Some("ROOT_TASK\n"));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Which output format should I use?"));
}
