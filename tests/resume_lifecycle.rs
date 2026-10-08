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
async fn superseding_old_work_cancels_its_unstarted_failed_child_without_requiring_success() {
    let mut fixture = Fixture::new(initial_turns(true));
    let mut engine = fixture.open(None);
    let original = engine
        .post("Delegate the original implementation", None)
        .unwrap();
    until(&mut engine, |engine| {
        engine.state().jobs.len() == 2
            && engine
                .state()
                .jobs
                .values()
                .all(|job| job.state == JobState::Paused)
    })
    .await;
    let (child_job, assigned) = child(&engine);
    assert_eq!(engine.result(&assigned).unwrap().kind, "failure");
    let original_budget = engine.state().budgets[&original].clone();
    fixture.append(vec![
        call("Conversation", "input_resolve", "supersede-original", json!({"resolutions":[{
            "input_id":original,"outcome":"superseded","reason":"The user explicitly abandoned this implementation and its delegated work"
        }]})),
        json!({"match_job_title":"Conversation","text":"The old implementation was abandoned; its failed child will not be retried."}),
    ]);
    let current = engine
        .post(
            "Abandon the original implementation and its child. Confirm the changed plan.",
            None,
        )
        .unwrap();
    until(&mut engine, |engine| engine.result(&current).is_some()).await;
    assert_eq!(engine.result(&current).unwrap().kind, "delivery");
    assert_eq!(
        engine.result(&original).unwrap().data["outcome"],
        "superseded"
    );
    assert_eq!(
        starts(&engine, &assigned),
        1,
        "supersession executed the failed child instead of abandoning it"
    );
    assert_eq!(engine.state().budgets[&original], original_budget);
    let session = engine.state().id.clone();
    drop(engine);
    let mut engine = fixture.open(Some(&session));
    engine.resume().unwrap();
    for _ in 0..8 {
        engine.step().await.unwrap();
    }
    assert_eq!(
        starts(&engine, &assigned),
        1,
        "resume resurrected a superseded child"
    );
    assert_ne!(engine.state().jobs[&child_job].state, JobState::Running);
    assert!(
        engine
            .read_event(&assigned)
            .unwrap()
            .data
            .to_string()
            .contains("Original delegated work")
    );
}

#[tokio::test]
async fn superseding_a_live_shell_then_resuming_retains_one_real_owned_terminal_result() {
    let command = if cfg!(windows) {
        "powershell.exe -NoProfile -NonInteractive -Command \"[Console]::Out.Write('observed-before-supersession'); [IO.File]::AppendAllText('effect.txt', 'x'); [Threading.Thread]::Sleep(10000); [IO.File]::WriteAllText('late.txt', 'must not execute')\""
    } else {
        "printf observed-before-supersession; printf x >> effect.txt; sleep 10; printf 'must not execute' > late.txt"
    };
    let mut fixture = Fixture::new(vec![
        call(
            "Conversation",
            "job_send",
            "assign-live-shell",
            json!({"title":"Child","message":"Execute the original shell once"}),
        ),
        call(
            "Child",
            "shell",
            "original-live-shell",
            json!({"command":command,"timeout_seconds":20}),
        ),
        json!({"match_job_title":"Conversation","match_last_user_contains":"ORIGINAL_LIVE_TOOL","delay_seconds":3,"text":"Old response must be cancelled"}),
    ]);
    fixture.options.read_only = false;
    let mut engine = fixture.open(None);
    let original = engine
        .post("ORIGINAL_LIVE_TOOL: delegate the operation once", None)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !fixture.local.workspace.join("effect.txt").exists() {
            let _ = tokio::time::timeout(Duration::from_millis(20), engine.step()).await;
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let (child_job, assigned) = child(&engine);
    let started = engine
        .events()
        .unwrap()
        .into_iter()
        .find(|event| event.kind == "tool_started" && event.data["tool_name"] == "shell")
        .unwrap();
    let original_call = started.call_id.clone();
    let original_budget = engine.state().budgets[&original].clone();
    fixture.append(vec![
        call("Conversation", "input_resolve", "supersede-live-shell", json!({"resolutions":[{
            "input_id":original,"outcome":"superseded","reason":"The user abandoned the original operation and its delegated work"
        }]})),
        json!({"match_job_title":"Conversation","text":"The old work was abandoned and its observed effects remain recorded."}),
    ]);
    let current = engine
        .post(
            "SUPERSEDE_LIVE_TOOL: abandon the old operation and its child.",
            None,
        )
        .unwrap();
    until(&mut engine, |engine| {
        engine
            .result(&original)
            .is_some_and(|event| event.data["outcome"] == "superseded")
    })
    .await;
    assert_eq!(engine.state().jobs[&child_job].current_call, original_call);
    assert!(
        !engine
            .events()
            .unwrap()
            .iter()
            .any(|event| event.kind == "tool_result" && event.call_id == original_call),
        "supersession invented a terminal result before draining the started tool"
    );
    engine.resume().unwrap();
    assert_eq!(
        engine.state().jobs[&child_job].current_call,
        original_call,
        "resume erased the live call's ownership"
    );
    assert!(
        !engine
            .events()
            .unwrap()
            .iter()
            .any(|event| event.kind == "tool_result" && event.call_id == original_call),
        "resume synthesized a second outcome while the tool was still live"
    );
    until(&mut engine, |engine| {
        engine
            .events()
            .unwrap()
            .iter()
            .any(|event| event.kind == "tool_result" && event.call_id == original_call)
    })
    .await;
    until(&mut engine, |engine| engine.result(&current).is_some()).await;
    assert_eq!(engine.result(&current).unwrap().kind, "delivery");
    let records = engine.events().unwrap();
    let results: Vec<_> = records
        .iter()
        .filter(|event| event.kind == "tool_result" && event.call_id == original_call)
        .collect();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].data["uncertain"], true);
    assert_eq!(results[0].job_id.as_deref(), Some(child_job.as_str()));
    assert_eq!(results[0].reply_to.as_deref(), Some(assigned.as_str()));
    assert_eq!(results[0].root_input.as_deref(), Some(original.as_str()));
    assert!(
        results[0]
            .data
            .to_string()
            .contains("observed-before-supersession")
    );
    assert_eq!(
        std::fs::read_to_string(fixture.local.workspace.join("effect.txt")).unwrap(),
        "x"
    );
    assert!(!fixture.local.workspace.join("late.txt").exists());
    assert_eq!(
        engine.state().budgets[&original],
        original_budget,
        "supersession or resume spent the old budget again"
    );
    let session = engine.state().id.clone();
    drop(engine);
    let mut engine = fixture.open(Some(&session));
    engine.resume().unwrap();
    for _ in 0..8 {
        engine.step().await.unwrap();
    }
    assert_eq!(
        engine
            .events()
            .unwrap()
            .iter()
            .filter(|event| event.kind == "tool_result" && event.call_id == original_call)
            .count(),
        1,
        "restart duplicated the real terminal result"
    );
    assert_eq!(
        engine
            .events()
            .unwrap()
            .iter()
            .filter(|event| event.kind == "tool_started" && event.data["tool_name"] == "shell")
            .count(),
        1
    );
    assert_eq!(engine.state().budgets[&original], original_budget);
    assert!(!fixture.local.workspace.join("late.txt").exists());
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
