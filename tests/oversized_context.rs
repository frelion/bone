//! A complete native tool batch must remain usable when its results exceed context.
use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

use bone::{
    config::{ModelReference, Profile},
    runtime::{Engine, RunOptions},
};
use rig_core::providers::{
    openai::{OpenAIConfig, Route},
    registry::{ProviderConfig, ProviderRef},
};
use serde_json::{Value, json};

struct LocalServer(Child);
impl Drop for LocalServer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn oversized_parallel_read_results_keep_originals_and_allow_continuation() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let original_a = format!("A_ORIGINAL_HEAD{}A_ORIGINAL_TAIL", "a".repeat(32_000));
    let original_b = format!("B_ORIGINAL_HEAD{}B_ORIGINAL_TAIL", "b".repeat(32_000));
    std::fs::write(workspace.join("a.txt"), &original_a).unwrap();
    std::fs::write(workspace.join("b.txt"), &original_b).unwrap();
    let script = root.path().join("script.json");
    let requests = root.path().join("requests.jsonl");
    std::fs::write(&script, serde_json::to_vec(&json!({
        "turns":[
            {"output":[
                {"type":"function_call","name":"read_file","call_id":"read-a","arguments":{"path":"a.txt","limit":32768}},
                {"type":"function_call","name":"read_file","call_id":"read-b","arguments":{"path":"b.txt","limit":32768}}
            ]},
            {"output":[{"type":"function_call","name":"job_inspect","call_id":"inspect-original","arguments":{"event_id":"$original_event_id","raw":true,"offset":0,"limit":512}}]},
            {"text":"CONTINUED_AFTER_OVERSIZED_BATCH","contains":["next_offset","inspect-original"]}
        ],
        "summary":"Both read_file calls completed. Their previews are incomplete; inspect the original audit event before drawing conclusions."
    })).unwrap()).unwrap();

    // Extend only this temporary server's fixture substitution. Capture actual
    // event IDs from bounded previews/audit notes rather than guessing UUIDs.
    let launcher = root.path().join("server.py");
    std::fs::write(&launcher, r#"
import importlib.util, json, re, sys
spec = importlib.util.spec_from_file_location('long_fixture', sys.argv.pop(1))
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)
expand = fixture.wire.expand_output
original_ids = []
def substitute(output, body):
    def collect(value):
        if isinstance(value, str):
            try: collect(json.loads(value))
            except ValueError:
                if 'Audit event IDs for the preceding original messages' in value:
                    original_ids.extend(re.findall(r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}', value))
        elif isinstance(value, list):
            for part in value: collect(part)
        elif isinstance(value, dict):
            if value.get('truncated') and isinstance(value.get('event_id'), str):
                original_ids.insert(0, value['event_id'])
            for part in value.values(): collect(part)
    collect(body.get('input', []))
    def replace(value):
        if value == '$original_event_id':
            return original_ids[0] if original_ids else 'missing-original-reference'
        if isinstance(value, list): return [replace(part) for part in value]
        if isinstance(value, dict): return {key:replace(part) for key,part in value.items()}
        return value
    return expand(replace(output), body)
fixture.wire.expand_output = substitute
fixture.main()
"#).unwrap();
    let mut server = LocalServer(
        Command::new("python3")
            .arg("-B")
            .arg(launcher)
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/long_task/server.py"))
            .arg("--script")
            .arg(script)
            .arg("--requests")
            .arg(&requests)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let mut port = String::new();
    BufReader::new(server.0.stdout.take().unwrap())
        .read_line(&mut port)
        .unwrap();
    let mut native = OpenAIConfig::new("")
        .with_base_url(format!("http://127.0.0.1:{}/v1", port.trim()))
        .with_route(Route::Responses);
    native.dialect = rig_core::providers::openai::wire::LLAMACPP;
    native.auth = native.dialect.quirks.auth;
    let profile = Profile {
        model: ModelReference::Registry(
            ProviderRef::configured(ProviderConfig::OpenAi(native), "fixture").unwrap(),
        ),
        credential_env: Some(format!(
            "BONE_OVERSIZED_EMPTY_{}",
            uuid::Uuid::new_v4().simple()
        )),
        reuse_codex_login: false,
        additional_params: None,
        max_tokens: None,
    };
    let mut engine = Engine::open(
        &data,
        &workspace,
        None,
        profile,
        "fixture".into(),
        RunOptions {
            context_chars: 32_000,
            max_calls: 8,
            single_job: true,
            read_only: true,
            ..Default::default()
        },
    )
    .unwrap();
    let input = engine
        .post(
            "Read both files using explicit 32 KiB limits; inspect original evidence and continue.",
            None,
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while engine.result(&input).is_none() {
            engine.step().await.unwrap();
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("oversized batch stalled");
    let events = engine.events().unwrap();
    let originals: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "tool_result" && event.data["tool_name"] == "read_file")
        .collect();
    assert_eq!(originals.len(), 2);
    for original in &originals {
        let stored = engine.read_event(&original.id).unwrap();
        let encoded = serde_json::to_string(&stored.data["message"]).unwrap();
        assert!(
            encoded.contains(&original_a) || encoded.contains(&original_b),
            "original result was shortened in audit storage"
        );
    }
    let result = engine.result(&input).unwrap();
    assert_eq!(
        result.kind,
        "delivery",
        "oversized batch failed: {}",
        engine.event_text(result).unwrap()
    );
    assert_eq!(
        engine.event_text(result).unwrap(),
        "CONTINUED_AFTER_OVERSIZED_BATCH"
    );
    let requests = read_requests(&requests);
    let job_id = engine.read_event(&input).unwrap().job_id.unwrap();
    let mut saw_pair = false;
    for request in &requests {
        let items = request["body"]["input"].as_array().unwrap();
        if items
            .iter()
            .any(|item| item["type"] == "function_call_output" && item["call_id"] == "read-a")
        {
            for id in ["read-a", "read-b"] {
                assert!(
                    items
                        .iter()
                        .any(|item| item["type"] == "function_call" && item["call_id"] == id)
                );
                assert!(items.iter().any(|item| item["type"] == "function_call_output" && item["call_id"] == id));
            }
            if request["summary"] == false {
                saw_pair = true;
                assert_work_instructions(&request["body"], &job_id, &input);
                assert!(
                    serde_json::to_string(items)
                        .unwrap()
                        .contains("Read both files using explicit 32 KiB limits")
                );
            }
            assert!(serde_json::to_string(items).unwrap().contains("truncated"));
        }
    }
    assert!(
        saw_pair,
        "native paired batch never reached the model after bounding"
    );
    let inspected = events
        .iter()
        .find(|event| event.kind == "tool_result" && event.data["tool_name"] == "job_inspect")
        .unwrap();
    assert!(
        serde_json::to_string(&inspected.data["message"])
            .unwrap()
            .contains("next_offset")
    );
    let inspect_observation = requests
        .iter()
        .find(|request| {
            request["body"]["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| {
                    item["type"] == "function_call_output" && item["call_id"] == "inspect-original"
                })
        })
        .unwrap();
    assert_eq!(
        inspect_observation["summary"], false,
        "Raw lookup was summarized before the work model could consume it"
    );
}

#[tokio::test]
async fn default_source_pages_reach_work_model_before_summary_and_can_continue() {
    source_pages_probe(
        &["src/lib.rs", "src/store.rs", "src/main.rs", "README.md"],
        false,
    )
    .await;
}

#[tokio::test]
async fn nine_default_source_pages_receive_bounded_work_previews_before_summary() {
    source_pages_probe(
        &[
            "src/lib.rs",
            "src/store.rs",
            "src/main.rs",
            "src/runtime.rs",
            "src/model.rs",
            "src/tools.rs",
            "src/context.rs",
            "src/config.rs",
            "README.md",
        ],
        false,
    )
    .await;
}

#[tokio::test]
async fn old_consumed_history_can_be_summarized_without_covering_fresh_source_pages() {
    source_pages_probe(
        &["src/lib.rs", "src/store.rs", "src/main.rs", "README.md"],
        true,
    )
    .await;
}

async fn source_pages_probe(source_paths: &[&str], preload: bool) {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(workspace.join("src")).unwrap();
    let sources: Vec<_> = source_paths
        .iter()
        .copied()
        .map(|path| {
            let text =
                std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap();
            std::fs::write(workspace.join(path), &text).unwrap();
            (path, text)
        })
        .collect();
    assert!(sources[1].1.len() > 8192);
    let script = root.path().join("script.json");
    let requests = root.path().join("requests.jsonl");
    let mut turns = vec![
        json!({"output":sources.iter().enumerate().map(|(index,(path,_))| json!({"type":"function_call","name":"read_file","call_id":format!("source-{index}"),"arguments":{"path":path}})).collect::<Vec<_>>() }),
        json!({"contains":["source-0","source-1","source-2","source-3"],"output":[{"type":"function_call","name":"read_file","call_id":"store-next","arguments":{"path":"src/store.rs","offset":8192}}]}),
        json!({"contains":["store-next","next_offset"],"text":"SOURCE_PAGES_CONSUMED"}),
    ];
    if preload {
        turns.insert(0,json!({"text":format!("EARLY_REQUIREMENT: signed integer cents. {}", "Earlier consumed work. ".repeat(1500))}));
    }
    if source_paths.len() > 4 {
        turns.truncate(1);
        turns.extend((0..16).map(|_| json!({"output":[{"type":"inspect_or_continue"}]})));
    }
    std::fs::write(&script, serde_json::to_vec(&json!({
        "turns":turns,
        "summary":"EARLY_REQUIREMENT: signed integer cents. Earlier work consumed; source pages for the current task must remain available."
    })).unwrap()).unwrap();
    let server_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/long_task/server.py");
    let mut command = Command::new("python3");
    command.arg("-B");
    if source_paths.len() > 4 {
        let launcher = root.path().join("inspect-server.py");
        std::fs::write(&launcher,r#"
import importlib.util, json, sys
spec=importlib.util.spec_from_file_location('long_fixture',sys.argv.pop(1))
fixture=importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)
expand=fixture.wire.expand_output
state={'target':None,'offset':0,'seen':set(),'followup':False}
def substitute(output,body):
    def collect(value):
        if isinstance(value,str):
            try: collect(json.loads(value))
            except ValueError: pass
        elif isinstance(value,list):
            for part in value: collect(part)
        elif isinstance(value,dict):
            if state['target'] is None and value.get('truncated') and value.get('preview_source_chars',0)>len(value.get('preview','')):
                state['target']=value['event_id']
            if value.get('event_id')==state['target'] and 'raw' in value and 'offset' in value and 'next_offset' in value:
                if value['offset'] not in state['seen']:
                    state['seen'].add(value['offset'])
                    state['offset']=value['next_offset']
            for part in value.values(): collect(part)
    collect(body.get('input',[]))
    if output and output[0].get('type')=='inspect_or_continue':
        if state['offset'] is not None:
            return [{'type':'function_call','name':'job_inspect','call_id':'inspect-page-%s'%state['offset'],'arguments':{'event_id':state['target'],'offset':state['offset'],'limit':4000}}]
        if not state['followup']:
            state['followup']=True
            return [{'type':'function_call','name':'read_file','call_id':'store-next','arguments':{'path':'src/store.rs','offset':8192}}]
        return [{'type':'message','role':'assistant','content':[{'type':'output_text','text':'SOURCE_PAGES_CONSUMED','annotations':[]}]}]
    return expand(output,body)
fixture.wire.expand_output=substitute
fixture.main()
"#).unwrap();
        command.arg(launcher).arg(server_path);
    } else {
        command.arg(server_path);
    }
    let mut server = LocalServer(
        command
            .arg("--script")
            .arg(script)
            .arg("--requests")
            .arg(&requests)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let mut port = String::new();
    BufReader::new(server.0.stdout.take().unwrap())
        .read_line(&mut port)
        .unwrap();
    let mut native = OpenAIConfig::new("")
        .with_base_url(format!("http://127.0.0.1:{}/v1", port.trim()))
        .with_route(Route::Responses);
    native.dialect = rig_core::providers::openai::wire::LLAMACPP;
    native.auth = native.dialect.quirks.auth;
    let profile = Profile {
        model: ModelReference::Registry(
            ProviderRef::configured(ProviderConfig::OpenAi(native), "fixture").unwrap(),
        ),
        credential_env: Some(format!(
            "BONE_SOURCE_PAGES_EMPTY_{}",
            uuid::Uuid::new_v4().simple()
        )),
        reuse_codex_login: false,
        additional_params: None,
        max_tokens: None,
    };
    let mut engine = Engine::open(
        &root.path().join("data"),
        &workspace,
        None,
        profile,
        "fixture".into(),
        RunOptions {
            context_chars: 64_000,
            max_calls: 20,
            ..Default::default()
        },
    )
    .unwrap();
    if preload {
        let old = engine
            .post(
                "Remember signed integer cents for the continuing implementation.",
                None,
            )
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while engine.result(&old).is_none() {
                engine.step().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(engine.result(&old).unwrap().kind, "delivery");
    }
    let input = engine
        .post(
            "Inspect the four source files and follow file-page cursors before implementing.",
            None,
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while engine.result(&input).is_none() {
            engine.step().await.unwrap();
        }
    })
    .await
    .expect("source page batch stalled");
    let result = engine.result(&input).unwrap();
    assert_eq!(
        result.kind,
        "delivery",
        "{}",
        engine.event_text(result).unwrap()
    );
    let requests = read_requests(&requests);
    let first_continuation = requests
        .iter()
        .find(|request| {
            request["body"]["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["type"] == "function_call_output" && item["call_id"] == "source-1")
        })
        .unwrap();
    assert_eq!(
        first_continuation["summary"], false,
        "Fresh source pages were summarized before the work model could inspect them"
    );
    let items = first_continuation["body"]["input"].as_array().unwrap();
    let job_id = engine.read_event(&input).unwrap().job_id.unwrap();
    assert_work_instructions(&first_continuation["body"], &job_id, &input);
    assert!(
        serde_json::to_string(items)
            .unwrap()
            .contains("Inspect the four source files and follow file-page cursors")
    );
    for (index, (_, original)) in sources.iter().enumerate() {
        let id = format!("source-{index}");
        assert!(
            items
                .iter()
                .any(|item| item["type"] == "function_call" && item["call_id"] == id)
        );
        let result = items
            .iter()
            .find(|item| item["type"] == "function_call_output" && item["call_id"] == id)
            .unwrap();
        let page: Value = serde_json::from_str(result["output"].as_str().unwrap()).unwrap();
        let mut end = original.len().min(8192);
        while !original.is_char_boundary(end) {
            end -= 1;
        }
        if source_paths.len() > 4 && page["truncated"] == true {
            let event_id = page["event_id"].as_str().unwrap();
            let audit = engine.read_event(event_id).unwrap();
            assert_eq!(tool_payload(&audit)["text"], original[..end]);
            assert!(page["preview"].as_str().unwrap().contains("sha256"));
            assert!(
                page.get("total_chars").is_none(),
                "Preview length was confused with the inspect cursor extent"
            );
            assert!(page["instruction"].as_str().unwrap().contains("offset=0"));
        } else {
            assert_eq!(page["text"], original[..end]);
            assert_eq!(
                page["next_offset"],
                if end < original.len() {
                    json!(end)
                } else {
                    Value::Null
                }
            );
        }
    }
    let last = requests
        .iter()
        .rev()
        .find(|request| {
            request["body"]["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| {
                    item["type"] == "function_call_output" && item["call_id"] == "store-next"
                })
        })
        .unwrap();
    assert_eq!(last["summary"], false);
    if preload {
        let events = engine.events().unwrap();
        let summary_requests: Vec<_> = requests
            .iter()
            .filter(|request| request["summary"] == true)
            .collect();
        assert!(!summary_requests.is_empty());
        for summary in summary_requests {
            let body = &summary["body"];
            assert!(
                serde_json::to_string(body).unwrap().chars().count() <= 64_000,
                "complete native wire summary request exceeds the context cap"
            );
            assert!(body["tools"].as_array().is_none_or(Vec::is_empty));
            assert!(
                body["instructions"]
                    .as_str()
                    .unwrap()
                    .starts_with("Summarize this job")
            );
            let items = body["input"].as_array().unwrap();
            let task = items.last().expect("summary must end with a native task");
            assert_eq!(task["role"], "user");
            let task_text = task["content"][0]["text"].as_str().unwrap();
            assert!(!task_text.is_empty());
            assert!(
                !task_text.contains("[BONE INPUT id="),
                "summary task must not impersonate a posted user input"
            );
            assert!(
                events.iter().all(|event| !task_text.contains(&event.id)),
                "the last message must be the new task, not the source lookup index"
            );
            assert!(
                items[..items.len() - 1].iter().any(|item| {
                    serde_json::to_string(item)
                        .unwrap()
                        .contains("signed integer cents")
                }),
                "original consumed material must precede the task"
            );
        }
        assert_eq!(
            events.iter().filter(|event| event.kind == "input").count(),
            2,
            "ephemeral summary task must not create a durable input"
        );
        assert!(
            serde_json::to_string(&last["body"])
                .unwrap()
                .contains("signed integer cents")
        );
    }
    if source_paths.len() > 4 {
        let events = engine.events().unwrap();
        let inspected: Vec<_> = events
            .iter()
            .filter(|event| event.kind == "tool_result" && event.data["tool_name"] == "job_inspect")
            .map(tool_payload)
            .collect();
        assert!(
            inspected.len() > 1,
            "Probe did not retrieve an omitted page tail"
        );
        let target = inspected[0]["event_id"].as_str().unwrap().to_owned();
        assert_eq!(inspected[0]["offset"], 0);
        let original =
            serde_json::to_string(&tool_payload(&engine.read_event(&target).unwrap())).unwrap();
        let mut recovered = String::new();
        let mut next = Some(0_usize);
        for page in &inspected {
            assert_eq!(page["event_id"], target);
            assert_eq!(page["offset"].as_u64().unwrap() as usize, next.unwrap());
            recovered.push_str(page["text"].as_str().unwrap());
            next = page["next_offset"].as_u64().map(|value| value as usize);
        }
        assert!(next.is_none());
        assert_eq!(
            recovered, original,
            "The omitted source page tail was not recovered exactly"
        );
        for page in &inspected {
            let id = format!("inspect-page-{}", page["offset"]);
            let first_observation = requests
                .iter()
                .find(|request| {
                    request["body"]["input"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|item| item["type"] == "function_call_output" && item["call_id"] == id)
                })
                .unwrap();
            assert_eq!(
                first_observation["summary"], false,
                "A retrieval page was summarized before work observation"
            );
        }
    }
    let result = last["body"]["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output" && item["call_id"] == "store-next")
        .unwrap();
    let page: Value = serde_json::from_str(result["output"].as_str().unwrap()).unwrap();
    let original = &sources[1].1;
    let mut end = (8192 + 8192).min(original.len());
    while !original.is_char_boundary(end) {
        end -= 1;
    }
    assert_eq!(page["text"], original[8192..end]);
    assert_eq!(
        engine.event_text(engine.result(&input).unwrap()).unwrap(),
        "SOURCE_PAGES_CONSUMED"
    );
}

fn assert_work_instructions(body: &Value, job_id: &str, input_id: &str) {
    let instructions = body["instructions"]
        .as_str()
        .expect("bounded work lost its native system preamble");
    assert!(instructions.contains(&format!("currently working inside job {job_id}")));
    assert!(instructions.contains(&format!("status=ACTIVE with ID {input_id}")));
    assert!(instructions.contains("When the user asks to stop, call pause_work"));
}

fn tool_payload(event: &bone::state::Event) -> Value {
    use rig_core::{
        completion::Message,
        message::{ToolResultContent, UserContent},
    };
    let Message::User { content } = serde_json::from_value(event.data["message"].clone()).unwrap()
    else {
        panic!("missing tool message")
    };
    let UserContent::ToolResult(result) = &content[0] else {
        panic!("missing native tool result")
    };
    match &result.content[0] {
        ToolResultContent::Text(text) => serde_json::from_str(&text.text).unwrap(),
        ToolResultContent::Json { value } => value.clone(),
        _ => panic!("unexpected tool result content"),
    }
}

fn read_requests(path: &PathBuf) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
