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
                {"type":"function_call","name":"read_file","call_id":"read-a","arguments":{"path":"a.txt"}},
                {"type":"function_call","name":"read_file","call_id":"read-b","arguments":{"path":"b.txt"}}
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
            "Read both files using the default limits; inspect original evidence and continue.",
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
            saw_pair = true;
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
}

fn read_requests(path: &PathBuf) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
