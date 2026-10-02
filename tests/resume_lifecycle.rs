//! Public API boundaries for default recovery versus explicit retry of retained work.
use bone::{
    runtime::{Engine, RunOptions},
    state::JobState,
};
use serde_json::{Value, json};
use std::time::Duration;

mod support;
struct Fixture {
    local: support::Fixture,
    turns: Vec<Value>,
    options: RunOptions,
}
impl Fixture {
    fn new(turns: Vec<Value>) -> Self {
        Self {
            local: support::Fixture::script(
                json!({"reload_script":true,"turns":turns}),
                "fixtures/long_task/server.py",
            ),
            turns,
            options: RunOptions {
                read_only: true,
                max_parallel: 3,
                max_calls: 24,
                ..Default::default()
            },
        }
    }
    fn open(&self, session: Option<&str>) -> Engine {
        self.local.engine(session, self.options.clone())
    }
    fn append(&mut self, turns: Vec<Value>) {
        self.turns.extend(turns);
        std::fs::write(
            &self.local.responses,
            serde_json::to_vec(&json!({"reload_script":true,"turns":self.turns})).unwrap(),
        )
        .unwrap();
    }
}
fn call(title: &str, name: &str, id: &str, args: Value) -> Value {
    json!({"match_job_title":title,"output":[{"type":"function_call","name":name,"call_id":id,"arguments":args}]})
}
fn initial_turns(parent_failed: bool) -> Vec<Value> {
    let mut turns = vec![
        call(
            "Conversation",
            "job_send",
            "assign",
            json!({"title":"Child","message":"Original delegated work"}),
        ),
        json!({"match_job_title":"Child","text":""}),
    ];
    if !parent_failed {
        turns.push(call(
            "Conversation",
            "job_wait",
            "wait-original",
            json!({"input_ids":["$latest_input_id"]}),
        ));
    }
    turns.push(json!({"match_job_title":"Conversation","text":if parent_failed { "" } else { "The delegated work failed; I am reporting that failure rather than claiming success." }}));
    turns
}
async fn until(engine: &mut Engine, predicate: impl Fn(&Engine) -> bool) {
    support::drive_until(engine, Duration::from_secs(8), predicate).await;
}

fn child(engine: &Engine) -> (String, String) {
    let job = engine
        .state()
        .jobs
        .values()
        .find(|j| j.title == "Child")
        .unwrap();
    (job.id.clone(), job.active_input.clone().unwrap())
}
fn starts(engine: &Engine, input: &str) -> usize {
    engine
        .events()
        .unwrap()
        .iter()
        .filter(|e| e.kind == "model_started" && e.reply_to.as_deref() == Some(input))
        .count()
}
async fn parent_delivery(fixture: &Fixture) -> (Engine, String, String, String) {
    let mut engine = fixture.open(None);
    let parent = engine
        .post("Delegate the work; report any failure honestly.", None)
        .unwrap();
    until(&mut engine, |e| e.result(&parent).is_some()).await;
    assert_eq!(engine.result(&parent).unwrap().kind, "delivery");
    let (job, input) = child(&engine);
    assert_eq!(engine.state().jobs[&job].state, JobState::Paused);
    assert_eq!(engine.result(&input).unwrap().kind, "failure");
    (engine, parent, job, input)
}

#[tokio::test]
async fn ended_parent_does_not_reactivate_failed_child_on_resume_or_restart() {
    let fixture = Fixture::new(initial_turns(false));
    let (mut engine, parent, job, input) = parent_delivery(&fixture).await;
    let budget = engine.state().budgets[&parent].clone();
    let original_failure = engine.result(&input).unwrap().id.clone();
    engine.resume().unwrap();
    for _ in 0..8 {
        engine.step().await.unwrap();
    }
    assert_eq!(starts(&engine, &input), 1);
    assert_eq!(engine.state().jobs[&job].state, JobState::Paused);
    assert_eq!(
        engine.state().jobs[&job].active_input.as_deref(),
        Some(input.as_str())
    );
    let session = engine.state().id.clone();
    drop(engine);
    let mut engine = fixture.open(Some(&session));
    engine.resume().unwrap();
    for _ in 0..8 {
        engine.step().await.unwrap();
    }
    assert_eq!(starts(&engine, &input), 1);
    assert_eq!(engine.result(&input).unwrap().id, original_failure);
    assert_eq!(engine.state().budgets[&parent], budget);
}

#[tokio::test]
async fn failure_without_parent_delivery_preserves_normal_resume() {
    let mut fixture = Fixture::new(initial_turns(true));
    let mut engine = fixture.open(None);
    let parent = engine.post("Delegate an unfinished task", None).unwrap();
    until(&mut engine, |e| {
        e.state().jobs.len() == 2 && e.state().jobs.values().all(|j| j.state == JobState::Paused)
    })
    .await;
    let (job, input) = child(&engine);
    assert_eq!(engine.result(&parent).unwrap().kind, "failure");
    assert_eq!(engine.result(&input).unwrap().kind, "failure");
    let used = engine.state().budgets[&parent].calls_used;
    fixture.append(vec![
        json!({"match_job_title":"Conversation","delay_seconds":2,"text":"Recovered parent"}),
        json!({"match_job_title":"Child","delay_seconds":2,"text":"Recovered child"}),
    ]);
    engine.resume().unwrap();
    until(&mut engine, |e| {
        starts(e, &parent) == 3 && starts(e, &input) == 2
    })
    .await;
    assert_eq!(
        engine.state().jobs[&job].active_input.as_deref(),
        Some(input.as_str())
    );
    assert_eq!(engine.state().budgets[&parent].calls_used, used + 2);
    engine.stop().unwrap();
}

#[tokio::test]
async fn unrelated_input_still_resumes_after_stop_and_restart() {
    let fixture = Fixture::new(vec![
        json!({"delay_seconds":2,"text":"Cancelled response"}),
        json!({"text":"Completed after ordinary restart"}),
    ]);
    let mut engine = fixture.open(None);
    let input = engine.post("Independent work", None).unwrap();
    until(&mut engine, |e| starts(e, &input) == 1).await;
    engine.stop().unwrap();
    let session = engine.state().id.clone();
    drop(engine);
    let mut engine = fixture.open(Some(&session));
    engine.resume().unwrap();
    until(&mut engine, |e| e.result(&input).is_some()).await;
    assert_eq!(engine.result(&input).unwrap().kind, "delivery");
    assert_eq!(starts(&engine, &input), 2);
    assert_eq!(engine.state().budgets[&input].calls_used, 2);
}

#[tokio::test]
async fn real_followup_runs_before_retained_failed_assignment_and_does_not_revive_it() {
    let mut fixture = Fixture::new(initial_turns(false));
    let (mut engine, parent, job, old) = parent_delivery(&fixture).await;
    let old_budget = engine.state().budgets[&parent].clone();
    fixture.append(vec![
        call(
            "Conversation",
            "job_send",
            "new-assignment",
            json!({"job_id":job,"message":"Independent followup, not retry of old failure"}),
        ),
        json!({"match_job_title":"Child","text":"Independent followup completed"}),
        call(
            "Conversation",
            "job_wait",
            "wait-new",
            json!({"input_ids":["$latest_input_id"]}),
        ),
        json!({"match_job_title":"Conversation","text":"Independent followup verified"}),
    ]);
    let followup = engine
        .post("Send the child a different independent assignment", None)
        .unwrap();
    until(&mut engine, |e| e.result(&followup).is_some()).await;
    let assigned = engine
        .events()
        .unwrap()
        .into_iter()
        .find(|e| e.kind == "input" && e.data["sender_input"] == followup)
        .unwrap()
        .id;
    assert_eq!(engine.result(&assigned).unwrap().kind, "delivery");
    assert_eq!(starts(&engine, &old), 1);
    assert!(engine.state().jobs[&job].inbox.contains(&old));
    assert_eq!(engine.state().jobs[&job].state, JobState::Paused);
    assert_eq!(engine.state().budgets[&parent], old_budget);
    engine.resume().unwrap();
    for _ in 0..8 {
        engine.step().await.unwrap();
    }
    assert_eq!(starts(&engine, &old), 1);
    let session = engine.state().id.clone();
    drop(engine);
    let mut engine = fixture.open(Some(&session));
    engine.resume().unwrap();
    for _ in 0..8 {
        engine.step().await.unwrap();
    }
    assert_eq!(starts(&engine, &old), 1);
    assert!(engine.state().jobs[&job].inbox.contains(&old));
}

#[tokio::test]
async fn explicit_retry_can_make_new_delegation_after_ancestor_delivery_without_new_budget() {
    let mut fixture = Fixture::new(initial_turns(false));
    let (mut engine, parent, job, old) = parent_delivery(&fixture).await;
    let used = engine.state().budgets[&parent].calls_used;
    fixture.append(vec![
        call("Conversation","job_control","retry",json!({"job_id":job,"state":"ready"})),
        call("Child","job_send","fresh-grandchild",json!({"title":"Grandchild","message":"Fresh delegation authorized during explicit retry"})),
        json!({"match_job_title":"Grandchild","text":"Fresh delegation completed"}),
        call("Child","job_wait","wait-grandchild",json!({"input_ids":["$latest_input_id"]})),
        json!({"match_job_title":"Child","text":"Original assignment now completed"}),
        call("Conversation","job_wait","wait-retry",json!({"input_ids":[old]})),
        json!({"match_job_title":"Conversation","text":"Explicit retry verified"}),
    ]);
    let retry = engine
        .post(
            "Explicitly authorize a retry of the retained child work",
            None,
        )
        .unwrap();
    until(&mut engine, |e| e.result(&retry).is_some()).await;
    assert_eq!(engine.result(&old).unwrap().kind, "delivery");
    assert_eq!(starts(&engine, &old), 4);
    let events = engine.events().unwrap();
    let grandchild = events
        .iter()
        .find(|e| e.kind == "input" && e.data["sender_input"] == old)
        .unwrap();
    assert_eq!(grandchild.root_input.as_deref(), Some(parent.as_str()));
    assert_eq!(engine.result(&grandchild.id).unwrap().kind, "delivery");
    assert_eq!(starts(&engine, &grandchild.id), 1);
    assert_eq!(engine.state().budgets[&parent].calls_used, used + 4);
    assert!(!engine.state().budgets.contains_key(&old));
    assert!(!engine.state().budgets.contains_key(&grandchild.id));
}

#[tokio::test]
async fn explicit_retry_does_not_survive_a_subsequent_stop_and_restart() {
    let mut fixture = Fixture::new(initial_turns(false));
    let (mut engine, parent, job, old) = parent_delivery(&fixture).await;
    let used = engine.state().budgets[&parent].calls_used;
    fixture.append(vec![
        call("Conversation", "job_control", "retry-once", json!({"job_id":job,"state":"ready"})),
        json!({"match_job_title":"Child","delay_seconds":2,"text":"Cancelled explicit retry"}),
        call("Conversation", "job_wait", "wait-interrupted-retry", json!({"input_ids":[old]})),
        json!({"match_job_title":"Conversation","text":"The interrupted retry remains paused; another explicit authorization is required."}),
    ]);
    let retry = engine
        .post("Explicitly retry the child once", None)
        .unwrap();
    until(&mut engine, |e| starts(e, &old) == 2).await;
    engine.stop().unwrap();
    let session = engine.state().id.clone();
    drop(engine);
    let mut engine = fixture.open(Some(&session));
    engine.resume().unwrap();
    until(&mut engine, |e| e.result(&retry).is_some()).await;
    assert_eq!(starts(&engine, &old), 2);
    assert_eq!(engine.state().jobs[&job].state, JobState::Paused);
    assert_eq!(
        engine.state().jobs[&job].active_input.as_deref(),
        Some(old.as_str())
    );
    assert_eq!(engine.state().budgets[&parent].calls_used, used + 1);
    assert_eq!(engine.result(&old).unwrap().kind, "failure");
}
