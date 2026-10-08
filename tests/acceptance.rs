//! Independent CLI acceptance through a scripted localhost Responses endpoint.
use std::{path::Path, time::Duration};

use serde_json::{Value, json};

mod support;
use support::{Fixture, bounded_output};

fn tool(name: &str, arguments: Value, call: &str) -> Value {
    json!({"output":[{"type":"function_call","name":name,"arguments":arguments,"call_id":call}]})
}

fn assert_effect_ownership(events: &[Value]) {
    let effects: Vec<_> = events
        .iter()
        .filter(|event| {
            let kind = event["kind"].as_str().unwrap();
            kind.starts_with("model_") || kind.starts_with("tool_")
        })
        .collect();
    assert!(!effects.is_empty(), "no model/tool events found");
    for event in effects {
        assert!(
            event["job_id"].is_string(),
            "effect lacks job_id: {}",
            event["kind"]
        );
        assert!(
            event["root_input"].is_string(),
            "effect lacks root input: {}",
            event["kind"]
        );
        assert!(
            event["call_id"].is_string(),
            "effect lacks call_id: {}",
            event["kind"]
        );
    }
}

#[test]
fn real_runtime_repairs_file_and_persists_owned_effects() {
    let before = "def total(values):\n    return sum(values)\n";
    let after =
        "def total(values):\n    return sum(value for value in values if value is not None)\n";
    let fixture = Fixture::turns(json!([
        tool("read_file",json!({"path":"totals.py"}),"call_read"),
        tool("write_file",json!({"path":"totals.py","content":after,"expected_sha256":format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(before.as_bytes()))}),"call_write"),
        {"text":"Fixed totals.py and verified empty values and None."}
    ]));
    std::fs::write(fixture.workspace.join("totals.py"), before).unwrap();
    let (output, result) = fixture.run(
        "Fix totals.py to ignore None and sum empty input as zero.",
        None,
        &[],
    );
    assert!(output.status.success(), "run failed: {}", result["text"]);
    assert_eq!(result["status"], "completed");
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("totals.py")).unwrap(),
        after
    );
    assert_eq!(fixture.request_count(), 3);
    assert_eq!(result["metrics"]["model_calls"], 3);
    assert_eq!(result["metrics"]["input_tokens"], 30);
    assert_effect_ownership(&fixture.history(result["session_id"].as_str().unwrap()));
}

#[test]
fn idle_job_accepts_followup_after_process_restart() {
    let fixture = Fixture::turns(
        json!([{"text":"First answer."},{"contains":["First answer.","Revised instruction"],"text":"Updated answer."}]),
    );
    let (first_output, first) = fixture.run("Initial instruction", None, &[]);
    assert!(first_output.status.success());
    let session = first["session_id"].as_str().unwrap();
    let (second_output, second) = fixture.run("Revised instruction", Some(session), &[]);
    assert!(second_output.status.success());
    assert_eq!(second["session_id"], first["session_id"]);
    assert_ne!(second["input_id"], first["input_id"]);
    assert_eq!(second["status"], "completed");
    let history = fixture.history(session);
    assert_effect_ownership(&history);
    let jobs: std::collections::BTreeSet<_> = history
        .iter()
        .filter(|event| event["kind"].as_str().unwrap().starts_with("model_"))
        .filter_map(|event| event["job_id"].as_str())
        .collect();
    assert_eq!(jobs.len(), 1, "continuation created another agent context");
    assert_eq!(fixture.request_count(), 2);
}

#[test]
fn handoff_moves_focus_and_followup_to_target_job() {
    let fixture = Fixture::turns(json!([
        tool("job_handoff",json!({"title":"Focused implementation"}),"call_handoff"),
        {"text":"Target delivery."},
        {"contains":["Target delivery.","Continue target"],"text":"Target continued."}
    ]));
    let (output, first) = fixture.run(
        "Delegate this original input to a focused internal job.",
        None,
        &[],
    );
    assert!(output.status.success());
    assert_eq!(first["status"], "completed");
    let session = first["session_id"].as_str().unwrap();
    let (output, second) = fixture.run("Continue target", Some(session), &[]);
    assert!(output.status.success());
    assert_eq!(second["status"], "completed");
    let history = fixture.history(session);
    assert_effect_ownership(&history);
    let models: Vec<_> = history
        .iter()
        .filter(|event| event["kind"].as_str().unwrap().starts_with("model_"))
        .collect();
    let first_job = models.first().unwrap()["job_id"].as_str().unwrap();
    let final_job = models.last().unwrap()["job_id"].as_str().unwrap();
    assert_ne!(first_job, final_job, "handoff did not transfer ownership");
    assert_eq!(fixture.request_count(), 3);
}

#[test]
fn provider_failure_is_persisted_and_does_not_report_success() {
    let fixture = Fixture::turns(json!([{"http_status":503}]));
    let (output, result) = fixture.run("Complete this instruction.", None, &[]);
    assert!(!output.status.success());
    assert_eq!(result["status"], "failed");
    assert_eq!(
        fixture.request_count(),
        1,
        "provider failure replayed blindly"
    );
    let history = fixture.history(result["session_id"].as_str().unwrap());
    assert_effect_ownership(&history);
    assert!(
        history
            .iter()
            .any(|event| event["kind"].as_str().unwrap().contains("fail"))
    );
}

#[test]
fn failed_request_does_not_turn_partial_usage_into_a_known_total() {
    let fixture = Fixture::turns(json!([
        tool("list_files", json!({}), "call_usage_inspection"),
        {"http_status":503}
    ]));
    let (output, result) = fixture.run("Inspect this workspace and report.", None, &[]);
    assert!(!output.status.success());
    assert_eq!(result["status"], "failed");
    assert_eq!(result["metrics"]["model_calls"], 2);
    for field in ["input_tokens", "output_tokens", "total_tokens", "cost"] {
        assert!(
            result["metrics"][field].is_null(),
            "partial {field} was reported as a known total"
        );
    }
}

async fn drive_until_result(engine: &mut bone::runtime::Engine, input: &str) {
    let prior = engine.result(input).map(|event| event.id.clone());
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if engine
                .result(input)
                .is_some_and(|event| Some(&event.id) != prior.as_ref())
            {
                break;
            }
            let events = engine.step().await.unwrap();
            assert!(
                !events.is_empty() || !engine.is_quiescent(),
                "runtime quiesced before delivering input"
            );
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("scripted runtime did not deliver within 15 seconds");
}

#[tokio::test]
async fn two_jobs_deliver_to_exact_assignment_ids_before_waiter_continues() {
    let root_text = "Coordinate two independent internal tasks";
    let mut assign = tool(
        "job_send",
        json!({"title":"A","message":"FIXTURE_A_ONLY"}),
        "call_assign_a",
    );
    assign["output"].as_array_mut().unwrap().push(
        tool(
            "job_send",
            json!({"title":"B","message":"FIXTURE_B_ONLY"}),
            "call_assign_b",
        )["output"][0]
            .clone(),
    );
    assign["match_job_title"] = json!("Conversation");
    let mut wait = tool("job_wait", json!({"input_ids":"$input_ids"}), "call_wait");
    wait["match_job_title"] = json!("Conversation");
    let fixture = Fixture::turns(json!([
        assign, wait,
        {"match_job_title":"A","text":"A delivered"},
        {"match_job_title":"B","text":"B delivered"},
        {"match_job_title":"Conversation","contains":["A delivered","B delivered"],"text":"Both independent tasks completed"}
    ]));
    let mut engine = fixture.engine(
        None,
        bone::runtime::RunOptions {
            max_parallel: 2,
            ..Default::default()
        },
    );
    let input = engine.post(root_text, None).unwrap();
    drive_until_result(&mut engine, &input).await;
    let assigned: Vec<_> = engine
        .events()
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "input" && event.data["source"] == "job")
        .map(|event| event.id.clone())
        .collect();
    assert_eq!(assigned.len(), 2);
    for assignment in &assigned {
        let reply = engine
            .events().unwrap().into_iter()
            .find(|event| event.kind == "delivery" && event.reply_to.as_ref() == Some(assignment))
            .unwrap_or_else(|| panic!("assignment has no exact delivery; compact fixture trace: {:?}",engine.events().unwrap().into_iter().map(|event|json!({"kind":event.kind,"job":event.job_id,"reply_to":event.reply_to,"error":event.data.get("error"),"choice":event.data.get("response").and_then(|response|response.get("choice"))})).collect::<Vec<_>>()));
        assert_ne!(reply.job_id.as_ref(), engine.state().focus.as_ref());
    }
    let waited = engine
        .events()
        .unwrap()
        .into_iter()
        .find(|event| event.kind == "tool_result" && event.data["tool_name"] == "job_wait")
        .expect("wait produced no native tool result");
    let result = serde_json::to_string(&waited.data["message"]).unwrap();
    assert!(assigned.iter().all(|id| result.contains(id)));
    assert!(result.contains("A delivered") && result.contains("B delivered"));
    assert_eq!(engine.result(&input).unwrap().kind, "delivery");
    assert_eq!(fixture.request_count(), 5);
    assert_eq!(engine.state().jobs.len(), 3);
    assert!(
        engine
            .state()
            .jobs
            .values()
            .all(|job| job.state == bone::state::JobState::Idle)
    );
}

#[tokio::test]
async fn new_instruction_blocks_unstarted_write_proposals() {
    let mut stale = tool(
        "write_file",
        json!({"path":"stale.txt","content":"old effects","expected_sha256":null}),
        "call_stale_write",
    );
    stale["contains"] = json!(["Create stale.txt"]);
    let fixture = Fixture::turns(
        json!([stale,{"contains":["Do not write any files"],"text":"New constraint accepted; no files written"}]),
    );
    let mut engine = fixture.engine(None, bone::runtime::RunOptions::default());
    let old = engine.post("Create stale.txt", None).unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !engine
            .events()
            .unwrap()
            .into_iter()
            .any(|event| event.kind == "model_message")
        {
            engine.step().await.unwrap();
            assert!(
                !engine.is_quiescent(),
                "model failed before proposing a write"
            );
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        !engine
            .events()
            .unwrap()
            .into_iter()
            .any(|event| event.kind == "tool_started")
    );
    let before = engine.state().revision;
    let newest = engine
        .post(
            "Do not write any files. Acknowledge this changed constraint.",
            None,
        )
        .unwrap();
    assert!(engine.state().revision > before);
    drive_until_result(&mut engine, &newest).await;
    assert!(!fixture.workspace.join("stale.txt").exists());
    assert!(
        !engine
            .events()
            .unwrap()
            .into_iter()
            .any(|event| event.kind == "tool_started" && event.data["tool_name"] == "write_file")
    );
    assert!(
        engine
            .events()
            .unwrap()
            .into_iter()
            .any(|event| event.kind == "tool_result"
                && event.data["tool_name"] == "write_file"
                && event.data.to_string().contains("cancelled"))
    );
    assert!(
        engine
            .result(&old)
            .is_none_or(|event| event.kind != "delivery"),
        "overtaken input delivered stale output"
    );
    assert_eq!(engine.result(&newest).unwrap().kind, "delivery");
}

#[tokio::test]
async fn interrupted_write_survives_restart_and_requires_reconciliation() {
    let command = if cfg!(windows) {
        "powershell.exe -NoProfile -NonInteractive -Command \"[IO.File]::AppendAllText('effect.txt', 'x'); [Threading.Thread]::Sleep(5000)\""
    } else {
        "printf x >> effect.txt; sleep 5"
    };
    let fixture = Fixture::turns(json!([
        tool("shell",json!({"command":command,"timeout_seconds":10}),"call_append_once"),
        tool("read_file",json!({"path":"effect.txt"}),"call_inspect_effect"),
        {"text":"Effect observed; waiting for explicit reconciliation"},
        {"contains":["already appended once"],"text":"Observed append reconciled without replay"}
    ]));
    let mut engine = fixture.engine(None, bone::runtime::RunOptions::default());
    let input = engine.post("Append once to effect.txt", None).unwrap();
    let session = engine.state().id.clone();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !fixture.workspace.join("effect.txt").exists() {
            let _ = tokio::time::timeout(Duration::from_millis(20), engine.step()).await;
            assert!(
                !engine.is_quiescent(),
                "shell failed before appending fixture effect"
            );
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    engine.stop().unwrap();
    assert_eq!(engine.state().unknown_writes.len(), 1);
    let call = engine.state().unknown_writes.keys().next().unwrap().clone();
    drop(engine);
    let mut restored = fixture.engine(Some(&session), bone::runtime::RunOptions::default());
    assert!(restored.state().unknown_writes.contains_key(&call));
    restored.resume().unwrap();
    drive_until_result(&mut restored, &input).await;
    assert_eq!(
        fixture.request_count(),
        3,
        "read-only inspection did not follow the scripted path"
    );
    assert_ne!(
        restored.result(&input).unwrap().kind,
        "delivery",
        "unreconciled write reported successful completion"
    );
    assert_eq!(
        restored
            .events()
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == "tool_started" && event.data["tool_name"] == "shell")
            .count(),
        1,
        "interrupted append was replayed"
    );
    assert!(restored.state().unknown_writes.contains_key(&call));
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("effect.txt")).unwrap(),
        "x"
    );
    restored
        .resolve_write(
            &call,
            "Inspected effect.txt: already appended once; do not append again",
        )
        .unwrap();
    assert!(restored.state().unknown_writes.is_empty());
    restored.resume().unwrap();
    drive_until_result(&mut restored, &input).await;
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("effect.txt")).unwrap(),
        "x"
    );
    assert_eq!(fixture.request_count(), 4);
    assert_eq!(restored.result(&input).unwrap().kind, "delivery");
}

#[test]
fn alternating_ablation_records_all_local_trials_and_usage() {
    let before = include_str!("../fixtures/acceptance/small_repair/totals.py");
    let after =
        "def total(values):\n    return sum(value for value in values if value is not None)\n";
    let mut turns = Vec::new();
    for _ in 0..18 {
        turns.push(tool(
            "read_file",
            json!({"path":"totals.py"}),
            "call_ablate_read",
        ));
        turns.push(tool("write_file",json!({"path":"totals.py","content":after,"expected_sha256":format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(before.as_bytes()))}),"call_ablate_write"));
        turns.push(json!({"text":"The repair is complete"}));
    }
    let fixture = Fixture::turns(Value::Array(turns));
    let records = fixture.root.path().join("ablation.jsonl");
    let mut command = support::python_command();
    command
        .arg("-B")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/ablate.py"))
        .arg("--run")
        .arg("--bone")
        .arg(env!("CARGO_BIN_EXE_bone"))
        .arg("--profile")
        .arg("fixture")
        .arg("--data-dir")
        .arg(&fixture.data)
        .arg("--output")
        .arg(&records)
        .arg("--artifacts-dir")
        .arg(fixture.root.path().join("artifacts"))
        .arg("--case")
        .arg("small_repair")
        .env("BONE_TEST_DUMMY_KEY", "fixture-dummy")
        .env_remove("BONE_MODEL");
    let output = bounded_output(command);
    assert!(output.status.success(), "local ablation failed");
    let records: Vec<Value> = std::fs::read_to_string(records)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let trials: Vec<_> = records
        .iter()
        .filter(|record| record.get("case_id").is_some())
        .collect();
    assert_eq!(trials.len(), 18);
    assert!(trials.iter().all(|record| record["passed"] == true));
    for record in &trials {
        let artifacts = Path::new(record["artifacts_dir"].as_str().unwrap());
        assert!(artifacts.join("stdout.1.json").is_file());
        let history: Vec<Value> =
            serde_json::from_slice(&std::fs::read(artifacts.join("history.json")).unwrap())
                .unwrap();
        assert!(history.iter().any(|event| event["kind"] == "delivery"));
        assert!(artifacts.join("workspace/totals.py").is_file());
        assert_eq!(record["event_counts"]["jobs"], 1);
        assert_eq!(record["event_counts"]["summaries"], 0);
    }
    assert!(
        trials
            .iter()
            .all(|record| record["metrics"]["model_calls"] == 3
                && record["metrics"]["input_tokens"] == 30)
    );
    assert!(
        trials
            .iter()
            .all(|record| record["metrics"]["cost_usd"].is_null())
    );
    assert_eq!(fixture.request_count(), 54);
    assert_eq!(records.last().unwrap()["type"], "summary");
}
