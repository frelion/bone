//! Long sessions and hard interruption through public Engine/CLI entry points.
use std::{
    collections::BTreeSet,
    path::Path,
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use bone::{
    runtime::{Engine, RunOptions},
    state::Event,
};
use serde_json::{Value, json};

mod support;
use support::bounded_output;

struct Fixture(support::Fixture);
impl std::ops::Deref for Fixture {
    type Target = support::Fixture;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl Fixture {
    fn script(script: Value, server: &str) -> Self {
        Self(support::Fixture::script(script, server))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::write(self.workspace.join("release"), "release");
    }
}

fn call(name: &str, id: &str, arguments: Value) -> Value {
    json!({"output":[{"type":"function_call","name":name,"call_id":id,"arguments":arguments}]})
}

async fn drive(engine: &mut Engine, input: &str) {
    let previous = engine.result(input).map(|event| event.id.clone());
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if engine
                .result(input)
                .is_some_and(|event| Some(&event.id) != previous.as_ref())
            {
                break;
            }
            engine.step().await.unwrap();
            assert!(
                !engine.is_quiescent() || engine.result(input).is_some(),
                "no result for active input"
            );
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("long-session fixture stalled");
}

fn effects_are_owned(events: &[Event]) {
    for event in events
        .iter()
        .filter(|event| event.kind.starts_with("model_") || event.kind.starts_with("tool_"))
    {
        assert!(
            event.job_id.is_some() && event.call_id.is_some() && event.root_input.is_some(),
            "unowned {}",
            event.kind
        );
    }
}

fn native_tool_outputs_have_calls(requests: &[Value]) {
    let mut outputs = 0;
    for request in requests {
        let mut calls = BTreeSet::new();
        for item in request["body"]["input"].as_array().unwrap() {
            if item["type"] == "function_call" {
                calls.insert(item["call_id"].as_str().unwrap());
            }
            if item["type"] == "function_call_output" {
                outputs += 1;
                assert!(
                    calls.contains(item["call_id"].as_str().unwrap()),
                    "native orphan tool output"
                );
            }
        }
    }
    assert!(
        outputs > 0,
        "fixture did not exercise native tool result history"
    );
}

#[tokio::test]
async fn twelve_turns_compact_repeatedly_and_resume_without_losing_original_constraints() {
    let turns: Vec<_> = (1..=12).map(|round| {
        let prefix = if round == 1 { "EARLY_CONSTRAINT exact signed integer cents; UNFINISHED_WORK CLI dry-run. " } else { "Round delivery. " };
        let mut turn = json!({"text":format!("{prefix}ROUND-{round:02}: {}", "observed history ".repeat(350))});
        if round == 12 { turn["contains"] = json!(["EARLY_CONSTRAINT", "UNFINISHED_WORK"]); }
        turn
    }).collect();
    let fixture = Fixture::script(
        json!({"turns":turns,
        "summary":"EARLY_CONSTRAINT: signed integer cents. UNFINISHED_WORK: CLI dry-run remains pending. Preserve revisions and all completed round results.",
        "summary_contains":["EARLY_CONSTRAINT","UNFINISHED_WORK"]}),
        "fixtures/long_task/server.py",
    );
    let options = RunOptions {
        context_chars: 20_000,
        max_calls: 24,
        ..Default::default()
    };
    let mut engine = fixture.engine(None, options.clone());
    let session = engine.state().id.clone();
    let mut inputs = Vec::new();
    for round in 1..=12 {
        let text = if round == 1 {
            "ROUND-01 EARLY_CONSTRAINT integer cents; UNFINISHED_WORK CLI dry-run".to_owned()
        } else {
            format!("ROUND-{round:02}: continue the same ledger project")
        };
        let input = engine
            .post(&text, None)
            .unwrap_or_else(|error| panic!("round {round} admission: {error:#}"));
        drive(&mut engine, &input).await;
        assert_eq!(
            engine.result(&input).unwrap().kind,
            "delivery",
            "round {round} result: {:?}",
            engine.result(&input).unwrap().data
        );
        inputs.push(input);
        if round == 6 {
            engine.stop().unwrap();
            assert!(engine.state().paused);
            drop(engine);
            engine = fixture.engine(Some(&session), options.clone());
            assert!(engine.state().paused);
            engine.resume().unwrap();
        }
    }
    let events = engine.events().unwrap();
    assert!(
        events
            .iter()
            .filter(|event| event.kind == "summary")
            .count()
            >= 2
    );
    for input in inputs {
        assert!(
            events
                .iter()
                .any(|event| event.kind == "delivery" && event.reply_to.as_ref() == Some(&input))
        );
    }
    assert_eq!(engine.state().id, session);
    assert_eq!(engine.state().jobs.len(), 1);
    effects_are_owned(&events);
    assert!(
        fixture
            .requests()
            .iter()
            .filter(|request| request["summary"] == true)
            .count()
            >= 2
    );
    // Public audit access still returns the original body after hot-history eviction.
    let first = events
        .iter()
        .find(|event| event.kind == "model_message")
        .unwrap();
    assert!(
        engine
            .read_event(&first.id)
            .unwrap()
            .data
            .to_string()
            .contains("ROUND-01")
    );
}

#[tokio::test]
async fn exact_wait_handoff_and_late_instruction_preserve_native_protocol() {
    let mut send = call(
        "job_send",
        "assign_a",
        json!({"title":"A","message":"deliver A"}),
    );
    send["output"].as_array_mut().unwrap().push(
        call(
            "job_send",
            "assign_b",
            json!({"title":"B","message":"deliver B"}),
        )["output"][0]
            .clone(),
    );
    send["match_job_title"] = json!("Conversation");
    let mut wait = call("job_wait", "wait_exact", json!({"input_ids":"$input_ids"}));
    wait["match_job_title"] = json!("Conversation");
    let fixture = Fixture::script(
        json!({"turns":[send, wait,
        {"match_job_title":"A","text":"A exact delivery"},
        {"match_job_title":"B","text":"B exact delivery"},
        {"match_job_title":"Conversation","contains":["A exact delivery","B exact delivery"],"text":"Assigned work delivered"},
        call("job_handoff","handoff",json!({"title":"Ledger"})),
        {"text":"Focused ledger work continues"},
        call("write_file","stale_write",json!({"path":"stale.txt","content":"obsolete","expected_sha256":null})),
        {"contains":["LATEST_CONSTRAINT"],"text":"Latest instruction honored"}]}),
        "fixtures/long_task/server.py",
    );
    let mut engine = fixture.engine(
        None,
        RunOptions {
            max_parallel: 2,
            ..Default::default()
        },
    );
    let input = engine.post("Complete the two assigned jobs", None).unwrap();
    drive(&mut engine, &input).await;
    let assignments: Vec<_> = engine
        .events()
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "input" && event.data["source"] == "job")
        .map(|event| event.id)
        .collect();
    assert_eq!(assignments.len(), 2);
    let events = engine.events().unwrap();
    for assignment in assignments {
        assert!(
            events
                .iter()
                .any(|event| event.kind == "delivery"
                    && event.reply_to.as_ref() == Some(&assignment))
        );
        assert!(events.iter().any(|event| event.kind == "tool_result"
            && event.data["tool_name"] == "job_wait"
            && event.data.to_string().contains(&assignment)));
    }
    let original_focus = engine.state().focus.clone();
    let handoff = engine
        .post("Hand off continuing ledger work", None)
        .unwrap();
    drive(&mut engine, &handoff).await;
    assert_ne!(engine.state().focus, original_focus);
    let old = engine.post("Propose an obsolete write", None).unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !engine
            .events()
            .unwrap()
            .iter()
            .any(|event| event.kind == "model_message" && event.root_input.as_ref() == Some(&old))
        {
            engine.step().await.unwrap();
        }
    })
    .await
    .unwrap();
    let latest = engine
        .post("LATEST_CONSTRAINT: do not write the obsolete file", None)
        .unwrap();
    drive(&mut engine, &latest).await;
    assert!(!fixture.workspace.join("stale.txt").exists());
    let events = engine.events().unwrap();
    assert!(
        events.iter().any(
            |event| event.kind == "tool_result" && event.data.to_string().contains("cancelled")
        )
    );
    assert!(
        engine
            .result(&old)
            .is_none_or(|event| event.kind != "delivery")
    );
    effects_are_owned(&events);
    native_tool_outputs_have_calls(&fixture.requests());
}

#[tokio::test]
async fn archived_public_requirements_and_summary_are_readable_across_job_handoff() {
    let mut turns: Vec<Value> = (0..10)
        .flat_map(|round| [call("read_file", &format!("audit_read_{round}"), json!({"path":"audit.txt"})),
            json!({"text":format!("Completed audit work. {}", "observed history ".repeat(350))})])
        .collect();
    let fixture = Fixture::script(
        json!({"reload_script":true,"turns":turns,
        "summary":"Completed storage and mutation work. Export and CLI remain pending."}),
        "fixtures/long_task/server.py",
    );
    std::fs::write(
        fixture.workspace.join("audit.txt"),
        "Read-only audit fixture",
    )
    .unwrap();
    let mut engine = fixture.engine(
        None,
        RunOptions {
            context_chars: 20_000,
            max_calls: 24,
            ..Default::default()
        },
    );
    let original = engine.post("ORIGINAL_SPEC: stable CSV export and CLI dry-run; only five existing files may change.", None).unwrap();
    drive(&mut engine, &original).await;
    let first_job = engine.state().focus.clone();
    for round in 1..10 {
        let input = engine
            .post(&format!("Continue audit round {round}"), None)
            .unwrap();
        drive(&mut engine, &input).await;
    }
    let events = engine.events().unwrap();
    let summaries: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "summary")
        .collect();
    assert!(summaries.len() >= 2);
    assert!(
        events.len() > 60,
        "must exercise a substantial audit history"
    );
    let summary = (*summaries.last().unwrap()).clone();
    assert!(
        !summary.data.to_string().contains("ORIGINAL_SPEC"),
        "summary intentionally loses the spec"
    );
    let update_script = |turns: &[Value]| {
        std::fs::write(
            &fixture.responses,
            serde_json::to_vec(&json!({
                "reload_script":true,"turns":turns,
                "summary":"Completed storage and mutation work. Export and CLI remain pending."
            }))
            .unwrap(),
        )
        .unwrap();
    };
    turns.push(call(
        "job_handoff",
        "move_context",
        json!({"title":"Continuing ledger"}),
    ));
    turns.push(json!({"text":"New job owns this conversation"}));
    update_script(&turns);
    let input = engine.post("Hand off this continuing work", None).unwrap();
    drive(&mut engine, &input).await;
    assert_ne!(engine.state().focus, first_job);
    let inspect_result = |engine: &Engine, input: &str| -> Value {
        let event = engine
            .events()
            .unwrap()
            .into_iter()
            .find(|event| {
                event.kind == "tool_result"
                    && event.root_input.as_deref() == Some(input)
                    && event.data["tool_name"] == "job_inspect"
            })
            .unwrap();
        let text = event.data["message"]["content"][0]["content"][0]["text"]
            .as_str()
            .unwrap();
        serde_json::from_str(text).unwrap()
    };
    let mut before: Option<String> = None;
    let mut public_records = Vec::new();
    for page in 0..20 {
        turns.push(call(
            "job_inspect",
            &format!("find_original_{page}"),
            json!({
                "job_id":engine.state().focus, "users_only":true, "limit":32,"before_id":before
            }),
        ));
        turns.push(json!({"text":"Public requirement page obtained"}));
        update_script(&turns);
        let input = engine
            .post("Recover the original requirements", None)
            .unwrap();
        drive(&mut engine, &input).await;
        let public = inspect_result(&engine, &input);
        assert_eq!(public["users_only"], true);
        public_records.extend(public["records"].as_array().unwrap().iter().cloned());
        if public_records.iter().any(|record| record["id"] == original) {
            break;
        }
        let next = public["next_before_id"]
            .as_str()
            .expect("original input not reachable by pagination");
        assert_ne!(
            before.as_deref(),
            Some(next),
            "pagination cursor did not advance"
        );
        before = Some(next.to_owned());
    }
    let recovered = public_records
        .iter()
        .find(|record| record["id"] == original)
        .expect("original cross-job public input missing");
    assert_eq!(recovered["job_id"].as_str(), first_job.as_deref());
    assert!(
        recovered["text"]
            .as_str()
            .unwrap()
            .contains("ORIGINAL_SPEC")
    );
    turns.push(call(
        "job_inspect",
        "read_summary",
        json!({"event_id":summary.id,"limit":512}),
    ));
    turns.push(json!({"text":"Readable summary obtained"}));
    update_script(&turns);
    let input = engine.post("Read the archived summary", None).unwrap();
    drive(&mut engine, &input).await;
    let readable = inspect_result(&engine, &input);
    assert_eq!(readable["raw"], false);
    assert_eq!(
        readable["text"],
        "Completed storage and mutation work. Export and CLI remain pending."
    );
    let expected = serde_json::to_string(&summary).unwrap();
    let mut raw = String::new();
    loop {
        turns.push(call(
            "job_inspect",
            &format!("raw_page_{}", raw.len()),
            json!({
                "event_id":summary.id,"raw":true,"offset":raw.chars().count(),"limit":512
            }),
        ));
        turns.push(json!({"text":"Raw audit page obtained"}));
        update_script(&turns);
        let input = engine.post("Read the next raw audit page", None).unwrap();
        drive(&mut engine, &input).await;
        let page = inspect_result(&engine, &input);
        assert_eq!(page["raw"], true);
        raw.push_str(page["text"].as_str().unwrap());
        if page["next_offset"].is_null() {
            break;
        }
        assert_eq!(
            page["next_offset"].as_u64().unwrap() as usize,
            raw.chars().count()
        );
        assert!(raw.len() < expected.len());
    }
    assert_eq!(raw, expected);
    assert_eq!(
        serde_json::from_str::<Value>(&raw).unwrap()["id"],
        summary.id
    );
    effects_are_owned(&engine.events().unwrap());
    native_tool_outputs_have_calls(&fixture.requests());
}

fn finish_child(child: Child) -> Output {
    support::finish_child(child, Duration::from_secs(25))
}

#[tokio::test]
async fn killed_owner_cannot_reconcile_or_allow_competing_writes_until_its_shell_exits() {
    let mut competing = call(
        "write_file",
        "competing_effect",
        json!({"path":"intruder.txt","content":"unsafe","expected_sha256":null}),
    );
    competing["match_last_user_contains"] = json!("Attempt another workspace write");
    let fixture = Fixture::script(
        json!({"turns":[
        call("shell","original_effect",json!({"command":"printf x >> effect.txt; i=0; while [ ! -e release ] && [ \"$i\" -lt 400 ]; do sleep 0.05; i=$((i+1)); done; printf y >> effect.txt","timeout_seconds":25})),
        competing,
        {"match_last_user_contains":"Attempt another workspace write","text":"Competing write was blocked"},
        call("read_file","inspect_original",json!({"path":"effect.txt"})),
        {"text":"Original effect inspected, awaiting explicit reconciliation"},
        {"text":"Reconciled original operation without replay"}]}),
        "fixtures/long_task/server.py",
    );
    let mut command = fixture.command_at(&fixture.data);
    command
        .arg("run")
        .arg("Perform the original effect once")
        .arg("--workspace")
        .arg(&fixture.workspace)
        .arg("--profile")
        .arg("fixture")
        .arg("--json");
    let mut owner = command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !fixture.workspace.join("effect.txt").exists() {
        assert!(Instant::now() < deadline, "original shell did not start");
        assert!(
            owner.try_wait().unwrap().is_none(),
            "owner exited before effect"
        );
        thread::sleep(Duration::from_millis(10));
    }
    owner.kill().unwrap();
    owner.wait().unwrap();
    let mut sessions = fixture.command_at(&fixture.data);
    sessions.arg("sessions").arg("--json");
    let states: Value = serde_json::from_slice(&bounded_output(sessions).stdout).unwrap();
    let session = states[0]["id"].as_str().unwrap().to_owned();
    let mut engine = fixture.engine(Some(&session), RunOptions::default());
    assert_eq!(engine.state().unknown_writes.len(), 1);
    let uncertain = engine.state().unknown_writes.keys().next().unwrap().clone();
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("effect.txt")).unwrap(),
        "x"
    );
    assert!(
        engine
            .resolve_write(&uncertain, "The child is still running; must not reconcile")
            .is_err(),
        "live shell was reconciled after owner kill"
    );
    assert!(engine.state().unknown_writes.contains_key(&uncertain));
    let other_data = fixture.root.path().join("other-data");
    std::fs::create_dir(&other_data).unwrap();
    std::fs::copy(
        fixture.data.join("config.toml"),
        other_data.join("config.toml"),
    )
    .unwrap();
    let mut contender = fixture.command_at(&other_data);
    contender
        .arg("run")
        .arg("Attempt another workspace write")
        .arg("--workspace")
        .arg(&fixture.workspace)
        .arg("--profile")
        .arg("fixture")
        .arg("--json");
    let mut competing_process = contender
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while fixture.requests().len() < 2 && competing_process.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "contender did not start or reject promptly"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!fixture.workspace.join("intruder.txt").exists());
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("effect.txt")).unwrap(),
        "x",
        "child ended before the lifetime assertion"
    );
    assert!(
        engine
            .resolve_write(&uncertain, "Still waiting for the same original child")
            .is_err()
    );
    // This sentinel is test coordination, not a model/tool effect.
    std::fs::write(fixture.workspace.join("release"), "release").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::fs::read_to_string(fixture.workspace.join("effect.txt")).unwrap() != "xy" {
        assert!(
            Instant::now() < deadline,
            "original child did not finish after release"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let _ = finish_child(competing_process);
    assert!(!fixture.workspace.join("intruder.txt").exists());
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("effect.txt")).unwrap(),
        "xy"
    );
    let input = engine
        .events()
        .unwrap()
        .into_iter()
        .find(|event| event.kind == "input" && event.data["source"] == "user")
        .unwrap()
        .id;
    engine.resume().unwrap();
    drive(&mut engine, &input).await;
    assert!(engine.state().unknown_writes.contains_key(&uncertain));
    assert_ne!(engine.result(&input).unwrap().kind, "delivery");
    engine
        .resolve_write(
            &uncertain,
            "Observed xy from one original operation; never replay it",
        )
        .unwrap();
    engine.resume().unwrap();
    drive(&mut engine, &input).await;
    let events = engine.events().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "tool_started" && event.data["tool_name"] == "shell")
            .count(),
        1
    );
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("effect.txt")).unwrap(),
        "xy"
    );
    effects_are_owned(&events);
}

#[test]
fn long_task_harness_keeps_model_quality_failures_and_raw_rounds() {
    let turns: Vec<_> = (1..=12).map(|round| {
        if round == 7 {
            call("pause_work", "pause_saved_session", json!({"reason":"user requested pause"}))
        } else {
            json!({"text":format!("CSV export and CLI dry-run remain pending. Round {round}: {}", "observed history ".repeat(350))})
        }
    }).collect();
    let fixture = Fixture::script(
        json!({"turns":turns,"summary":"Preserve pending CSV export, CLI dry-run, integer cents, and UTC constraints."}),
        "fixtures/long_task/server.py",
    );
    let artifacts = fixture.root.path().join("dogfood");
    let mut command = Command::new("python3");
    command
        .arg("-B")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/long_task.py"))
        .arg("--run")
        .arg("--bone")
        .arg(env!("CARGO_BIN_EXE_bone"))
        .arg("--data-dir")
        .arg(&fixture.data)
        .arg("--profile")
        .arg("fixture")
        .arg("--output-dir")
        .arg(&artifacts)
        .arg("--context-chars")
        .arg("20000")
        .env("BONE_TEST_DUMMY_KEY", "long-session-synthetic-key")
        .env_remove("BONE_MODEL");
    let output = bounded_output(command);
    assert!(
        !output.status.success(),
        "unfinished package must fail independent acceptance"
    );
    let result: Value =
        serde_json::from_slice(&std::fs::read(artifacts.join("result.json")).unwrap()).unwrap();
    assert_eq!(result["rounds"], 12);
    assert!(!result["quality_failures"].as_array().unwrap().is_empty());
    assert!(result["kernel_faults"].as_array().unwrap().is_empty());
    assert!(result["execution_failures"].as_array().unwrap().is_empty());
    assert!(result["coverage_failures"].as_array().unwrap().is_empty());
    assert!(result["metrics"]["cost"].is_null());
    let progress: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let second = progress.iter().find(|line| line["round"] == 2).unwrap();
    assert_eq!(second["verification_passed"], false);
    assert_eq!(second["deferred_work_passed"], true);
    assert_eq!(second["no_write_passed"], true);
    assert_eq!(second["passed"], false);
    assert!(second.get("checks_passed").is_none());
    let records: Vec<Value> = std::fs::read_to_string(artifacts.join("rounds.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        records
            .iter()
            .filter(|record| record.get("round").is_some())
            .count(),
        12
    );
    for round in 1..=12 {
        let directory = artifacts.join(format!("round-{round:02}"));
        assert!(directory.join("run.stdout").is_file());
        assert!(directory.join("history.stdout").is_file());
        assert!(directory.join("verification.stdout").is_file());
    }
    assert!(
        records
            .iter()
            .any(|record| record["round"] == 7 && record["status"] == "paused")
    );
    assert!(
        records.last().unwrap()["event_counts"]["summaries"]
            .as_u64()
            .unwrap()
            >= 2
    );
}

#[tokio::test]
async fn completed_natural_pause_is_not_consumed_again_after_continuation() {
    let fixture = Fixture::script(
        json!({"turns":[
            call("pause_work", "natural_pause", json!({"reason":"user pause request"})),
            {"text":"The new continuation is complete"},
            call("pause_work", "unexpected_pause_replay", json!({"reason":"old pause input was replayed"}))
        ]}),
        "fixtures/long_task/server.py",
    );
    let mut engine = fixture.engine(
        None,
        RunOptions {
            max_calls: 8,
            ..Default::default()
        },
    );
    let pause = engine.post("Pause work now", None).unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !engine.state().paused {
            engine.step().await.unwrap();
        }
    })
    .await
    .unwrap();
    let continuation = engine
        .post("Continue and explain the completed work", None)
        .unwrap();
    drive(&mut engine, &continuation).await;
    for _ in 0..24 {
        if engine.is_quiescent() {
            break;
        }
        engine.step().await.unwrap();
        tokio::task::yield_now().await;
    }
    assert!(
        !engine.state().paused,
        "completed natural-language pause was replayed"
    );
    assert!(
        engine.is_quiescent(),
        "old pause left runnable work after continuation"
    );
    assert_eq!(
        fixture.requests().len(),
        2,
        "completed pause caused another model invocation"
    );
    assert!(
        engine
            .state()
            .jobs
            .values()
            .all(|job| job.active_input.as_ref() != Some(&pause) && !job.inbox.contains(&pause))
    );
}
