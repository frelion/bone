//! Reproduce preserved inputs becoming active again after consolidated delivery.
use std::time::Duration;

use bone::{
    runtime::{Engine, RunOptions},
    state::JobState,
};
use serde_json::json;

mod support;
fn fixture(script: serde_json::Value) -> support::Fixture {
    support::Fixture::script(script, "fixtures/long_task/server.py")
}

async fn until_result(engine: &mut Engine, input: &str) {
    support::drive_until(engine, Duration::from_secs(5), |engine| {
        engine.result(input).is_some()
    })
    .await;
}

async fn until_started(engine: &mut Engine, input: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let events = engine.step().await.unwrap();
            if events.iter().any(|event| {
                event.kind == "model_started" && event.reply_to.as_deref() == Some(input)
            }) {
                break;
            }
        }
    })
    .await
    .expect("input never started");
}

#[tokio::test]
async fn consolidated_delivery_leaves_prior_inputs_runnable_after_restart() {
    let fixture = fixture(json!({"turns":[
        {"match_last_user_contains":"INITIAL_ENGINEERING_REQUEST", "delay_seconds":2, "text":"Initial result must be cancelled"},
        {"match_last_user_contains":"REVISED_ENGINEERING_REQUEST", "delay_seconds":2, "text":"Revised result must be cancelled"},
        {"match_last_user_contains":"CONTINUE_CONSOLIDATED_WORK", "text":"Completed the implementation including the revised contract and verification."},
        {"text":"Unexpected duplicate work"}
    ]}));
    let data = fixture.data.clone();
    let workspace = fixture.workspace.clone();
    let profile = fixture.local_profile();
    let options = RunOptions {
        single_job: true,
        read_only: true,
        max_calls: 8,
        ..Default::default()
    };
    let mut engine = Engine::open(
        &data,
        &workspace,
        None,
        profile.clone(),
        "fixture".into(),
        options.clone(),
    )
    .unwrap();
    let initial = engine
        .post(
            "INITIAL_ENGINEERING_REQUEST: implement history paging.",
            None,
        )
        .unwrap();
    until_started(&mut engine, &initial).await;
    let revised = engine
        .post(
            "REVISED_ENGINEERING_REQUEST: use the updated page bounds.",
            None,
        )
        .unwrap();
    until_started(&mut engine, &revised).await;
    engine.stop().unwrap();
    let session = engine.state().id.clone();
    assert!(engine.state().paused);
    drop(engine);

    let mut engine = Engine::open(
        &data,
        &workspace,
        Some(&session),
        profile,
        "fixture".into(),
        options,
    )
    .unwrap();
    let current = engine
        .post(
            "CONTINUE_CONSOLIDATED_WORK: finish the implementation under the latest contract.",
            None,
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while engine.result(&current).is_none() {
            engine.step().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(engine.result(&current).unwrap().kind, "delivery");
    let job = &engine.state().jobs[engine.state().focus.as_ref().unwrap()];
    assert_eq!(job.state, JobState::Ready);
    assert_eq!(
        job.inbox.iter().cloned().collect::<Vec<_>>(),
        vec![initial.clone(), revised.clone()]
    );
    assert!(engine.result(&initial).is_none());
    assert!(engine.result(&revised).is_none());

    until_started(&mut engine, &initial).await;
    let job = &engine.state().jobs[engine.state().focus.as_ref().unwrap()];
    assert_eq!(job.active_input.as_deref(), Some(initial.as_str()));
    assert_eq!(engine.state().budgets[&initial].calls_used, 2);
    assert_eq!(engine.state().budgets[&revised].calls_used, 1);
    assert_eq!(engine.state().budgets[&current].calls_used, 1);
    // Preserve the present failure as a deterministic characterization, rather
    // than silently clearing unrelated work or injecting a successful result.
    engine.stop().unwrap();
}

#[tokio::test]
async fn explicit_resolution_survives_restart_and_preserves_independent_queued_input() {
    let fixture = fixture(json!({"reload_script":true,"turns":[
        {"match_last_user_contains":"INITIAL", "delay_seconds":2,"text":"cancelled original"},
        {"match_last_user_contains":"REVISED", "delay_seconds":2,"text":"cancelled revision"}
    ]}));
    let data = fixture.data.clone();
    let workspace = fixture.workspace.clone();
    let profile = fixture.local_profile();
    std::fs::write(workspace.join("evidence.txt"), "IMPLEMENTATION_VERIFIED").unwrap();
    let options = RunOptions {
        single_job: true,
        read_only: true,
        max_calls: 8,
        ..Default::default()
    };
    let mut engine = Engine::open(
        &data,
        &workspace,
        None,
        profile.clone(),
        "fixture".into(),
        options.clone(),
    )
    .unwrap();
    let initial = engine
        .post("INITIAL: implement the original request", None)
        .unwrap();
    until_started(&mut engine, &initial).await;
    let revised = engine
        .post("REVISED: adjust the original bounds", None)
        .unwrap();
    until_started(&mut engine, &revised).await;
    engine.stop().unwrap();
    let session = engine.state().id.clone();
    drop(engine);
    let mut engine = Engine::open(
        &data,
        &workspace,
        Some(&session),
        profile.clone(),
        "fixture".into(),
        options.clone(),
    )
    .unwrap();
    let independent = engine
        .post("INDEPENDENT: another task must remain runnable", None)
        .unwrap();
    let current = engine
        .post(
            "RESOLVE_CONSOLIDATED: finish the original work under the current bounds",
            None,
        )
        .unwrap();
    std::fs::write(&fixture.responses,serde_json::to_vec(&json!({"reload_script":true,"turns":[
        {"output":[{"type":"function_call","name":"read_file","call_id":"verify","arguments":{"path":"evidence.txt"}}]},
        {"contains":["IMPLEMENTATION_VERIFIED"],"output":[{"type":"function_call","name":"input_resolve","call_id":"settle","arguments":{"resolutions":[
            {"input_id":initial,"outcome":"completed","reason":"The implementation evidence was inspected and verifies the original work"},
            {"input_id":revised,"outcome":"superseded","reason":"The latest active contract replaces the intermediate bounds"}
        ]}}]},
        {"contains":["input_resolve","superseded","completed"],"text":"Consolidated work completed"},
        {"text":"Independent task completed"}
    ]})).unwrap()).unwrap();
    until_result(&mut engine, &current).await;
    assert_eq!(
        engine.result(&current).unwrap().kind,
        "delivery",
        "{}",
        engine.event_text(engine.result(&current).unwrap()).unwrap()
    );
    for (input, outcome) in [(&initial, "completed"), (&revised, "superseded")] {
        let result = engine.result(input).unwrap();
        assert_eq!(result.kind, "input_resolved");
        assert_eq!(result.reply_to.as_deref(), Some(input.as_str()));
        assert_eq!(result.root_input.as_deref(), Some(input.as_str()));
        assert_eq!(result.data["actor_input"], current);
        assert_eq!(result.data["outcome"], outcome);
        assert!(!engine.event_text(result).unwrap().is_empty());
        assert_eq!(engine.read_event(input).unwrap().kind, "input");
    }
    let owner = engine.state().focus.clone().unwrap();
    assert_eq!(
        engine.state().jobs[&owner]
            .inbox
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        vec![independent.clone()]
    );
    let budgets = engine.state().budgets.clone();
    assert_eq!(budgets[&initial].calls_used, 1);
    assert_eq!(budgets[&revised].calls_used, 1);
    assert_eq!(budgets[&independent].calls_used, 0);
    drop(engine);
    let mut engine = Engine::open(
        &data,
        &workspace,
        Some(&session),
        profile,
        "fixture".into(),
        options,
    )
    .unwrap();
    assert_eq!(engine.state().budgets, budgets);
    assert_eq!(
        engine.result(&initial).unwrap().data["outcome"],
        "completed"
    );
    assert_eq!(
        engine.result(&revised).unwrap().data["outcome"],
        "superseded"
    );
    engine.resume().unwrap();
    until_result(&mut engine, &independent).await;
    assert_eq!(engine.result(&independent).unwrap().kind, "delivery");
    assert_eq!(engine.state().jobs[&owner].state, JobState::Idle);
    assert_eq!(engine.state().budgets[&initial], budgets[&initial]);
    assert_eq!(engine.state().budgets[&revised], budgets[&revised]);
    let events = engine.events().unwrap();
    for input in [&initial, &revised] {
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "model_started"
                    && event.reply_to.as_deref() == Some(input))
                .count(),
            1
        );
    }
    let requests = std::fs::read_to_string(&fixture.requests).unwrap();
    assert!(requests.contains("input_resolve"));
    assert!(requests.contains("settle"));
}
