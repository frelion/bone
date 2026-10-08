//! Native Responses endings must never promote an unfinished proposal to an action.
use std::time::Duration;

use bone::runtime::{Engine, RunOptions};
use futures_util::StreamExt;
use rig_core::completion::{CompletionRequest, FinishReason};
use serde_json::{Value, json};

mod support;
use support::{Fixture, drive_until};

fn proposal(name: &str, arguments: Value, id: &str) -> Value {
    json!({"type":"function_call","name":name,"arguments":arguments,"call_id":id})
}

fn write(path: &str, content: &str, id: &str) -> Value {
    proposal(
        "write_file",
        json!({"path":path,"content":content,"mode":"replace","expected_sha256":null}),
        id,
    )
}

fn limited(output: Value) -> Value {
    json!({"response_status":"incomplete","incomplete_reason":"max_output_tokens","output":output})
}

fn options() -> RunOptions {
    RunOptions {
        max_calls: 10,
        single_job: true,
        ..Default::default()
    }
}

async fn finish(engine: &mut Engine, input: &str) {
    drive_until(engine, Duration::from_secs(15), |engine| {
        engine.result(input).is_some()
    })
    .await;
}

fn assert_delivery(engine: &Engine, input: &str, expected: &str) {
    let result = engine.result(input).unwrap();
    assert_eq!(
        result.kind,
        "delivery",
        "{}",
        engine.event_text(result).unwrap()
    );
    assert_eq!(engine.event_text(result).unwrap(), expected);
}

#[tokio::test]
async fn rig_native_endings_preserve_reasons_and_drop_only_malformed_partial_calls() {
    let fixture = Fixture::turns(json!([
        limited(json!([write("native-only.txt", "complete JSON remains a proposal", "native-complete")])),
        limited(json!([{"type":"function_call","name":"write_file","call_id":"native-partial",
            "status":"in_progress","arguments":"{\"path\":\"native-partial.txt\""}])),
        limited(json!([])),
        {"response_status":"incomplete","incomplete_reason":"content_filter","output":[]},
        {"response_status":"incomplete","incomplete_reason":"max_tool_calls","output":[]}
    ]));
    let bone::config::ModelReference::Registry(reference) = fixture.local_profile().model else {
        panic!("native provider reference");
    };
    let model = reference.completion_model_with(
        "fixture-synthetic",
        rig_core::http_client::DynHttpClient::new(rig_reqwest::shared()),
    );
    for (reason, calls) in [
        (FinishReason::Length, 1),
        (FinishReason::Length, 0),
        (FinishReason::Length, 0),
        (FinishReason::ContentFilter, 0),
        (FinishReason::Other("max_tool_calls".into()), 0),
    ] {
        let mut stream = model
            .stream(CompletionRequest::new("Inspect the native ending."))
            .unwrap();
        while let Some(item) = stream.next().await {
            item.unwrap();
        }
        let response = stream.finish().await.unwrap();
        assert_eq!(response.finish_reason(), Some(reason));
        assert_eq!(response.tool_calls().count(), calls);
    }
    assert!(!fixture.workspace.join("native-only.txt").exists());
    assert!(!fixture.workspace.join("native-partial.txt").exists());
}

#[tokio::test]
async fn length_with_complete_json_discards_proposal_and_recovers_in_the_same_job() {
    let fixture = Fixture::turns(json!([
        limited(json!([write("unsafe.txt", "UNSAFE_PARTIAL_PROPOSAL", "discarded-write")])),
        {"output":[write("safe.txt", "bounded action", "replacement-write")]},
        {"text":"Completed the bounded action."}
    ]));
    let mut engine = fixture.engine(None, options());
    let input = engine
        .post(
            "Create the requested file; retain ORIGINAL_REQUIREMENT.",
            None,
        )
        .unwrap();
    finish(&mut engine, &input).await;
    assert_delivery(&engine, &input, "Completed the bounded action.");
    assert!(!fixture.workspace.join("unsafe.txt").exists());
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("safe.txt")).unwrap(),
        "bounded action"
    );
    let events = engine.events().unwrap();
    let limited = events
        .iter()
        .find(|event| event.kind == "model_limited")
        .unwrap();
    assert_eq!(limited.data["reason"], "length");
    assert!(
        limited.data["response"]
            .to_string()
            .contains("UNSAFE_PARTIAL_PROPOSAL")
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "tool_started")
            .count(),
        1
    );
    assert!(
        events
            .iter()
            .all(|event| event.job_id == events[0].job_id || event.job_id.is_none())
    );
    let requests = fixture.requests();
    assert_eq!(requests.len(), 3);
    let recovery = requests[1]["body"].to_string();
    assert!(recovery.contains("ORIGINAL_REQUIREMENT"));
    assert!(
        !recovery.contains("discarded-write"),
        "unfinished native calls entered working history"
    );
    assert!(!recovery.contains("UNSAFE_PARTIAL_PROPOSAL"));
    let started = events
        .iter()
        .find(|event| event.kind == "model_started" && event.data["recovery_of"].is_string())
        .unwrap();
    assert_eq!(
        started.data["recovery_of"],
        limited.call_id.as_ref().unwrap().as_str()
    );
    assert_eq!(started.root_input.as_deref(), Some(input.as_str()));
}

#[tokio::test]
async fn malformed_partial_arguments_remain_unexecuted_and_allow_a_new_generation() {
    let fixture = Fixture::turns(json!([
        limited(json!([{"type":"function_call","name":"write_file","call_id":"partial-write",
            "status":"in_progress","arguments":"{\"path\":\"partial.txt\",\"content\":\"unfinished"}])),
        {"text":"The unfinished proposal was replaced."}
    ]));
    let mut engine = fixture.engine(None, options());
    let input = engine.post("Handle the partial response.", None).unwrap();
    finish(&mut engine, &input).await;
    assert_delivery(&engine, &input, "The unfinished proposal was replaced.");
    assert!(!fixture.workspace.join("partial.txt").exists());
    let events = engine.events().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "model_limited")
            .count(),
        1
    );
    assert!(events.iter().all(|event| event.kind != "tool_started"));
    assert_eq!(fixture.request_count(), 2);
    assert!(
        !fixture.requests()[1]["body"]
            .to_string()
            .contains("partial-write")
    );
}

#[tokio::test]
async fn reasoning_only_length_preserves_generation_configuration_and_usage() {
    let fixture = Fixture::turns(json!([
        {"response_status":"incomplete","incomplete_reason":"max_output_tokens","output":[],
         "usage":{"input_tokens":11,"output_tokens":128,"total_tokens":139,
                  "output_tokens_details":{"reasoning_tokens":128}}},
        {"text":"A visible response after the limit."}
    ]));
    let mut profile = fixture.local_profile();
    profile.max_tokens = Some(128);
    profile.additional_params = Some(json!({"reasoning":{"effort":"high"}}));
    let mut engine = Engine::open(
        &fixture.data,
        &fixture.workspace,
        None,
        profile,
        "fixture".into(),
        options(),
    )
    .unwrap();
    let input = engine.post("Explain the task.", None).unwrap();
    finish(&mut engine, &input).await;
    assert_delivery(&engine, &input, "A visible response after the limit.");
    let events = engine.events().unwrap();
    let observation = events
        .iter()
        .find(|event| event.kind == "model_limited")
        .unwrap();
    assert_eq!(observation.data["usage"]["input_tokens"], 11);
    assert_eq!(observation.data["usage"]["output_tokens"], 128);
    assert_eq!(observation.data["usage"]["total_tokens"], 139);
    assert!(observation.data["provider_reported"]["max_output_tokens"].is_null());
    assert_eq!(fixture.request_count(), 2);
    for request in fixture.requests() {
        assert_eq!(request["body"]["max_output_tokens"], 128);
        assert_eq!(request["body"]["reasoning"]["effort"], "high");
    }
    assert!(events.iter().all(|event| event.kind != "summary"));
    assert_eq!(engine.state().budgets[&input].calls_used, 2);
    let metrics = engine.metrics(&input);
    assert_eq!(
        metrics["output_tokens"], 133,
        "limited generations must count toward usage"
    );
    assert_eq!(metrics["total_tokens"], 154);
    assert!(
        metrics["cost"].is_null(),
        "unreported costs must remain unknown"
    );
}

#[tokio::test]
async fn consecutive_length_pauses_and_restart_does_not_reset_recovery_or_call_budget() {
    let fixture = Fixture::turns(json!([
        limited(json!([write("first.txt", "never", "first-limit")])),
        limited(json!([write("second.txt", "never", "second-limit")])),
        {"text":"Unexpected third attempt"}
    ]));
    let mut engine = fixture.engine(
        None,
        RunOptions {
            max_calls: 3,
            ..options()
        },
    );
    let input = engine.post("Complete the retained work.", None).unwrap();
    finish(&mut engine, &input).await;
    assert_eq!(engine.result(&input).unwrap().kind, "failure");
    assert_eq!(engine.state().budgets[&input].calls_used, 2);
    assert_eq!(fixture.request_count(), 2);
    assert!(
        engine
            .events()
            .unwrap()
            .iter()
            .all(|event| event.kind != "tool_started")
    );
    let session = engine.state().id.clone();
    drop(engine);
    let mut reopened = fixture.engine(
        Some(&session),
        RunOptions {
            max_calls: 100,
            ..options()
        },
    );
    assert_eq!(reopened.state().budgets[&input].max_calls, 3);
    assert_eq!(reopened.state().budgets[&input].calls_used, 2);
    reopened.resume().unwrap();
    finish(&mut reopened, &input).await;
    assert_eq!(
        fixture.request_count(),
        2,
        "restart silently granted another recovery"
    );
    assert_eq!(reopened.state().budgets[&input].calls_used, 2);
    assert!(!fixture.workspace.join("first.txt").exists());
    assert!(!fixture.workspace.join("second.txt").exists());
}

#[tokio::test]
async fn content_filter_unknown_termination_and_missing_terminal_never_execute_tools() {
    for (reason, terminal) in [
        ("content_filter", true),
        ("max_tool_calls", true),
        ("eof", false),
    ] {
        let fixture = Fixture::turns(json!([
            {"response_status":"incomplete","incomplete_reason":reason,"omit_terminal":!terminal,
             "output":[write("forbidden.txt", "NEVER_EXECUTE", "forbidden-call")]},
            {"text":"Unexpected automatic replay"}
        ]));
        let mut engine = fixture.engine(None, options());
        let input = engine.post("Perform the action.", None).unwrap();
        finish(&mut engine, &input).await;
        assert_eq!(engine.result(&input).unwrap().kind, "failure", "{reason}");
        assert!(
            !fixture.workspace.join("forbidden.txt").exists(),
            "{reason}"
        );
        assert_eq!(fixture.request_count(), 1, "{reason} was retried");
        let events = engine.events().unwrap();
        assert!(
            events.iter().all(|event| event.kind != "tool_started"),
            "{reason}"
        );
        assert!(
            events.iter().any(|event| event.kind == "model_failed"),
            "{reason}"
        );
        assert!(
            events.iter().all(|event| event.kind != "model_limited"),
            "{reason} was classified as output exhaustion"
        );
    }
}

#[tokio::test]
async fn recovery_keeps_earlier_tool_results_without_replaying_their_effects() {
    let fixture = Fixture::turns(json!([
        {"output":[write("completed.txt", "already committed", "initial-write")]},
        limited(json!([write("discarded.txt", "never commit", "later-limit")])),
        {"contains":["initial-write","already committed"],"output":[proposal("read_file",json!({"path":"completed.txt"}),"verify-write")]},
        {"text":"Earlier work was verified."}
    ]));
    let mut engine = fixture.engine(None, options());
    let input = engine.post("Write and verify the result.", None).unwrap();
    finish(&mut engine, &input).await;
    assert_delivery(&engine, &input, "Earlier work was verified.");
    let events = engine.events().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "tool_started" && event.data["tool_name"] == "write_file")
            .count(),
        1
    );
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("completed.txt")).unwrap(),
        "already committed"
    );
    assert!(!fixture.workspace.join("discarded.txt").exists());
    assert_eq!(fixture.request_count(), 4);
}

async fn start_delayed_recovery(fixture: &Fixture, engine: &mut Engine, input: &str) {
    drive_until(engine, Duration::from_secs(5), |engine| {
        engine
            .events()
            .unwrap()
            .iter()
            .any(|event| event.kind == "model_limited")
    })
    .await;
    drive_until(engine, Duration::from_secs(5), |engine| {
        engine
            .events()
            .unwrap()
            .iter()
            .any(|event| event.kind == "model_started" && event.data["recovery_of"].is_string())
    })
    .await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while fixture.request_count() < 2 {
            let _ = tokio::time::timeout(Duration::from_millis(20), engine.step()).await;
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(engine.result(input).is_none());
}

#[tokio::test]
async fn stop_during_recovery_prevents_actions_and_does_not_refund_reserved_calls() {
    let fixture = Fixture::turns(json!([
        limited(json!([])),
        {"delay_seconds":1,"output":[write("stopped.txt", "stale", "stopped-write")]},
        {"text":"Unexpected restarted recovery"}
    ]));
    let mut engine = fixture.engine(None, options());
    let input = engine.post("Work until stopped.", None).unwrap();
    start_delayed_recovery(&fixture, &mut engine, &input).await;
    engine.stop().unwrap();
    assert_eq!(engine.state().budgets[&input].calls_used, 2);
    let cancelled = engine
        .events()
        .unwrap()
        .into_iter()
        .find(|event| event.kind == "cancelled")
        .unwrap();
    assert_eq!(cancelled.reply_to.as_deref(), Some(input.as_str()));
    assert!(
        cancelled.data["observations"]["observations"]
            .as_array()
            .is_some_and(|facts| !facts.is_empty()),
        "stop discarded the already observed native request boundary"
    );
    assert!(
        cancelled.data["usage"].is_null(),
        "unreported usage became a fabricated zero"
    );
    assert!(!fixture.workspace.join("stopped.txt").exists());
    let session = engine.state().id.clone();
    drop(engine);
    let mut reopened = fixture.engine(Some(&session), options());
    reopened.resume().unwrap();
    finish(&mut reopened, &input).await;
    assert_eq!(
        fixture.request_count(),
        2,
        "cancellation refunded the recovery allowance"
    );
    assert!(!fixture.workspace.join("stopped.txt").exists());
}

#[tokio::test]
async fn new_user_instruction_invalidates_a_recovery_proposal() {
    let fixture = Fixture::turns(json!([
        limited(json!([])),
        {"delay_seconds":1,"output":[write("obsolete.txt", "stale", "obsolete-write")]},
        {"contains":["NEW_INSTRUCTION"],"text":"Explained the revised request."}
    ]));
    let mut engine = fixture.engine(None, options());
    let original = engine.post("Perform the original task.", None).unwrap();
    start_delayed_recovery(&fixture, &mut engine, &original).await;
    let revised = engine
        .post("NEW_INSTRUCTION: explain only, make no file changes.", None)
        .unwrap();
    finish(&mut engine, &revised).await;
    assert_delivery(&engine, &revised, "Explained the revised request.");
    assert!(!fixture.workspace.join("obsolete.txt").exists());
    assert!(
        engine
            .events()
            .unwrap()
            .iter()
            .all(|event| event.kind != "tool_started")
    );
    assert_eq!(engine.state().budgets[&original].calls_used, 2);
}

#[tokio::test]
async fn failed_summary_preserves_prior_summary_and_sources_and_only_retries_smaller() {
    let text = "DETAIL_TO_RETAIN ".repeat(437);
    let cycle = |id: &str| {
        json!({"match_summary":false,"output":[
            {"type":"message","role":"assistant","content":[{"type":"output_text","text":text,"annotations":[]}]},
            proposal("read_file",json!({"path":"facts.txt"}),id)
        ]})
    };
    let fixture = Fixture::turns(json!([
        cycle("read-a"), cycle("read-b"), cycle("read-c"), cycle("read-d"),
        cycle("read-e"), cycle("read-f"), cycle("read-g"), cycle("read-h"),
        cycle("read-i"), cycle("read-j"), cycle("read-k"), cycle("read-l"),
        {"match_summary":true,"text":"PRIOR_SUMMARY: preserve original integer constraint and completed observations."},
        {"match_summary":true,"response_status":"incomplete","incomplete_reason":"max_output_tokens","text":"BAD_PARTIAL_SUMMARY_ONE"},
        {"match_summary":true,"response_status":"incomplete","incomplete_reason":"max_output_tokens","text":"BAD_PARTIAL_SUMMARY_TWO"},
        {"match_summary":false,"text":"Unexpected work after repeated summary failure"}
    ]));
    std::fs::write(fixture.workspace.join("facts.txt"), "ORIGINAL_SOURCE_FACTS").unwrap();
    let mut profile = fixture.local_profile();
    profile.max_tokens = Some(8192);
    let mut engine = Engine::open(
        &fixture.data,
        &fixture.workspace,
        None,
        profile,
        "fixture".into(),
        RunOptions {
            context_chars: 42_000,
            max_calls: 20,
            ..options()
        },
    )
    .unwrap();
    let input = engine
        .post(
            "EARLY_CONSTRAINT: use integers only; inspect twelve times then continue.",
            None,
        )
        .unwrap();
    finish(&mut engine, &input).await;
    assert_eq!(engine.result(&input).unwrap().kind, "failure");
    let events = engine.events().unwrap();
    let summaries: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "summary")
        .collect();
    assert_eq!(
        summaries.len(),
        1,
        "failed summaries changed working context"
    );
    assert!(
        engine.read_event(&summaries[0].id).unwrap().data["response"]
            .to_string()
            .contains("PRIOR_SUMMARY")
    );
    let job = engine.state().jobs.values().next().unwrap();
    assert_eq!(job.summary.as_ref(), Some(&summaries[0].id));
    let limited: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "model_limited" && event.data["purpose"] == "summary")
        .collect();
    assert_eq!(limited.len(), 2, "summary stopped before a smaller request: {}; calls={:?}",
        engine.event_text(engine.result(&input).unwrap()).unwrap(),
        events.iter().filter(|event| event.kind == "model_started").map(|event| json!({"purpose":event.data["purpose"],"source_chars":event.data["source_chars"]})).collect::<Vec<_>>());
    assert!(
        limited[1].data["source_chars"].as_u64().unwrap()
            <= limited[0].data["source_chars"].as_u64().unwrap() / 2
    );
    assert!(!job.history.contains(&limited[0].id));
    assert!(!job.history.contains(&limited[1].id));
    for event in events.iter().filter(|event| event.kind == "tool_result") {
        assert!(
            engine
                .read_event(&event.id)
                .unwrap()
                .data
                .to_string()
                .contains("ORIGINAL_SOURCE_FACTS")
        );
    }
    let requests = fixture.requests();
    let summary_requests: Vec<_> = requests
        .iter()
        .filter(|request| {
            request["body"]["instructions"]
                .as_str()
                .unwrap()
                .starts_with("Summarize this job")
        })
        .collect();
    assert_eq!(summary_requests.len(), 3);
    for request in summary_requests {
        assert_eq!(
            request["body"]["max_output_tokens"], 8192,
            "hidden summary cap remained"
        );
        assert!(request["body"].to_string().contains("EARLY_CONSTRAINT"));
    }
}

#[tokio::test]
async fn provider_context_limit_is_distinct_from_output_length_and_never_retries() {
    let fixture = Fixture::turns(json!([
        {"response_status":"incomplete","incomplete_reason":"model_context_window_exceeded",
         "output":[write("context-failure.txt", "never", "context-call")]},
        {"text":"Unexpected length recovery"}
    ]));
    let mut engine = fixture.engine(None, options());
    let input = engine.post("Process the supplied source.", None).unwrap();
    finish(&mut engine, &input).await;
    let result = engine.result(&input).unwrap();
    assert_eq!(result.kind, "failure");
    assert!(engine.event_text(result).unwrap().contains("context limit"));
    assert_eq!(fixture.request_count(), 1);
    assert!(!fixture.workspace.join("context-failure.txt").exists());
    let events = engine.events().unwrap();
    assert!(events.iter().all(|event| event.kind != "model_limited"));
    let failed = events
        .iter()
        .find(|event| event.kind == "model_failed")
        .unwrap();
    assert!(
        failed
            .data
            .to_string()
            .contains("model_context_window_exceeded")
    );
}

#[tokio::test]
async fn accepted_work_allows_a_later_independent_length_recovery() {
    let fixture = Fixture::turns(json!([
        limited(json!([])),
        {"output":[proposal("read_file",json!({"path":"facts.txt"}),"first-progress")]},
        limited(json!([])),
        {"output":[proposal("read_file",json!({"path":"facts.txt"}),"second-progress")]},
        {"text":"Both phases completed."}
    ]));
    std::fs::write(fixture.workspace.join("facts.txt"), "evidence").unwrap();
    let mut engine = fixture.engine(None, options());
    let input = engine
        .post("Inspect the evidence in two distinct phases.", None)
        .unwrap();
    finish(&mut engine, &input).await;
    assert_delivery(&engine, &input, "Both phases completed.");
    let events = engine.events().unwrap();
    let limited: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "model_limited")
        .collect();
    assert_eq!(limited.len(), 2);
    assert!(
        limited
            .iter()
            .all(|event| event.data["retry_allowed"] == true)
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "model_started" && event.data["recovery_of"].is_string())
            .count(),
        2
    );
    assert_eq!(fixture.request_count(), 5);
    assert_eq!(engine.state().budgets[&input].calls_used, 5);
}

#[tokio::test]
async fn successful_summary_does_not_grant_an_extra_work_recovery() {
    let fixture = Fixture::turns(json!([
        {"match_summary":false,"output":[
            {"type":"message","role":"assistant","content":[{"type":"output_text","text":"RECORDED_FINDING ".repeat(875),"annotations":[]}]},
            proposal("read_file",json!({"path":"facts.txt"}),"completed-cycle")
        ]},
        {"match_summary":false,"output":[
            {"type":"message","role":"assistant","content":[{"type":"output_text","text":"ANOTHER_FINDING ".repeat(875),"annotations":[]}]},
            proposal("read_file",json!({"path":"facts.txt"}),"another-completed-cycle")
        ]},
        {"match_summary":false,"response_status":"incomplete","incomplete_reason":"max_output_tokens","output":[]},
        {"match_summary":true,"text":"A concise summary of the completed read; the ACTIVE task remains unfinished."},
        {"match_summary":false,"response_status":"incomplete","incomplete_reason":"max_output_tokens","output":[]},
        {"match_summary":false,"text":"Unexpected third work attempt"}
    ]));
    std::fs::write(fixture.workspace.join("facts.txt"), "original evidence").unwrap();
    let mut engine = fixture.engine(
        None,
        RunOptions {
            context_chars: 64_000,
            ..options()
        },
    );
    let input = engine
        .post(
            "Preserve the original evidence and continue the task.",
            None,
        )
        .unwrap();
    drive_until(&mut engine, Duration::from_secs(5), |engine| {
        engine
            .events()
            .unwrap()
            .iter()
            .any(|event| event.kind == "model_limited" && event.data["purpose"] == "work")
    })
    .await;
    let session = engine.state().id.clone();
    drop(engine);
    let mut reopened = fixture.engine(
        Some(&session),
        RunOptions {
            context_chars: 32_000,
            ..options()
        },
    );
    reopened.resume().unwrap();
    finish(&mut reopened, &input).await;
    assert_eq!(reopened.result(&input).unwrap().kind, "failure");
    let events = reopened.events().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "summary")
            .count(),
        1
    );
    let limits: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "model_limited" && event.data["purpose"] == "work")
        .collect();
    assert_eq!(limits.len(), 2);
    assert_eq!(limits[0].data["retry_allowed"], true);
    assert_eq!(limits[1].data["retry_allowed"], false);
    assert_eq!(fixture.request_count(), 5);
    assert_eq!(reopened.state().budgets[&input].calls_used, 5);
}

#[tokio::test]
async fn file_can_be_created_then_appended_after_restart_without_replaying_creation() {
    use sha2::{Digest, Sha256};
    let first = "first complete portion\n";
    let second = "second complete portion\n";
    let hash = format!("{:x}", Sha256::digest(first.as_bytes()));
    let fixture = Fixture::turns(json!([
        {"output":[write("incremental.txt",first,"initial-portion")]},
        {"contains":[hash],"output":[proposal("write_file",json!({"path":"incremental.txt","content":second,
            "mode":"append","expected_sha256":hash}),"next-portion")]},
        {"text":"The complete file is saved."}
    ]));
    let mut engine = fixture.engine(None, options());
    let input = engine
        .post(
            "Create the file in complete portions and retain progress.",
            None,
        )
        .unwrap();
    drive_until(&mut engine, Duration::from_secs(5), |engine| {
        engine
            .events()
            .unwrap()
            .iter()
            .any(|event| event.kind == "tool_result" && event.data["tool_name"] == "write_file")
    })
    .await;
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("incremental.txt")).unwrap(),
        first
    );
    assert_eq!(engine.state().budgets[&input].calls_used, 1);
    let session = engine.state().id.clone();
    drop(engine);
    let mut reopened = fixture.engine(Some(&session), options());
    reopened.resume().unwrap();
    finish(&mut reopened, &input).await;
    assert_delivery(&reopened, &input, "The complete file is saved.");
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("incremental.txt")).unwrap(),
        format!("{first}{second}")
    );
    assert_eq!(fixture.request_count(), 3);
    assert_eq!(reopened.state().budgets[&input].calls_used, 3);
    assert_eq!(
        reopened
            .events()
            .unwrap()
            .iter()
            .filter(|event| event.kind == "tool_started" && event.data["tool_name"] == "write_file")
            .count(),
        2
    );
}
