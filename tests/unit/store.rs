use super::*;
use crate::state::{Budget, Job};

fn assert_known_writes(store: &Store, state: &SessionState) {
    assert!(
        store
            .recover_session(&state.id)
            .unwrap()
            .unknown_writes
            .is_empty()
    );
}

fn call_event(state: &SessionState, kind: &str, call: &str, data: serde_json::Value) -> Event {
    let mut event = Event::new(&state.id, kind, data);
    event.job_id = state.focus.clone();
    event.call_id = Some(call.into());
    event.revision = state.revision;
    event
}

fn fixture() -> (tempfile::TempDir, Store, SessionState, Event) {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("bone.sqlite")).unwrap();
    let mut state = SessionState::new(directory.path());
    let job = Job::new("main");
    state.focus = Some(job.id.clone());
    state.jobs.insert(job.id.clone(), job.clone());
    store.create_session(&state).unwrap();
    let mut input = Event::new(&state.id, "input", json!({"text": "hello"}));
    input.job_id = Some(job.id.clone());
    input.root_input = Some(input.id.clone());
    input.revision = 1;
    state.revision = 1;
    state
        .jobs
        .get_mut(&job.id)
        .unwrap()
        .inbox
        .push_back(input.id.clone());
    state.pending_inputs.push_back(input.id.clone());
    state.budgets.insert(input.id.clone(), Budget::new(5, 3));
    (directory, store, state, input)
}

#[test]
fn history_page_uses_append_order_and_bounded_cursor_reads() {
    let (_directory, store, mut state, first) = fixture();
    store.commit(&state, std::slice::from_ref(&first)).unwrap();
    let mut second = Event::new(&state.id, "second", json!({"body":"original"}));
    second.revision = 2;
    state.revision = 2;
    store.commit(&state, std::slice::from_ref(&second)).unwrap();
    let (page, more) = store.history_page(&state.id, None, 1).unwrap();
    assert_eq!(page, vec![first.clone()]);
    assert!(more);
    let (last, more) = store.history_page(&state.id, Some(&first.id), 1).unwrap();
    assert_eq!(last, vec![second]);
    assert!(!more);
    assert!(store.history_page(&state.id, Some("unknown"), 1).is_err());
    assert!(store.history_page(&state.id, None, 0).is_err());
}

#[test]
fn history_before_preserves_append_order_and_session_scoped_backward_cursor() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("sessions.sqlite3")).unwrap();
    let state = SessionState::new(directory.path());
    let foreign = SessionState::new(directory.path());
    store.create_session(&state).unwrap();
    store.create_session(&foreign).unwrap();
    let mut expected = Vec::new();
    let foreign_event = Event::new(&foreign.id, "notice", json!({"text":"foreign"}));
    for index in 0..5 {
        let mut event = Event::new(&state.id, "notice", json!({"index":index}));
        // Timestamp ties and reverse lexical IDs must not alter append order.
        event.timestamp = "1".into();
        event.id = format!("event-{}", 5 - index);
        store.commit(&state, std::slice::from_ref(&event)).unwrap();
        expected.push(event);
        if index == 1 {
            store
                .commit(&foreign, std::slice::from_ref(&foreign_event))
                .unwrap();
        }
    }
    let tail = crate::history_before(directory.path(), &state.id, None, 2).unwrap();
    assert_eq!(tail.events, expected[3..]);
    assert!(tail.has_more);
    assert_eq!(tail.next_cursor.as_deref(), Some(expected[3].id.as_str()));
    let older =
        crate::history_before(directory.path(), &state.id, tail.next_cursor.as_deref(), 2).unwrap();
    assert_eq!(older.events, expected[1..3]);
    assert!(older.has_more);
    let first = crate::history_before(directory.path(), &state.id, older.next_cursor.as_deref(), 2)
        .unwrap();
    assert_eq!(first.events, expected[..1]);
    assert!(!first.has_more);
    let combined = first
        .events
        .into_iter()
        .chain(older.events)
        .chain(tail.events)
        .collect::<Vec<_>>();
    assert_eq!(combined, expected);
    let empty = crate::history_before(directory.path(), &state.id, first.next_cursor.as_deref(), 2)
        .unwrap();
    assert!(empty.events.is_empty());
    assert!(empty.next_cursor.is_none());
    assert!(!empty.has_more);
    for (cursor, limit) in [
        (Some(foreign_event.id.as_str()), 2),
        (Some("unknown"), 2),
        (None, 0),
        (None, 1001),
    ] {
        assert!(
            crate::history_before(directory.path(), &state.id, cursor, limit).is_err(),
            "cursor={cursor:?}, limit={limit}"
        );
    }
    assert_eq!(
        crate::history_before(directory.path(), &foreign.id, None, 2)
            .unwrap()
            .events,
        vec![foreign_event]
    );
}

#[test]
fn restart_keeps_snapshot_and_one_copy_of_native_message() {
    let (directory, store, mut state, input) = fixture();
    store.commit(&state, &[input]).unwrap();
    let mut message = Event::new(
        &state.id,
        "model_message",
        json!({"role":"assistant", "content":[{"type":"text", "text":"native"}]}),
    );
    message.job_id = state.focus.clone();
    message.revision = 2;
    state.revision = 2;
    state
        .jobs
        .get_mut(state.focus.as_ref().unwrap())
        .unwrap()
        .history
        .push(message.id.clone());
    store
        .commit(&state, std::slice::from_ref(&message))
        .unwrap();
    drop(store);
    let restarted = Store::open(directory.path().join("bone.sqlite")).unwrap();
    assert_eq!(restarted.load_session(&state.id).unwrap(), state);
    assert_eq!(
        restarted.events(&state.id).unwrap().last().unwrap(),
        &message
    );
    assert!(!serde_json::to_string(&state).unwrap().contains("native"));
}

#[test]
fn duplicate_ids_are_idempotent_and_conflicts_rollback() {
    let (_directory, store, mut state, input) = fixture();
    store.commit(&state, std::slice::from_ref(&input)).unwrap();
    store.commit(&state, std::slice::from_ref(&input)).unwrap();
    assert_eq!(store.events(&state.id).unwrap().len(), 1);
    let previous = state.clone();
    state.revision += 1;
    let mut fresh = Event::new(&state.id, "summary", json!({}));
    fresh.revision = state.revision;
    let mut conflict = input;
    conflict.data = json!({"text": "changed"});
    assert!(store.commit(&state, &[fresh, conflict]).is_err());
    assert_eq!(store.load_session(&state.id).unwrap(), previous);
    assert_eq!(store.events(&state.id).unwrap().len(), 1);
}

#[test]
fn job_transitions_preserve_instruction_revision_and_reject_regression() {
    let (_directory, store, mut state, input) = fixture();
    store.commit(&state, &[input]).unwrap();
    state
        .jobs
        .get_mut(state.focus.as_ref().unwrap())
        .unwrap()
        .state = JobState::Ready;
    let mut event = Event::new(&state.id, "context_note", json!({"reason":"ready"}));
    event.revision = state.revision;
    store.commit(&state, &[event]).unwrap();
    assert_eq!(store.load_session(&state.id).unwrap(), state);
    assert_eq!(store.events(&state.id).unwrap().len(), 2);
    state.revision = 0;
    assert!(store.commit(&state, &[]).is_err());
    assert_eq!(store.load_session(&state.id).unwrap().revision, 1);
}

#[test]
fn sqlite_fault_after_event_insert_rolls_back_snapshot_and_events() {
    let (_directory, store, state, input) = fixture();
    let before = store.load_session(&state.id).unwrap();
    store.connection.execute_batch("CREATE TRIGGER inject_fault BEFORE UPDATE ON sessions BEGIN SELECT RAISE(ABORT, 'injected disk write fault'); END;").unwrap();
    assert!(store.commit(&state, std::slice::from_ref(&input)).is_err());
    assert_eq!(store.load_session(&state.id).unwrap(), before);
    assert!(store.events(&state.id).unwrap().is_empty());
    store
        .connection
        .execute_batch("DROP TRIGGER inject_fault;")
        .unwrap();
    store.commit(&state, &[input]).unwrap();
}

#[test]
fn dangling_references_roll_back() {
    let (_directory, store, mut state, input) = fixture();
    state.pending_inputs.push_back("missing".into());
    assert!(store.commit(&state, &[input]).is_err());
    assert_eq!(store.load_session(&state.id).unwrap().revision, 0);
    assert!(store.events(&state.id).unwrap().is_empty());
}

#[test]
fn session_ownership_is_exclusive_and_released() {
    let (directory, store, state, _) = fixture();
    let another = Store::open(directory.path().join("bone.sqlite")).unwrap();
    let lease = store.acquire_session(&state.id).unwrap();
    assert!(another.acquire_session(&state.id).is_err());
    drop(lease);
    assert!(another.acquire_session(&state.id).is_ok());
}

#[test]
fn recovery_pauses_work_and_records_uncertain_writes_without_replay() {
    let (directory, store, mut state, input) = fixture();
    let mut started = call_event(
        &state,
        "tool_started",
        "tool-call-1",
        json!({"effect":"write", "tool_name":"write_file"}),
    );
    started.root_input = Some(input.id.clone());
    let job = state.jobs.get_mut(state.focus.as_ref().unwrap()).unwrap();
    job.active_input = Some(job.inbox.pop_front().unwrap());
    job.current_call = started.call_id.clone();
    job.state = JobState::Running;
    let original_input = job.active_input.clone();
    store.commit(&state, &[input, started]).unwrap();
    drop(store);
    let store = Store::open(directory.path().join("bone.sqlite")).unwrap();
    let _lease = store.acquire_session(&state.id).unwrap();
    let recovered = store.recover_session(&state.id).unwrap();
    assert!(recovered.paused);
    let job = &recovered.jobs[state.focus.as_ref().unwrap()];
    assert_eq!(job.state, JobState::Paused);
    assert_eq!(job.active_input, original_input);
    assert_eq!(job.current_call.as_deref(), Some("tool-call-1"));
    assert_eq!(
        recovered.unknown_writes["tool-call-1"].tool_name,
        "write_file"
    );
    assert!(!directory.path().join("output.txt").exists());
    assert_eq!(store.recover_session(&state.id).unwrap(), recovered);
    assert_eq!(store.events(&state.id).unwrap().len(), 3);
}

#[test]
fn restart_pauses_queued_and_waiting_work_and_preserves_idle_and_closed_jobs() {
    let (_directory, store, mut state, input) = fixture();
    let original_job = state.focus.clone().unwrap();
    state.jobs.get_mut(&original_job).unwrap().state = JobState::Ready;
    let mut waiting = Job::new("waiting");
    waiting.state = JobState::Waiting;
    waiting.wait_for.push(input.id.clone());
    let waiting_id = waiting.id.clone();
    let mut idle_with_work = Job::new("queued");
    idle_with_work.inbox.push_back(input.id.clone());
    let queued_id = idle_with_work.id.clone();
    let idle = Job::new("idle");
    let idle_id = idle.id.clone();
    let mut closed = Job::new("closed");
    closed.state = JobState::Closed;
    let closed_id = closed.id.clone();
    for job in [waiting, idle_with_work, idle, closed] {
        state.jobs.insert(job.id.clone(), job);
    }
    store.commit(&state, std::slice::from_ref(&input)).unwrap();
    let recovered = store.recover_session(&state.id).unwrap();
    assert!(recovered.paused);
    for id in [&original_job, &waiting_id, &queued_id] {
        assert_eq!(recovered.jobs[id].state, JobState::Paused);
    }
    assert_eq!(
        recovered.jobs[&original_job].inbox,
        state.jobs[&original_job].inbox
    );
    assert_eq!(recovered.jobs[&waiting_id].wait_for, vec![input.id]);
    assert_eq!(recovered.jobs[&idle_id].state, JobState::Idle);
    assert_eq!(recovered.jobs[&closed_id].state, JobState::Closed);
    assert_eq!(store.recover_session(&state.id).unwrap(), recovered);
    assert_eq!(store.events(&state.id).unwrap().len(), 2);
}

#[test]
fn uncertain_native_result_stays_unknown_until_explicit_reconciliation() {
    let (_directory, store, mut state, input) = fixture();
    let mut started = call_event(
        &state,
        "tool_started",
        "uncertain-call",
        json!({"effect":"write","tool_name":"write_file"}),
    );
    started.root_input = Some(input.id.clone());
    let result = call_event(
        &state,
        "tool_result",
        "uncertain-call",
        json!({"uncertain":true}),
    );
    store.commit(&state, &[input, started, result]).unwrap();
    state = store.recover_session(&state.id).unwrap();
    assert!(state.unknown_writes.contains_key("uncertain-call"));
    state.unknown_writes.remove("uncertain-call");
    let reconciled = call_event(
        &state,
        "tool_reconciled",
        "uncertain-call",
        json!({"observation":"inspected"}),
    );
    store.commit(&state, &[reconciled]).unwrap();
    assert_known_writes(&store, &state);
}

#[test]
fn finished_write_is_not_uncertain_after_restart() {
    let (_directory, store, mut state, input) = fixture();
    let started = call_event(
        &state,
        "tool_started",
        "completed",
        json!({"effect":"write"}),
    );
    let result = call_event(&state, "tool_result", "completed", json!({"ok":true}));
    state
        .jobs
        .get_mut(state.focus.as_ref().unwrap())
        .unwrap()
        .state = JobState::Idle;
    store.commit(&state, &[input, started, result]).unwrap();
    assert_known_writes(&store, &state);
}

#[test]
fn complete_history_search_crosses_pages_and_excludes_opaque_native_parts() {
    let (_directory, store, state, input) = fixture();
    store.commit(&state, &[input]).unwrap();
    let mut expected = Vec::new();
    for index in 0..45 {
        let (kind, data) = match index {
            0 => (
                "input",
                json!({"message":[{"type":"text","text":format!("{} Needle 中文", "x".repeat(20_000))}]}),
            ),
            1 => (
                "input",
                json!({"message":[{"type":"text","text":"{\"custom\":\"LITERAL_JSON\"}"}]}),
            ),
            22 => (
                "model_message",
                json!({"response":{"choice":[{"type":"reasoning","text":"OPAQUE_ONLY"},{"type":"encrypted","content":[{"type":"text","text":"OPAQUE_ONLY"}]},{"type":"toolcall","function":{"arguments":{"text":"OPAQUE_ONLY","path":"src/searchable.rs","command":"cargo check"}}},{"type":"text","text":"needle in original model output"}]}}),
            ),
            44 => (
                "tool_result",
                json!({"message":[{"type":"toolresult","content":[{"type":"text","text":"{\"stdout\":\"NEEDLE tool output\",\"stderr\":\"real warning\",\"opaque\":\"OPAQUE_ONLY\"}"}]}]}),
            ),
            _ => (
                "context_note",
                json!({"message":[{"type":"text","text":"noise"}]}),
            ),
        };
        let event = call_event(&state, kind, "unused", data);
        if [0, 22, 44].contains(&index) {
            expected.push(event.id.clone());
        }
        store.commit(&state, &[event]).unwrap();
    }
    assert_eq!(
        store
            .history_search(&state.id, "LITERAL_JSON", 100)
            .unwrap()
            .len(),
        1
    );
    let found = store.history_search(&state.id, "needle", 100).unwrap();
    assert_eq!(
        found
            .iter()
            .map(|found| found.event_id.clone())
            .collect::<Vec<_>>(),
        expected
    );
    assert!(found[0].snippet.contains("Needle 中文"));
    assert!(
        found
            .iter()
            .all(|found| found.snippet.chars().count() <= 240)
    );
    assert_eq!(
        store.history_search(&state.id, "needle", 1).unwrap().len(),
        1
    );
    assert_eq!(
        store
            .history_search(&state.id, "searchable.rs", 100)
            .unwrap()[0]
            .event_id,
        expected[1]
    );
    assert_eq!(
        store.history_search(&state.id, "cargo check", 100).unwrap()[0].event_id,
        expected[1]
    );
    assert!(
        store
            .history_search(&state.id, "OPAQUE_ONLY", 100)
            .unwrap()
            .is_empty()
    );
    for (query, limit) in [("", 1), ("needle", 0), ("needle", 101)] {
        assert!(store.history_search(&state.id, query, limit).is_err());
    }
    let foreign = SessionState::new(state.workspace.clone());
    store.create_session(&foreign).unwrap();
    assert!(
        store
            .history_search(&foreign.id, "needle", 100)
            .unwrap()
            .is_empty()
    );
    // Read-only search leaves the snapshot and source event body intact.
    assert_eq!(store.load_session(&state.id).unwrap(), state);
    assert!(
        store
            .read_event(&state.id, &expected[0])
            .unwrap()
            .data
            .to_string()
            .contains(&"x".repeat(20_000))
    );
}
