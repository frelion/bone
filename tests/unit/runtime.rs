use super::*;

#[test]
fn profile_switch_validates_before_mutation_and_preserves_paused_session() {
    let (_directory, mut engine) = fixture_engine();
    let mut invalid = Profile::from_model("ollama:next").unwrap();
    invalid.additional_params = Some(json!(false));
    assert!(engine.set_profile(invalid, "next".into()).is_err());
    assert_eq!(engine.profile_name, "fixture");
    engine.post("remain paused", None).unwrap();
    engine.stop().unwrap();
    let before = serde_json::to_value(&engine.state).unwrap();
    engine
        .set_profile(Profile::from_model("ollama:next").unwrap(), "next".into())
        .unwrap();
    assert_eq!(engine.profile_name, "next");
    assert_eq!(serde_json::to_value(&engine.state).unwrap(), before);
}

#[tokio::test]
async fn profile_switch_preserves_an_in_flight_call_and_changes_future_recipe() {
    let (_directory, mut engine) = fixture_engine();
    let input = engine.post("keep working", None).unwrap();
    engine.prepare_pending_input().unwrap();
    let job = engine.state.focus.clone().unwrap();
    let origin = Origin {
        job: job.clone(),
        input: input.clone(),
        root: input,
        call: new_id(),
        revision: engine.state.revision,
    };
    let task = tokio::spawn(std::future::pending::<()>());
    engine.running.insert(
        origin.call.clone(),
        Running {
            origin: origin.clone(),
            abort: task.abort_handle(),
            tool: None,
            cancellation: None,
        },
    );
    engine.state.jobs.get_mut(&job).unwrap().state = JobState::Running;
    engine.state.jobs.get_mut(&job).unwrap().current_call = Some(origin.call.clone());
    let before = engine.state.clone();
    assert!(!engine.is_quiescent());
    engine
        .set_profile(Profile::from_model("ollama:next").unwrap(), "next".into())
        .unwrap();
    assert_eq!(engine.profile_recipe().0, "next");
    assert_eq!(engine.state, before);
    assert!(engine.running.contains_key(&origin.call));
    assert!(!task.is_finished());
    task.abort();
}

fn progress_origin(revision: u64) -> Origin {
    Origin {
        job: "job".into(),
        input: "input".into(),
        root: "root".into(),
        call: "call".into(),
        revision,
    }
}

#[test]
fn model_progress_is_opt_in_bounded_lossy_and_preserves_native_item() {
    use rig_core::streaming::{Item, UnknownPayload};
    let content = serde_json::to_value(rig_core::message::AssistantContent::text("done")).unwrap();
    assert_eq!(content, json!({"type":"text","text":"done"}));
    let transcript = json!([
        {"item":"event","value":{"event":"start","part":0,"kind":"text"}},
        {"item":"event","value":{"event":"text","part":0,"text":"done"}},
        {"item":"event","value":{"event":"end","part":0,"content":content}}
    ]);
    let native = rig_core::streaming::Transcript::parse(transcript.clone()).unwrap();
    assert_eq!(serde_json::to_value(native).unwrap(), transcript);
    let observer = ProgressObserver::default();
    let native = Item::<rig_core::streaming::StreamEvent>::Unknown(UnknownPayload::from(
        json!({"provider_field":"native"}),
    ));
    let item = serde_json::to_value(native).unwrap();
    assert_eq!(
        item,
        json!({"item":"unknown","value":{"provider_field":"native"}})
    );
    observer.push(&progress_origin(3), "work", &item, 64);
    assert!(observer.queue.lock().unwrap().is_empty());
    assert!(observer.drain(3, false).is_empty());
    for _ in 0..MODEL_PROGRESS_RECORDS + 2 {
        observer.push(&progress_origin(3), "work", &item, 64);
    }
    observer.push(
        &progress_origin(3),
        "work",
        &item,
        MODEL_PROGRESS_ITEM_BYTES + 1,
    );
    assert_eq!(observer.queue.lock().unwrap().len(), MODEL_PROGRESS_RECORDS);
    let records = observer.drain(3, false);
    assert_eq!(records.len(), MODEL_PROGRESS_RECORDS);
    assert_eq!(records[0].item, item);
    assert_eq!(records[0].job_id, "job");
    assert_eq!(records[0].call_id, "call");
    assert_eq!(records[0].purpose, "work");
    let guard = observer.queue.lock().unwrap();
    observer.push(&progress_origin(3), "work", &item, 64);
    drop(guard);
    assert!(observer.drain(3, false).is_empty());
}

#[test]
fn model_progress_drops_stale_revisions_and_stop_clears_it() {
    let (_directory, mut engine) = fixture_engine();
    engine.drain_model_progress();
    let revision = engine.state.revision;
    let item = json!({"item":"event","value":{"event":"text","part":0,"text":"native"}});
    engine
        .model_progress
        .push(&progress_origin(revision + 1), "work", &item, 80);
    assert!(engine.drain_model_progress().is_empty());
    engine
        .model_progress
        .push(&progress_origin(revision), "work", &item, 80);
    engine.stop().unwrap();
    assert!(engine.model_progress.queue.lock().unwrap().is_empty());
    // A racing producer from the aborted call cannot reappear after stop.
    engine
        .model_progress
        .push(&progress_origin(revision), "work", &item, 80);
    assert!(engine.drain_model_progress().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aborted_write_keeps_ownership_until_its_future_actually_exits() {
    let workspace = tempfile::tempdir().unwrap();
    let lease = WriteLease::acquire(workspace.path()).unwrap().unwrap();
    let task_lock = Arc::clone(&lease.file);
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let task = tokio::spawn(async move {
        let _workspace_ownership = task_lock;
        entered.send(()).unwrap();
        // Like a synchronous file operation, this cannot observe abort
        // until its current poll finishes.
        let _ = blocked.recv();
    });
    started.await.unwrap();
    task.abort();
    assert!(!task.is_finished());
    drop(lease);
    // Aborting a task cannot release ownership while its poll still runs.
    assert!(WriteLease::acquire(workspace.path()).unwrap().is_none());
    release.send(()).unwrap();
    let _ = task.await;
    let next = WriteLease::acquire(workspace.path()).unwrap().unwrap();
    drop(next);
    assert!(WriteLease::acquire(workspace.path()).unwrap().is_some());
}

#[tokio::test]
async fn stop_collects_tool_result_before_releasing_ownership_and_allows_new_work() {
    let (_directory, mut engine) = fixture_engine();
    let input = engine.post("run a build", None).unwrap();
    engine.prepare_pending_input().unwrap();
    let job = engine.state.focus.clone().unwrap();
    let command = if cfg!(windows) {
        "powershell.exe -NoProfile -NonInteractive -Command \"[Console]::Write('before stop'); [Console]::Out.Flush(); [IO.File]::WriteAllText('ready.txt', 'ready'); while ($true) { [Threading.Thread]::Sleep(10) }\""
    } else {
        "printf 'before stop'; printf 'ready' > ready.txt; while :; do sleep 0.01; done"
    };
    engine.drain_tool_progress();
    let tool = native_proposal(&mut engine, &job, "shell", json!({"command":command}));
    engine.start_tool(&job, tool).unwrap();
    let call = engine.state.jobs[&job].current_call.clone().unwrap();
    let ready = engine.state.workspace.join("ready.txt");
    // Live previews are lossy. A physical marker after stdout is flushed proves
    // the command actually started, independently of observation timing.
    tokio::time::timeout(Duration::from_secs(30), async {
        while std::fs::read_to_string(&ready).ok().as_deref() != Some("ready") {
            if engine.running[&call].abort.is_finished() {
                while engine.running.contains_key(&call) {
                    engine.step().await.unwrap();
                }
                panic!(
                    "shell ended before readiness: {}",
                    engine.read_call_event(&call, "tool_result").unwrap().data
                );
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    engine.stop().unwrap();
    assert!(engine.state.paused);
    assert_eq!(engine.state.jobs[&job].state, JobState::Running);
    assert_eq!(engine.state.jobs[&job].current_call.as_ref(), Some(&call));
    assert!(!engine.is_quiescent());
    assert!(
        WriteLease::acquire(&engine.state.workspace)
            .unwrap()
            .is_none()
    );
    engine.resume().unwrap();
    assert_eq!(engine.state.jobs[&job].current_call.as_ref(), Some(&call));
    assert!(engine.running.contains_key(&call));
    engine
        .post("Use the new constraint after stopping", None)
        .unwrap();
    engine.prepare_pending_input().unwrap();
    assert_eq!(engine.state.jobs[&job].active_input.as_ref(), Some(&input));
    assert_eq!(engine.state.jobs[&job].current_call.as_ref(), Some(&call));
    assert!(
        engine
            .events()
            .unwrap()
            .iter()
            .all(|event| event.kind != "tool_result")
    );
    engine.stop().unwrap();
    // Allow the bounded child wait, pipe drain and Windows job cleanup to finish.
    tokio::time::timeout(Duration::from_secs(20), async {
        while !engine.is_quiescent() {
            engine.step().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(engine.state.jobs[&job].state, JobState::Paused);
    assert!(engine.state.jobs[&job].current_call.is_none());
    let results = engine
        .events()
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "tool_result" && event.call_id.as_deref() == Some(&call))
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].reply_to.as_ref(), Some(&input));
    assert_eq!(results[0].data["uncertain"], true);
    let text = engine.readable_event(&results[0]).unwrap();
    assert!(text.contains("before stop"));
    assert!(text.contains("interrupted"));

    engine.post("write the next result", None).unwrap();
    engine.prepare_pending_input().unwrap();
    let tool = native_proposal(
        &mut engine,
        &job,
        "write_file",
        json!({
            "path":"after-stop.txt","content":"new work","expected_sha256":null,
        }),
    );
    engine.start_tool(&job, tool).unwrap();
    let done = engine.tasks.join_next().await.unwrap().unwrap();
    engine.complete(done).unwrap();
    assert_eq!(
        std::fs::read_to_string(engine.state.workspace.join("after-stop.txt")).unwrap(),
        "new work"
    );
    assert!(engine.pending_tools(&job).unwrap().is_empty());
}

#[test]
fn wait_cycle_uses_input_owners_and_ignores_already_terminal_replies() {
    let mut state = SessionState::new(".");
    let mut first = Job::new("first");
    let mut second = Job::new("second");
    let mut third = Job::new("third");
    first.active_input = Some("input-a".into());
    second.active_input = Some("input-b".into());
    third.inbox.push_back("input-c".into());
    first.wait_for = vec!["input-b".into(), "input-c".into()];
    second.wait_for = vec!["input-a".into()];
    for job in [first, second, third] {
        state.jobs.insert(job.id.clone(), job);
    }
    assert!(has_wait_cycle(&state, |_| false));
    assert!(!has_wait_cycle(&state, |input| input == "input-b"));
    // A reply for another input cannot discharge this dependency.
    assert!(has_wait_cycle(&state, |input| input == "other-input-b"));
}

fn open_fixture_engine(directory: &Path, workspace: &Path, session: Option<&str>) -> Engine {
    Engine::open(
        &directory.join("data"),
        workspace,
        session,
        Profile::from_model("ollama:unused-local-fixture").unwrap(),
        "fixture".into(),
        RunOptions::default(),
    )
    .unwrap()
}

fn fixture_engine() -> (tempfile::TempDir, Engine) {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let engine = open_fixture_engine(directory.path(), &workspace, None);
    (directory, engine)
}

#[test]
fn resumed_input_hides_historical_failure_until_new_result() {
    let (_directory, mut engine) = fixture_engine();
    let input = engine.post("Complete the implementation", None).unwrap();
    engine.prepare_pending_input().unwrap();
    let job = engine.state().focus.clone().unwrap();
    engine.fail(&job, "temporary provider failure").unwrap();
    let failure = engine.result(&input).unwrap().clone();
    assert_eq!(failure.kind, "failure");

    engine.resume().unwrap();
    assert!(engine.result(&input).is_none());
    assert!(engine.terminal(&input).is_none());
    assert_eq!(engine.read_event(&failure.id).unwrap().kind, "failure");

    let response = engine.event(
        &job,
        "model_message",
        json!({"response":native_response(Message::assistant("Completed and verified"))}),
    );
    let response_id = response.id.clone();
    engine.append_history(&job, response).unwrap();
    engine.deliver(&job, &response_id).unwrap();
    let delivered = engine.result(&input).unwrap();
    assert_eq!(delivered.kind, "delivery");
    assert_eq!(
        engine.event_text(delivered).unwrap(),
        "Completed and verified"
    );
    assert!(
        engine
            .events()
            .unwrap()
            .iter()
            .any(|event| event.id == failure.id)
    );
}

#[tokio::test]
async fn mixed_queue_runs_internal_assignment_then_parks_old_user_until_resume() {
    let (_directory, mut engine) = fixture_engine();
    let older = engine.post("Original engineering work", None).unwrap();
    engine.prepare_pending_input().unwrap();
    let newest = engine.post("Explain the current design", None).unwrap();
    engine.prepare_pending_input().unwrap();
    let job = engine.state.focus.clone().unwrap();
    let mut assignment = engine.event(
        &job,
        "input",
        json!({"message":Message::user("Independent internal assignment"),"source":"job","sender_input":older}),
    );
    assignment.reply_to = None;
    assignment.root_input = Some(older.clone());
    let assigned = assignment.id.clone();
    let mut state = engine.state.clone();
    state
        .jobs
        .get_mut(&job)
        .unwrap()
        .inbox
        .push_back(assigned.clone());
    engine.commit(state, vec![assignment]).unwrap();
    assert_eq!(
        engine.state.jobs[&job].inbox,
        VecDeque::from([older.clone(), assigned.clone()])
    );

    let response = engine.event(
        &job,
        "model_message",
        json!({
            "response":native_response(Message::assistant("Here is the explanation")),
        }),
    );
    let response_id = response.id.clone();
    engine.append_history(&job, response).unwrap();
    engine.admit_input(&job, &newest).unwrap();
    engine.deliver(&job, &response_id).unwrap();
    assert_eq!(engine.state.jobs[&job].state, JobState::Ready);
    engine.schedule().unwrap();
    assert_eq!(
        engine.state.jobs[&job].active_input.as_ref(),
        Some(&assigned)
    );
    assert_eq!(
        engine.state.jobs[&job].inbox,
        VecDeque::from([older.clone()])
    );

    let origin = engine
        .running
        .values()
        .find(|running| running.origin.input == assigned)
        .unwrap()
        .origin
        .clone();
    engine
        .complete(Completed::Model {
            origin,
            response: Ok(native_response(Message::assistant(
                "Internal assignment completed",
            ))),
            covered: None,
            stream_items: vec![],
        })
        .unwrap();
    assert_eq!(engine.state.jobs[&job].state, JobState::Paused);
    assert!(engine.state.jobs[&job].active_input.is_none());
    assert_eq!(
        engine.state.jobs[&job].inbox,
        VecDeque::from([older.clone()])
    );
    engine.schedule().unwrap();
    assert!(engine.state.jobs[&job].active_input.is_none());
    assert!(
        engine
            .events()
            .unwrap()
            .iter()
            .all(|event| event.kind != "model_started" || event.reply_to.as_ref() != Some(&older))
    );

    engine.resume().unwrap();
    engine.schedule().unwrap();
    assert_eq!(engine.state.jobs[&job].active_input.as_ref(), Some(&older));
    assert_eq!(engine.state.jobs[&job].state, JobState::Running);
    engine.stop().unwrap();
}

fn native_response(message: Message) -> CompletionResponse {
    let Message::Assistant { content, .. } = message else {
        panic!("expected assistant")
    };
    CompletionResponse::new(
        content,
        rig_core::completion::Usage::default(),
        "ollama",
        json!({}),
    )
}

fn native_proposal(engine: &mut Engine, job: &str, name: &str, arguments: Value) -> PendingTool {
    use rig_core::message::{CallId, ToolFunction, ToolName};
    let call = ToolCall::new(
        CallId::from_wire(new_id()),
        ToolFunction {
            name: ToolName::new(name).unwrap(),
            arguments,
        },
    );
    let event = engine.event(job, "model_message", json!({"response":native_response(Message::Assistant {id:None,content:vec![rig_core::completion::AssistantContent::ToolCall(call.clone())]})}));
    let key = format!("{}:{}", event.id, serde_json::to_string(&call.id).unwrap());
    engine.append_history(job, event).unwrap();
    PendingTool { call, key }
}

fn resolution_proposal(engine: &mut Engine, job: &str, resolutions: Value) -> PendingTool {
    native_proposal(
        engine,
        job,
        "input_resolve",
        json!({"resolutions":resolutions}),
    )
}

#[test]
fn superseding_queued_ancestor_answers_current_native_call_once_without_moving_budget() {
    let (_directory, mut engine) = fixture_engine();
    let ancestor = engine.post("Original engineering request", None).unwrap();
    engine.prepare_pending_input().unwrap();
    let owner = engine.state.focus.clone().unwrap();
    engine.admit_input(&owner, &ancestor).unwrap();
    let send = native_proposal(
        &mut engine,
        &owner,
        "job_send",
        json!({
            "title":"Relay","message":"Prepare a followup assignment",
        }),
    );
    engine.internal_tool(&owner, &send).unwrap();
    let relay = engine
        .state
        .jobs
        .keys()
        .find(|id| *id != &owner)
        .unwrap()
        .clone();
    let mut state = engine.state.clone();
    let job = state.jobs.get_mut(&relay).unwrap();
    job.active_input = job.inbox.pop_front();
    job.history.push(job.active_input.clone().unwrap());
    engine.commit(state, vec![]).unwrap();
    let returned = native_proposal(
        &mut engine,
        &relay,
        "job_send",
        json!({
            "job_id":owner,"message":"Return the followup to the original context",
        }),
    );
    engine.internal_tool(&relay, &returned).unwrap();
    // Persist the reachable continuation: the returned assignment is active,
    // while its original user ancestor remains queued in this same Job.
    let mut state = engine.state.clone();
    let job = state.jobs.get_mut(&owner).unwrap();
    let active = job.inbox.pop_back().unwrap();
    job.inbox
        .push_front(job.active_input.replace(active.clone()).unwrap());
    job.history.push(active.clone());
    engine.commit(state, vec![]).unwrap();
    assert!(engine.descendant_inputs(&ancestor).contains(&active));
    let before = engine.state.budgets.clone();
    let resolve = resolution_proposal(
        &mut engine,
        &owner,
        json!([{
            "input_id":ancestor,"outcome":"superseded","reason":"The original plan was withdrawn",
        }]),
    );
    engine.internal_tool(&owner, &resolve).unwrap();

    let results = engine
        .events()
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "tool_result" && event.data["tool_key"] == resolve.key)
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 1);
    let result = &results[0];
    assert_eq!(result.call_id.as_deref(), Some(resolve.key.as_str()));
    assert_eq!(result.reply_to.as_ref(), Some(&active));
    assert_eq!(result.root_input.as_ref(), Some(&ancestor));
    let message: Message = serde_json::from_value(result.data["message"].clone()).unwrap();
    let Message::User { content } = message else {
        panic!("expected native tool result")
    };
    let [rig_core::message::UserContent::ToolResult(native)] = content.as_slice() else {
        panic!("expected exactly one native result part")
    };
    assert_eq!(native.call, resolve.call.id);
    assert_eq!(native.name, resolve.call.function.name);
    assert!(engine.pending_tools(&owner).unwrap().is_empty());
    assert_eq!(
        engine.result(&active).unwrap().data["outcome"],
        "superseded"
    );
    assert_eq!(engine.state.budgets, before);
}

#[test]
fn input_resolution_validation_is_atomic_and_preserves_independent_work_and_budgets() {
    let (_directory, mut engine) = fixture_engine();
    let first = engine
        .post("Original implementation request", None)
        .unwrap();
    engine.prepare_pending_input().unwrap();
    let unrelated = engine.post("Independent queued task", None).unwrap();
    let current = engine
        .post("Finish the revised implementation", None)
        .unwrap();
    engine.prepare_pending_input().unwrap();
    let job = engine.state.focus.clone().unwrap();
    let mut foreign = Job::new("Foreign queued input");
    foreign.state = JobState::Ready;
    let mut foreign_input = engine.event(
        &foreign.id,
        "input",
        json!({"message":Message::user("foreign"),"source":"job","sender_input":current}),
    );
    foreign_input.reply_to = None;
    foreign_input.root_input = Some(current.clone());
    foreign.inbox.push_back(foreign_input.id.clone());
    let foreign_id = foreign_input.id.clone();
    let mut state = engine.state.clone();
    state.jobs.insert(foreign.id.clone(), foreign);
    engine.commit(state, vec![foreign_input]).unwrap();
    let valid = json!({"input_id":first,"outcome":"completed","reason":"Original work verified under the revised contract","evidence_event_ids":[current]});
    let invalid = [
        json!([valid.clone(), {"input_id":current,"outcome":"completed","reason":"cannot settle active"}]),
        json!([valid.clone(), valid.clone()]),
        json!([{ "input_id":first,"outcome":"completed","reason":" "}]),
        json!([{ "input_id":first,"outcome":"merged","reason":"unsupported"}]),
        json!([{ "input_id":first,"outcome":"superseded","reason":"replaced","evidence_event_ids":["missing"]}]),
        json!([{ "input_id":"missing","outcome":"completed","reason":"missing"}]),
        json!([valid.clone(), {"input_id":foreign_id,"outcome":"completed","reason":"foreign"}]),
        json!([{ "input_id":first,"outcome":"superseded","reason":"replaced","evidence_event_ids":null}]),
        json!([]),
    ];
    for resolutions in invalid {
        let tool = resolution_proposal(&mut engine, &job, resolutions);
        let before = engine.state.clone();
        let count = engine.events().unwrap().len();
        assert!(engine.internal_tool(&job, &tool).is_err());
        assert_eq!(engine.state, before);
        assert_eq!(engine.events().unwrap().len(), count);
        // Answer the rejected proposal exactly as the scheduler does.
        let event = engine
            .tool_result(&job, &tool, json!({"error":"rejected"}), None, false)
            .unwrap();
        engine.append_history(&job, event).unwrap();
    }
    let tool = resolution_proposal(&mut engine, &job, json!([valid]));
    let budgets = engine.state.budgets.clone();
    engine.internal_tool(&job, &tool).unwrap();
    assert_eq!(engine.state.budgets, budgets);
    assert_eq!(engine.state.jobs[&job].inbox, VecDeque::from([unrelated]));
    assert_eq!(
        engine.state.jobs[&job].active_input.as_deref(),
        Some(current.as_str())
    );
    let settled = engine.result(&first).unwrap();
    assert_eq!(settled.kind, "input_resolved");
    assert_eq!(settled.data["actor_input"], current);
    assert_eq!(settled.data["outcome"], "completed");
    assert_eq!(settled.root_input.as_deref(), Some(first.as_str()));
    assert_eq!(engine.read_event(&first).unwrap().kind, "input");
}

#[test]
fn resolution_wakes_exact_waiters_with_distinct_outcomes() {
    let (_directory, mut engine) = fixture_engine();
    let old = engine.post("Old bounds", None).unwrap();
    engine.prepare_pending_input().unwrap();
    let other = engine.post("Unrelated request", None).unwrap();
    let current = engine.post("Changed bounds", None).unwrap();
    engine.prepare_pending_input().unwrap();
    let owner = engine.state.focus.clone().unwrap();
    let mut waiter = Job::new("Wait on changed requirement");
    waiter.state = JobState::Waiting;
    waiter.wait_for = vec![old.clone()];
    let waiter_id = waiter.id.clone();
    let mut waiter_input = engine.event(&waiter_id, "input", json!({"message":Message::user("Wait for the original input"),"source":"job","sender_input":current}));
    waiter_input.reply_to = None;
    waiter_input.root_input = Some(current.clone());
    waiter.active_input = Some(waiter_input.id.clone());
    let mut still_waiting = Job::new("Wait on independent task");
    still_waiting.state = JobState::Waiting;
    still_waiting.wait_for = vec![other];
    let waiting_id = still_waiting.id.clone();
    let mut state = engine.state.clone();
    state.jobs.insert(waiter_id.clone(), waiter);
    state.jobs.insert(waiting_id.clone(), still_waiting);
    engine.commit(state, vec![waiter_input]).unwrap();
    let wait_call = native_proposal(
        &mut engine,
        &waiter_id,
        "job_wait",
        json!({"input_ids":[old]}),
    );
    let tool = resolution_proposal(
        &mut engine,
        &owner,
        json!([{"input_id":old,"outcome":"superseded","reason":"New bounds replace the old bounds"}]),
    );
    engine.internal_tool(&owner, &tool).unwrap();
    engine.wake_waiters().unwrap();
    assert_eq!(engine.state.jobs[&waiter_id].state, JobState::Ready);
    assert!(engine.state.jobs[&waiter_id].wait_for.is_empty());
    assert_eq!(engine.state.jobs[&waiting_id].state, JobState::Waiting);
    let results = engine.wait_results(std::slice::from_ref(&old)).unwrap();
    assert_eq!(results["results"][0]["input_id"], old);
    assert_eq!(results["results"][0]["status"], "input_resolved");
    assert_eq!(results["results"][0]["outcome"], "superseded");
    assert_eq!(results["results"][0]["actor_input"], current);
    let answered = engine
        .events()
        .unwrap()
        .into_iter()
        .find(|e| e.kind == "tool_result" && e.data["tool_key"] == wait_call.key)
        .unwrap();
    let message: Message = serde_json::from_value(answered.data["message"].clone()).unwrap();
    let Message::User { content } = message else {
        panic!("missing native result")
    };
    let rig_core::message::UserContent::ToolResult(result) = &content[0] else {
        panic!("missing native result")
    };
    assert_eq!(result.call, wait_call.call.id);
    assert_eq!(result.name.as_str(), "job_wait");
    assert!(
        serde_json::to_string(&result.content)
            .unwrap()
            .contains("superseded")
    );
    assert!(
        !engine
            .events()
            .unwrap()
            .iter()
            .any(|e| e.kind == "delivery" && e.reply_to.as_deref() == Some(old.as_str()))
    );
}

#[test]
fn resolution_ownership_follows_handoff_queue_without_rewriting_original_input() {
    let (_directory, mut engine) = fixture_engine();
    let old = engine.post("Original task", None).unwrap();
    engine.prepare_pending_input().unwrap();
    let source = engine.state.focus.clone().unwrap();
    let mut target = Job::new("Handoff target");
    target.active_input = Some(old.clone());
    target.state = JobState::Ready;
    target.history.push(old.clone());
    let target_id = target.id.clone();
    let mut state = engine.state.clone();
    state.jobs.get_mut(&source).unwrap().active_input = None;
    state.jobs.get_mut(&source).unwrap().state = JobState::Idle;
    state.focus = Some(target_id.clone());
    state.jobs.insert(target_id.clone(), target);
    engine.commit(state, vec![]).unwrap();
    engine.post("Newer task", None).unwrap();
    engine.prepare_pending_input().unwrap();
    let tool = resolution_proposal(
        &mut engine,
        &target_id,
        json!([{"input_id":old,"outcome":"superseded","reason":"Newer instruction replaced the task after handoff"}]),
    );
    engine.internal_tool(&target_id, &tool).unwrap();
    assert!(engine.state.jobs[&target_id].inbox.is_empty());
    assert_eq!(
        engine.read_event(&old).unwrap().job_id.as_deref(),
        Some(source.as_str())
    );
    assert_eq!(
        engine.result(&old).unwrap().job_id.as_deref(),
        Some(target_id.as_str())
    );
}

#[test]
fn resolution_rejects_unfinished_batches_and_live_descendants() {
    let (_directory, mut engine) = fixture_engine();
    let old = engine.post("Original work", None).unwrap();
    engine.prepare_pending_input().unwrap();
    engine.post("Current work", None).unwrap();
    engine.prepare_pending_input().unwrap();
    let owner = engine.state.focus.clone().unwrap();
    let mut child = Job::new("Delegated work");
    child.state = JobState::Paused;
    let mut child_input = engine.event(
        &child.id,
        "input",
        json!({"message":Message::user("child"),"source":"job","sender_input":old}),
    );
    child_input.reply_to = None;
    child_input.root_input = Some(old.clone());
    child.active_input = Some(child_input.id.clone());
    let mut failure = engine.event(&child.id, "failure", json!({"error":"child failed"}));
    failure.reply_to = Some(child_input.id.clone());
    failure.root_input = Some(old.clone());
    let mut grandchild = Job::new("Live descendant");
    grandchild.state = JobState::Ready;
    let mut descendant = engine.event(
        &grandchild.id,
        "input",
        json!({"message":Message::user("descendant"),"source":"job","sender_input":child_input.id}),
    );
    descendant.root_input = Some(old.clone());
    grandchild.inbox.push_back(descendant.id.clone());
    let mut state = engine.state.clone();
    state.jobs.insert(child.id.clone(), child);
    state.jobs.insert(grandchild.id.clone(), grandchild);
    engine
        .commit(state, vec![child_input, failure, descendant])
        .unwrap();
    assert!(
        engine
            .ensure_input_resolvable(&owner, &old)
            .unwrap_err()
            .to_string()
            .contains("delegated")
    );
    // Unanswered original proposals are also rejected, even if reconstructed
    // from an older snapshot rather than the usual admission path.
    let mut pending = cycles(&engine, &owner, 1, 16).remove(0);
    pending.reply_to = Some(old.clone());
    engine.append_history(&owner, pending).unwrap();
    assert!(
        engine
            .ensure_input_resolvable(&owner, &old)
            .unwrap_err()
            .to_string()
            .contains("unfinished native")
    );
}

#[test]
fn failed_retained_child_blocks_parent_settlement_across_public_resume_until_completed() {
    let (_directory, mut engine) = fixture_engine();
    let old = engine.post("Parent implementation", None).unwrap();
    engine.prepare_pending_input().unwrap();
    engine.post("Current instruction", None).unwrap();
    engine.prepare_pending_input().unwrap();
    let owner = engine.state.focus.clone().unwrap();
    let mut child = Job::new("Retained failed child");
    let child_id = child.id.clone();
    child.state = JobState::Paused;
    let mut assigned = engine.event(&child_id, "input", json!({"message":Message::user("Continue parent implementation"),"source":"job","sender_input":old}));
    assigned.reply_to = None;
    assigned.root_input = Some(old.clone());
    let assigned_id = assigned.id.clone();
    child.active_input = Some(assigned_id.clone());
    child.history.push(assigned_id.clone());
    let mut failure = engine.event(
        &child_id,
        "failure",
        json!({"error":"Build failed; correction is still needed"}),
    );
    failure.reply_to = Some(assigned_id.clone());
    failure.root_input = Some(old.clone());
    let mut state = engine.state.clone();
    state.jobs.insert(child_id.clone(), child);
    engine.commit(state, vec![assigned, failure]).unwrap();
    assert_eq!(engine.terminal(&assigned_id).unwrap().kind, "failure");
    let tool = resolution_proposal(
        &mut engine,
        &owner,
        json!([{"input_id":old,"outcome":"completed","reason":"Cannot claim completion while failed child remains resumable"}]),
    );
    let before = engine.state.clone();
    let event_count = engine.events().unwrap().len();
    let error = engine.internal_tool(&owner, &tool).unwrap_err();
    assert!(error.to_string().contains("retained delegated"));
    assert_eq!(engine.state, before);
    assert_eq!(engine.events().unwrap().len(), event_count);
    let rejected = engine
        .tool_result(
            &owner,
            &tool,
            json!({"error":error.to_string()}),
            None,
            false,
        )
        .unwrap();
    engine.append_history(&owner, rejected).unwrap();

    engine.resume().unwrap();
    assert_eq!(engine.state.jobs[&child_id].state, JobState::Ready);
    assert_eq!(
        engine.state.jobs[&child_id].active_input.as_deref(),
        Some(assigned_id.as_str())
    );
    assert!(engine.terminal(&assigned_id).is_none());
    assert!(engine.result(&old).is_none());
    assert!(engine.state.jobs[&owner].inbox.contains(&old));
    assert!(engine.ensure_input_resolvable(&owner, &old).is_err());

    let response = engine.event(
        &child_id,
        "model_message",
        json!({"response":native_response(Message::assistant("Build corrected and verified"))}),
    );
    let response_id = response.id.clone();
    engine.append_history(&child_id, response).unwrap();
    engine.deliver(&child_id, &response_id).unwrap();
    assert!(engine.state.jobs[&child_id].active_input.is_none());
    assert_eq!(engine.terminal(&assigned_id).unwrap().kind, "delivery");
    let tool = resolution_proposal(
        &mut engine,
        &owner,
        json!([{"input_id":old,"outcome":"completed","reason":"The child has now completed and provided verification","evidence_event_ids":[response_id]}]),
    );
    engine.internal_tool(&owner, &tool).unwrap();
    assert_eq!(engine.result(&old).unwrap().kind, "input_resolved");
    assert!(!engine.state.jobs[&owner].inbox.contains(&old));
}

fn cycles(engine: &Engine, job: &str, count: usize, bytes: usize) -> Vec<Event> {
    use rig_core::message::{CallId, ToolFunction, ToolName};
    let mut events = Vec::new();
    for _ in 0..count {
        let call = ToolCall::new(
            CallId::from_wire(new_id()),
            ToolFunction {
                name: ToolName::new("read_file").unwrap(),
                arguments: json!({"path":"evidence.txt"}),
            },
        );
        let model = engine.event(job,"model_message", json!({"response":native_response(Message::Assistant {id:None,content:vec![rig_core::completion::AssistantContent::ToolCall(call.clone())]})}));
        let key = format!("{}:{}", model.id, serde_json::to_string(&call.id).unwrap());
        let result = engine.event(job,"tool_result", json!({"message":Message::tool_result(call.id, call.function.name, format!("EXACT_EVIDENCE_{}", "证".repeat(bytes/3))),"tool_key":key,"uncertain":false}));
        events.extend([model, result]);
    }
    events
}

fn cache_bytes(engine: &Engine) -> usize {
    engine
        .events
        .values()
        .map(|e| serde_json::to_vec(e).unwrap().len())
        .sum()
}

#[tokio::test]
async fn thousands_of_large_events_stay_archived_across_repeated_summaries_and_restarts() {
    let (directory, mut engine) = fixture_engine();
    let input = engine
        .post("Original requirement: retain exact evidence.", None)
        .unwrap();
    engine.prepare_pending_input().unwrap();
    let job_id = engine.state.focus.clone().unwrap();
    let events = cycles(&engine, &job_id, 1100, 8192);
    let original_event = events[1].clone();
    let mut state = engine.state.clone();
    let ids: Vec<_> = events.iter().map(|e| e.id.clone()).collect();
    // Persist the summary and its already-compacted history atomically.
    // The covered native events remain available in the immutable archive.
    let summary = engine.event(&job_id,"summary",json!({"response":native_response(Message::assistant("Exact evidence remains available through audit IDs.")),"covered_ids":ids}));
    state.jobs.get_mut(&job_id).unwrap().summary = Some(summary.id.clone());
    state.jobs.get_mut(&job_id).unwrap().public_revision = 1;
    let mut all = events;
    all.push(summary);
    engine.store.commit(&state, &all).unwrap();
    let durable_bytes: usize = all
        .iter()
        .map(|e| serde_json::to_vec(e).unwrap().len())
        .sum();
    let session = engine.state.id.clone();
    let workspace = engine.state.workspace.clone();
    drop(engine);
    let mut previous_fresh = Vec::<String>::new();
    for round in 0..4 {
        let mut engine = open_fixture_engine(directory.path(), &workspace, Some(&session));
        assert!(engine.events.len() > 2200);
        assert!(
            cache_bytes(&engine) < durable_bytes / 4,
            "large original bodies leaked into live cache"
        );
        if previous_fresh.is_empty() {
            assert!(engine.state.jobs[&job_id].history.len() <= 2);
        } else {
            assert_eq!(
                engine.state.jobs[&job_id].history,
                [vec![input.clone()], previous_fresh.clone()].concat(),
                "Unobserved native results must survive summary and reopen"
            );
        }
        assert_eq!(
            engine.read_event(&original_event.id).unwrap(),
            original_event
        );
        engine.resume().unwrap();
        let events = cycles(&engine, &job_id, 8, 1024);
        let fresh: Vec<_> = events[events.len() - 2..]
            .iter()
            .map(|event| event.id.clone())
            .collect();
        let mut state = engine.state.clone();
        state
            .jobs
            .get_mut(&job_id)
            .unwrap()
            .history
            .extend(events.iter().map(|e| e.id.clone()));
        // New work responses consumed the previous round's fresh pair.
        // The final pair has no subsequent committed work response yet.
        let expected_covered: BTreeSet<_> = state.jobs[&job_id]
            .history
            .iter()
            .filter(|id| *id != &input && !fresh.contains(id))
            .cloned()
            .collect();
        engine.commit(state, events).unwrap();
        let (_, covered) =
            context::compaction_prefix(&engine.state.jobs[&job_id], &engine.events, 1, 96_000)
                .unwrap()
                .unwrap();
        assert_eq!(
            covered.len(),
            if round == 0 { 14 } else { 16 },
            "previously summarized IDs must not accumulate"
        );
        assert_eq!(
            covered.iter().cloned().collect::<BTreeSet<_>>(),
            expected_covered,
            "Summary must cover exactly consumed pairs, never fresh or already summarized events"
        );
        let origin = Origin {
            job: job_id.clone(),
            input: input.clone(),
            root: input.clone(),
            call: new_id(),
            revision: engine.state.revision,
        };
        let task = tokio::spawn(std::future::pending::<()>());
        engine.running.insert(
            origin.call.clone(),
            Running {
                origin: origin.clone(),
                abort: task.abort_handle(),
                tool: None,
                cancellation: None,
            },
        );
        engine
            .complete(Completed::Model {
                origin,
                response: Ok(native_response(Message::assistant(format!(
                    "Round {round}: preserve requirement and audit {}",
                    original_event.id
                )))),
                covered: Some(covered),
                stream_items: vec![],
            })
            .unwrap();
        task.abort();
        assert_eq!(
            engine.state.jobs[&job_id].history,
            [vec![input.clone()], fresh.clone()].concat()
        );
        assert_eq!(engine.state.jobs[&job_id].public_revision, 1);
        assert!(engine.pending_tools(&job_id).unwrap().is_empty());
        let history = context::build_history(&engine.state.jobs[&job_id], &engine.events).unwrap();
        assert_eq!(history.len(), 4);
        let fresh_response: CompletionResponse =
            serde_json::from_value(engine.read_event(&fresh[0]).unwrap().data["response"].clone())
                .unwrap();
        assert_eq!(Some(history[2].clone()), fresh_response.message());
        let fresh_result: Message =
            serde_json::from_value(engine.read_event(&fresh[1]).unwrap().data["message"].clone())
                .unwrap();
        assert_eq!(history[3], fresh_result);
        previous_fresh = fresh;
        let page = engine.inspect(&json!({"job_id":job_id,"limit":2})).unwrap();
        assert!(page["next_before_id"].is_string());
        let next_page = engine
            .inspect(&json!({"job_id":job_id,"limit":2,"before_id":page["next_before_id"]}))
            .unwrap();
        assert_ne!(page["records"][0]["id"], next_page["records"][0]["id"]);
        // Read exactly the original Unicode JSON through bounded chunks.
        let mut recovered = String::new();
        let mut offset = 0;
        loop {
            let chunk = engine
                .inspect(
                    &json!({"event_id":original_event.id,"offset":offset,"limit":500,"raw":true}),
                )
                .unwrap();
            assert!(serde_json::to_string(&chunk).unwrap().chars().count() < 8_000);
            recovered.push_str(chunk["text"].as_str().unwrap());
            let Some(next) = chunk["next_offset"].as_u64() else {
                break;
            };
            offset = next;
        }
        assert_eq!(
            serde_json::from_str::<Event>(&recovered).unwrap(),
            original_event
        );
        assert!(engine.inspect(&json!({"event_id":"absent"})).is_err());
        assert!(
            engine
                .inspect(&json!({"job_id":job_id,"before_id":"absent"}))
                .is_err()
        );
        eprintln!(
            "archive round {round}: events={}, durable_seed_bytes={durable_bytes}, resident_event_bytes={}",
            engine.events.len(),
            cache_bytes(&engine)
        );
    }
}

#[tokio::test]
async fn idle_and_closed_jobs_do_not_keep_large_native_bodies_resident() {
    let (directory, mut engine) = fixture_engine();
    let mut state = engine.state.clone();
    let mut events = Vec::new();
    for index in 0..80 {
        let mut job = Job::new(format!("finished-{index}"));
        if index % 2 == 0 {
            job.state = JobState::Closed;
        }
        let mut event = Event::new(
            &state.id,
            "model_message",
            json!({"response":native_response(Message::assistant("large".repeat(20_000)))}),
        );
        event.job_id = Some(job.id.clone());
        job.history.push(event.id.clone());
        state.jobs.insert(job.id.clone(), job);
        events.push(event);
    }
    engine.commit(state, events).unwrap();
    let session = engine.state.id.clone();
    let workspace = engine.state.workspace.clone();
    drop(engine);
    let mut engine = open_fixture_engine(directory.path(), &workspace, Some(&session));
    assert!(engine.loaded.is_empty());
    assert!(cache_bytes(&engine) < 100_000);
    let input = engine.post("continue here", None).unwrap();
    let focus = engine.state.focus.clone().unwrap();
    assert!(engine.loaded.contains(&input));
    assert_eq!(engine.state.jobs[&focus].state, JobState::Ready);
}

#[tokio::test]
async fn completion_transaction_failure_poison_blocks_actions_and_restart_recovers_one_result() {
    use rig_core::message::{CallId, ToolFunction, ToolName};
    let (directory, mut engine) = fixture_engine();
    let input = engine.post("write once", None).unwrap();
    engine.prepare_pending_input().unwrap();
    let job = engine.state.focus.clone().unwrap();
    let call = ToolCall::new(
        CallId::from_wire("write"),
        ToolFunction {
            name: ToolName::new("write_file").unwrap(),
            arguments: json!({"path":"result","content":"one"}),
        },
    );
    let event = engine.event(&job,"model_message",json!({"response":native_response(Message::Assistant {id:None,content:vec![rig_core::completion::AssistantContent::ToolCall(call.clone())]})}));
    let tool = PendingTool {
        key: format!("{}:{}", event.id, serde_json::to_string(&call.id).unwrap()),
        call,
    };
    engine.append_history(&job, event).unwrap();
    let origin = Origin {
        job: job.clone(),
        input: input.clone(),
        root: input,
        call: new_id(),
        revision: engine.state.revision,
    };
    let mut started = engine.event(
        &job,
        "tool_started",
        json!({"effect":"write","tool_name":"write_file","tool_key":tool.key}),
    );
    started.call_id = Some(origin.call.clone());
    let mut state = engine.state.clone();
    state.jobs.get_mut(&job).unwrap().state = JobState::Running;
    state.jobs.get_mut(&job).unwrap().current_call = Some(origin.call.clone());
    engine.commit(state, vec![started]).unwrap();
    let lease = WriteLease::acquire(&engine.state.workspace)
        .unwrap()
        .unwrap();
    engine.write_lease = Some(lease);
    let task = tokio::spawn(std::future::pending::<()>());
    engine.running.insert(
        origin.call.clone(),
        Running {
            origin: origin.clone(),
            abort: task.abort_handle(),
            tool: Some(tool.clone()),
            cancellation: None,
        },
    );
    let db = rusqlite::Connection::open(engine.data_dir.join("sessions.sqlite")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_result BEFORE INSERT ON events WHEN json_extract(NEW.payload,'$.kind')='tool_result' BEGIN SELECT RAISE(ABORT,'injected completion commit failure'); END;").unwrap();
    assert!(
        engine
            .complete(Completed::Tool {
                origin: origin.clone(),
                tool,
                outcome: tools::ToolOutcome {
                    content: json!({"written":true}),
                    uncertain: false
                }
            })
            .is_err()
    );
    assert!(engine.faulted);
    assert!(
        engine
            .step()
            .await
            .unwrap_err()
            .to_string()
            .contains("reopen")
    );
    assert!(engine.post("new instruction", None).is_err());
    assert!(engine.stop().is_err());
    assert!(engine.resume().is_err());
    assert!(engine.read_event(&engine.order[0]).is_ok());
    db.execute_batch("DROP TRIGGER reject_result").unwrap();
    let session = engine.state.id.clone();
    let workspace = engine.state.workspace.clone();
    drop(engine);
    let mut recovered = open_fixture_engine(directory.path(), &workspace, Some(&session));
    assert!(recovered.state.paused);
    assert_eq!(recovered.state.jobs[&job].state, JobState::Paused);
    let results = recovered
        .events()
        .unwrap()
        .into_iter()
        .filter(|event| {
            event.kind == "tool_result" && event.call_id.as_deref() == Some(&origin.call)
        })
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].data["uncertain"], true);
    assert!(
        recovered
            .readable_event(&results[0])
            .unwrap()
            .contains("\"effect\":\"unknown\"")
    );
    assert!(recovered.pending_tools(&job).unwrap().is_empty());
    recovered.resume().unwrap();
    assert!(WriteLease::acquire(&workspace).unwrap().is_some());
    task.abort();
}

#[tokio::test]
async fn restart_between_input_preparation_and_model_start_keeps_older_shared_corrections() {
    let (directory, mut engine) = fixture_engine();
    engine.post("start", None).unwrap();
    engine.prepare_pending_input().unwrap();
    let first = engine.state.focus.clone().unwrap();
    engine.incorporate_public_inputs(&first).unwrap();
    let mut state = engine.state.clone();
    let second = Job::new("another continuing context");
    let second_id = second.id.clone();
    state.jobs.insert(second_id.clone(), second);
    state.focus = Some(second_id.clone());
    engine.commit(state, vec![]).unwrap();
    let correction = engine
        .post("GLOBAL CORRECTION: preserve the original data", None)
        .unwrap();
    engine.prepare_pending_input().unwrap();
    let mut state = engine.state.clone();
    state.focus = Some(first.clone());
    engine.commit(state, vec![]).unwrap();
    let newest = engine
        .post("continue with this new own input", None)
        .unwrap();
    engine.prepare_pending_input().unwrap();
    assert!(engine.state.jobs[&first].history.contains(&newest));
    assert!(!engine.state.jobs[&first].history.contains(&correction));
    let prior_cursor = engine.state.jobs[&first].public_revision;
    let session = engine.state.id.clone();
    let workspace = engine.state.workspace.clone();
    drop(engine);
    let mut engine = open_fixture_engine(directory.path(), &workspace, Some(&session));
    assert_eq!(engine.state.jobs[&first].public_revision, prior_cursor);
    engine.resume().unwrap();
    engine.incorporate_public_inputs(&first).unwrap();
    assert!(engine.state.jobs[&first].history.contains(&correction));
    let history = context::build_history(&engine.state.jobs[&first], &engine.events).unwrap();
    assert!(
        serde_json::to_string(&history)
            .unwrap()
            .contains("GLOBAL CORRECTION")
    );
}

#[tokio::test]
async fn waiting_on_long_child_results_returns_bounded_previews_and_exact_audit_references() {
    let (_directory, mut engine) = fixture_engine();
    engine.options.context_chars = 16_000;
    let parent_input = engine.post("verify all delegated output", None).unwrap();
    engine.prepare_pending_input().unwrap();
    let mut state = engine.state.clone();
    let mut events = Vec::new();
    let mut inputs = Vec::new();
    for index in 0..10 {
        let job = Job::new(format!("child {index}"));
        let mut input = Event::new(
            &state.id,
            "input",
            json!({"message":Message::user("produce evidence"),"source":"job"}),
        );
        input.job_id = Some(job.id.clone());
        input.root_input = Some(parent_input.clone());
        inputs.push(input.id.clone());
        let mut response = Event::new(
            &state.id,
            "model_message",
            json!({"response":native_response(Message::assistant("verified exact output".repeat(5000)))}),
        );
        response.job_id = Some(job.id.clone());
        response.reply_to = Some(input.id.clone());
        let mut delivery = Event::new(&state.id, "delivery", json!({"response_event":response.id}));
        delivery.job_id = Some(job.id.clone());
        delivery.reply_to = Some(input.id.clone());
        state.jobs.insert(job.id.clone(), job);
        events.extend([input, response, delivery]);
    }
    engine.commit(state, events).unwrap();
    let results = engine.wait_results(&inputs).unwrap();
    assert!(serde_json::to_string(&results).unwrap().chars().count() < 3_000);
    assert!(
        results["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["truncated"] == true && r["response_event"].is_string())
    );
    let original = engine
        .read_event(results["results"][0]["response_event"].as_str().unwrap())
        .unwrap();
    assert!(serde_json::to_string(&original).unwrap().len() > 100_000);
}

#[tokio::test]
async fn user_input_lookup_skips_audit_noise_and_crosses_job_boundaries() {
    let (_directory, mut engine) = fixture_engine();
    engine.options.context_chars = 32_000;
    let first = engine
        .post(
            "Original requirement: preserve the exact public interface and named output format",
            None,
        )
        .unwrap();
    let original_job = engine.state.focus.clone().unwrap();
    let mut state = engine.state.clone();
    let second = Job::new("later context");
    let second_id = second.id.clone();
    state.jobs.insert(second_id.clone(), second);
    state.focus = Some(second_id.clone());
    engine.commit(state, vec![]).unwrap();
    let mut expected = vec![first.clone()];
    for round in 0..5 {
        let input = engine
            .post(&format!("Later instruction {round}"), None)
            .unwrap();
        expected.push(input);
        let audit = (0..100)
            .map(|_| {
                engine.event(
                    &second_id,
                    "model_started",
                    json!({"diagnostic":"audit noise"}),
                )
            })
            .collect();
        engine.commit(engine.state.clone(), audit).unwrap();
    }
    assert!(engine.events.len() > 500);
    let mut ids = Vec::new();
    let mut before = None;
    loop {
        let page = engine
            .inspect(&json!({"users_only":true,"job_id":second_id,"limit":2,"before_id":before}))
            .unwrap();
        for record in page["records"].as_array().unwrap() {
            ids.push(record["id"].as_str().unwrap().to_owned());
        }
        if !page["truncated"].as_bool().unwrap() {
            break;
        }
        before = Some(page["next_before_id"].as_str().unwrap().to_owned());
    }
    expected.reverse();
    assert_eq!(ids, expected);
    let original = engine
        .inspect(&json!({"users_only":true,"limit":32}))
        .unwrap();
    assert!(
        original["records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == first && r["job_id"] == original_job)
    );
    assert!(
        engine
            .inspect(&json!({"users_only":true,"before_id":engine.order.last().unwrap()}))
            .is_err()
    );
    let full = engine.inspect(&json!({"event_id":first})).unwrap();
    assert!(
        full["text"]
            .as_str()
            .unwrap()
            .starts_with("Original requirement:")
    );
    assert_eq!(full["raw"], false);
}

#[tokio::test]
async fn readable_summary_pages_skip_native_metadata_and_preserve_every_character() {
    let (_directory, mut engine) = fixture_engine();
    engine.post("inspect earlier requirements", None).unwrap();
    let job = engine.state.focus.clone().unwrap();
    let expected="Exact unfinished interface: preserve quoted \"names\", Unicode 中文 and emoji 🦀.\nRequired output format and option semantics stay explicit. ".repeat(12);
    let mut response =
        serde_json::to_value(native_response(Message::assistant(expected.clone()))).unwrap();
    response["choice"].as_array_mut().unwrap().insert(0,json!({"type":"reasoning","issuer":"chatgpt","id":"opaque-reasoning","content":[{"type":"encrypted","content":"sealed-opaque".repeat(2000)}]}));
    response["raw"] = json!({"instructions":"large raw request".repeat(2000)});
    let summary = engine.event(
        &job,
        "summary",
        json!({"response":response,"covered_ids":(0..100).map(|_|new_id()).collect::<Vec<_>>() }),
    );
    let id = summary.id.clone();
    engine
        .commit(engine.state.clone(), vec![summary.clone()])
        .unwrap();
    let mut text = String::new();
    let mut offset = 0;
    loop {
        let page = engine
            .inspect(&json!({"event_id":id,"offset":offset,"limit":37}))
            .unwrap();
        assert_eq!(page["offset"], offset);
        assert_eq!(page["total_chars"], expected.chars().count());
        let part = page["text"].as_str().unwrap();
        text.push_str(part);
        let Some(next) = page["next_offset"].as_u64() else {
            break;
        };
        assert_eq!(next as usize, offset + part.chars().count());
        offset = next as usize;
    }
    assert_eq!(text, expected);
    assert!(!text.contains("sealed-opaque"));
    let original = engine
        .inspect(&json!({"event_id":id,"raw":true,"limit":1}))
        .unwrap();
    assert_eq!(original["text"], "{");
    assert_eq!(original["raw"], true);
    assert_eq!(engine.read_event(&id).unwrap(), summary);
}

#[tokio::test]
async fn shell_progress_precedes_completion_and_keeps_final_output_after_new_input() {
    let command = if cfg!(windows) {
        "powershell.exe -NoProfile -NonInteractive -Command \"[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); [Console]::Write('early 中文'); [Console]::Error.Write('warning;native='+[Environment]::CurrentDirectory); while (![IO.File]::Exists('release')) { [Threading.Thread]::Sleep(10) }; [Console]::Write(' final')\""
    } else {
        "printf 'early 中文'; printf 'warning' >&2; while [ ! -f release ]; do sleep 0.01; done; printf ' final'"
    };
    let (_directory, mut engine) = fixture_engine();
    engine.post("run a build", None).unwrap();
    engine.prepare_pending_input().unwrap();
    let job = engine.state.focus.clone().unwrap();
    assert!(engine.drain_tool_progress().is_empty());
    let tool = native_proposal(&mut engine, &job, "shell", json!({"command":command}));
    engine.start_tool(&job, tool).unwrap();
    let call = engine.state.jobs[&job].current_call.clone().unwrap();
    assert!(
        engine
            .events()
            .unwrap()
            .iter()
            .any(|event| event.kind == "tool_started" && event.call_id.as_ref() == Some(&call))
    );
    let observed = tokio::time::timeout(Duration::from_secs(if cfg!(windows) { 20 } else { 3 }), async {
        loop {
            let records = engine.drain_tool_progress();
            if let Some(record) = records
                .into_iter()
                .find(|record| record.stdout.contains("中文") && record.stderr.contains("warning"))
            {
                break record;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|error| {
        panic!("shell did not produce live stdout/stderr before completion: {error}; recent progress: {:?}", engine.drain_tool_progress())
    });
    assert_eq!(observed.call_id, call);
    assert_eq!(observed.job_id, job);
    assert_eq!(observed.tool_name, "shell");
    assert!(
        !engine.tasks.is_empty(),
        "observation waited for final completion"
    );
    assert!(
        !engine
            .events()
            .unwrap()
            .iter()
            .any(|event| event.kind == "tool_result")
    );
    engine.post("add a new requirement", None).unwrap();
    assert!(
        engine.drain_tool_progress().is_empty(),
        "old revision leaked after new input"
    );
    std::fs::write(engine.state.workspace.join("release"), "").unwrap();
    let done = tokio::time::timeout(Duration::from_secs(3), engine.tasks.join_next())
        .await
        .unwrap_or_else(|error| {
            panic!(
                "shell did not finish after release in {}: {error}; observed shell location: {}",
                engine.state.workspace.display(),
                observed.stderr
            )
        })
        .unwrap()
        .unwrap();
    engine.complete(done).unwrap();
    assert!(
        engine.drain_tool_progress().is_empty(),
        "finished call preview returned"
    );
    let result = engine
        .events()
        .unwrap()
        .into_iter()
        .find(|event| event.kind == "tool_result")
        .unwrap();
    let text = serde_json::to_string(&result.data).unwrap();
    assert!(text.contains("early 中文 final"));
    assert!(text.contains("warning"));
    assert_eq!(result.data["uncertain"], false);
}

#[test]
fn tool_observation_is_opt_in_bounded_and_rejects_unowned_or_stopped_calls() {
    let (_directory, mut engine) = fixture_engine();
    engine.post("observe", None).unwrap();
    engine.prepare_pending_input().unwrap();
    let job = engine.state.focus.clone().unwrap();
    let mut origin = progress_origin(engine.state.revision);
    origin.job = job.clone();
    engine
        .tool_progress
        .push(&origin, "shell", false, b"before opt in");
    assert!(engine.tool_progress.calls.lock().unwrap().is_empty());
    engine.drain_tool_progress();
    engine.state.jobs.get_mut(&job).unwrap().current_call = Some(origin.call.clone());
    for _ in 0..10 {
        engine
            .tool_progress
            .push(&origin, "shell", false, "中文".repeat(2000).as_bytes());
    }
    engine.tool_progress.push(&origin, "shell", true, b"stderr");
    let records = engine.drain_tool_progress();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].stderr, "stderr");
    assert!(records[0].stdout.len() <= TOOL_PROGRESS_BYTES + 3);
    assert!(
        engine.drain_tool_progress().is_empty(),
        "unchanged snapshot returned"
    );
    engine
        .tool_progress
        .push(&origin, "shell", false, b"unowned");
    engine.state.jobs.get_mut(&job).unwrap().current_call = None;
    assert!(engine.drain_tool_progress().is_empty());
    engine.stop().unwrap();
    engine
        .tool_progress
        .push(&origin, "shell", false, b"racing producer");
    assert!(engine.drain_tool_progress().is_empty());
    for index in 0..TOOL_PROGRESS_CALLS + 5 {
        origin.call = format!("call-{index}");
        engine
            .tool_progress
            .push(&origin, "shell", false, b"bounded");
    }
    assert_eq!(
        engine.tool_progress.calls.lock().unwrap().len(),
        TOOL_PROGRESS_CALLS
    );
}

#[test]
fn independent_messages_and_explicit_question_replies_keep_their_routing() {
    let (_directory, mut engine) = fixture_engine();
    let root = engine.post("parent work", None).unwrap();
    engine.prepare_pending_input().unwrap();
    let parent = engine.state.focus.clone().unwrap();
    let tool = native_proposal(
        &mut engine,
        &parent,
        "ask_user",
        json!({"question":"First question?"}),
    );
    engine.internal_tool(&parent, &tool).unwrap();
    let first = engine.unanswered_questions()[0].id.clone();
    let original_budget = engine.state.budgets[&root].clone();

    let mut child = Job::new("child");
    let child_id = child.id.clone();
    let mut assigned = engine.event(
        &child_id,
        "input",
        json!({"source":"job","message":Message::user("child work"),"sender_input":root}),
    );
    assigned.reply_to = None;
    assigned.root_input = Some(root.clone());
    child.active_input = Some(assigned.id.clone());
    child.history.push(assigned.id.clone());
    let mut state = engine.state.clone();
    state.jobs.insert(child_id.clone(), child);
    engine.commit(state, vec![assigned]).unwrap();
    let tool = native_proposal(
        &mut engine,
        &child_id,
        "ask_user",
        json!({"question":"Second question?"}),
    );
    engine.internal_tool(&child_id, &tool).unwrap();
    let second = engine.unanswered_questions().last().unwrap().id.clone();
    let independent = engine
        .post_message("new instruction, not an answer")
        .unwrap();
    assert!(engine.read_event(&independent).unwrap().reply_to.is_none());
    assert_eq!(engine.unanswered_questions().len(), 2);
    assert_eq!(engine.state.budgets[&root], original_budget);
    assert_eq!(
        engine.read_event(&independent).unwrap().root_input.as_ref(),
        Some(&independent)
    );

    let reply = engine.post("answer first", Some(&first)).unwrap();
    assert_eq!(
        engine.read_event(&reply).unwrap().reply_to.as_ref(),
        Some(&first)
    );
    assert_eq!(
        engine.read_event(&reply).unwrap().job_id.as_ref(),
        Some(&parent)
    );
    assert_eq!(
        engine
            .unanswered_questions()
            .iter()
            .map(|q| q.id.clone())
            .collect::<Vec<_>>(),
        vec![second.clone()]
    );
    let before = engine.state.clone();
    let error = engine.post("stale answer", Some(&first)).unwrap_err();
    assert!(error.to_string().contains("no longer awaiting"));
    assert_eq!(engine.state, before);
    assert!(engine.post("wrong target", Some(&independent)).is_err());
    assert_eq!(engine.state, before);
    let automatic = engine.post("automatic answer", None).unwrap();
    assert_eq!(
        engine.read_event(&automatic).unwrap().reply_to.as_ref(),
        Some(&second)
    );
    assert_eq!(
        engine.read_event(&automatic).unwrap().job_id.as_ref(),
        Some(&child_id)
    );
    assert!(engine.unanswered_questions().is_empty());
    assert_eq!(engine.state.budgets[&root], original_budget);
}
