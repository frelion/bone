use super::*;
use rig_core::completion::Usage;
use rig_core::message::{CallId, ToolCall, ToolFunction, ToolName, ToolResultContent};
use serde_json::{Value, json};

fn append(job: &mut Job, events: &mut BTreeMap<String, Event>, kind: &str, data: Value) -> String {
    let event = Event::new("session", kind, data);
    let id = event.id.clone();
    job.history.push(id.clone());
    events.insert(id.clone(), event);
    id
}

fn append_input(job: &mut Job, events: &mut BTreeMap<String, Event>, text: &str) -> String {
    append(
        job,
        events,
        "input",
        json!({"message":Message::user(text),"source":"user"}),
    )
}

fn append_response(
    job: &mut Job,
    events: &mut BTreeMap<String, Event>,
    native: CompletionResponse,
) -> String {
    append(job, events, "model_message", json!({"response":native}))
}

fn call_response(calls: Vec<ToolCall>) -> CompletionResponse {
    response(Message::Assistant {
        id: None,
        content: calls.into_iter().map(AssistantContent::ToolCall).collect(),
    })
}

fn original_input(message: &Message) -> Message {
    let Message::User { content } = message else {
        panic!("expected native user message")
    };
    let UserContent::Text(marker) = &content[0] else {
        panic!("missing routing text")
    };
    assert!(marker.text.starts_with("[BONE INPUT id="));
    Message::User {
        content: content[1..].to_vec(),
    }
}

fn response(message: Message) -> CompletionResponse {
    let Message::Assistant { content, .. } = message else {
        panic!("expected assistant")
    };
    CompletionResponse::new(content, Usage::default(), "openai", json!({}))
}

fn call(id: &str, name: &str) -> ToolCall {
    ToolCall::new(
        CallId::from_wire(id),
        ToolFunction {
            name: ToolName::new(name).unwrap(),
            arguments: json!({}),
        },
    )
}

#[test]
fn native_batch_ids_and_provider_fields_survive_history_replay() {
    let mut job = Job::new("work");
    let mut events = BTreeMap::new();
    let mut first = call("call-a", "read_file");
    first.signature = Some("provider-signature".into());
    first.additional_params = Some(json!({"opaque":"keep"}));
    let second = call("call-b", "list_files");
    let mut native = response(Message::Assistant {
        id: None,
        content: vec![
            AssistantContent::ToolCall(first.clone()),
            AssistantContent::ToolCall(second.clone()),
        ],
    });
    native.message_id = Some("assistant-provider-id".into());
    append(
        &mut job,
        &mut events,
        "model_message",
        json!({"response":native}),
    );
    let results = Message::tool_results(vec![
        first.result(vec![ToolResultContent::Json {
            value: json!({"text":"one"}),
        }]),
        second.result(vec![ToolResultContent::Json {
            value: json!({"files":[]}),
        }]),
    ]);
    append(
        &mut job,
        &mut events,
        "tool_result",
        json!({"message":results}),
    );
    let history = build_history(&job, &events).unwrap();
    assert_eq!(Some(history[0].clone()), native.message());
    assert_eq!(history[1], results);
}

#[test]
fn compaction_keeps_whole_batches_and_covers_the_maximal_complete_prefix() {
    let mut job = Job::new("work");
    let mut events = BTreeMap::new();
    let mut original_ids = Vec::new();
    for index in 0..6 {
        let first = call(&format!("a-{index}"), "read_file");
        let second = call(&format!("b-{index}"), "list_files");
        original_ids.push(append(
            &mut job,
            &mut events,
            "model_message",
            json!({"response":call_response(vec![first.clone(),second.clone()])}),
        ));
        original_ids.push(append(&mut job, &mut events, "tool_result", json!({"message":Message::tool_results(vec![first.result(vec![ToolResultContent::Json {value:json!({})}])])})));
        original_ids.push(append(&mut job, &mut events, "tool_result", json!({"message":Message::tool_results(vec![second.result(vec![ToolResultContent::Json {value:json!({})}])])})));
    }
    let (prefix, ids) = compaction_prefix(&job, &events, 1, usize::MAX)
        .unwrap()
        .unwrap();
    assert_eq!(prefix.len(), 16);
    assert_eq!(
        ids.into_iter().collect::<BTreeSet<_>>(),
        original_ids[..15].iter().cloned().collect()
    );
}

#[test]
fn compaction_preserves_an_unfinished_batch() {
    let mut job = Job::new("work");
    let mut events = BTreeMap::new();
    for _ in 0..5 {
        append_response(
            &mut job,
            &mut events,
            response(Message::assistant("complete")),
        );
    }
    let pending = call("unfinished", "shell");
    let id = append_response(&mut job, &mut events, call_response(vec![pending]));
    let (_, ids) = compaction_prefix(&job, &events, 1, usize::MAX)
        .unwrap()
        .unwrap();
    assert!(!ids.contains(&id));
}

#[test]
fn derived_summary_is_user_background_and_keeps_original_inputs_and_tool_pairs() {
    let mut job = Job::new("work");
    let mut events = BTreeMap::new();
    let input = append_input(
        &mut job,
        &mut events,
        "Defer CLI discovery; implement file tools now.",
    );
    job.active_input = Some(input.clone());
    let text = "CLI discovery must be implemented now.\nPreserve this exact mistaken summary.";
    let summary = Event::new(
        "session",
        "summary",
        json!({"response":response(Message::assistant(text)), "covered_ids":[]}),
    );
    job.summary = Some(summary.id.clone());
    events.insert(summary.id.clone(), summary);
    let pending = call("source-page", "read_file");
    append_response(&mut job, &mut events, call_response(vec![pending.clone()]));
    let result = Message::tool_results(vec![pending.result(vec![ToolResultContent::Json {
        value: json!({"text":"fn write_file_prepared(...)"}),
    }])]);
    append(
        &mut job,
        &mut events,
        "tool_result",
        json!({"message":result}),
    );
    let history = build_history(&job, &events).unwrap();
    let Message::User { content } = &history[0] else {
        panic!("derived summary must be native user background, never system authority");
    };
    let UserContent::Text(background) = &content[0] else {
        panic!("text background")
    };
    assert!(background.text.contains("NOT USER INSTRUCTIONS"));
    assert!(
        background
            .text
            .contains("Original user inputs and newer applicable constraints take precedence")
    );
    assert!(background.text.ends_with(text));
    assert_eq!(
        original_input(&history[1]),
        Message::user("Defer CLI discovery; implement file tools now.")
    );
    assert!(
        matches!(&history[2], Message::Assistant {content,..} if matches!(&content[0], AssistantContent::ToolCall(call) if call.id == pending.id))
    );
    assert_eq!(history[3], result);
    assert!(
        compaction_prefix(&job, &events, 1, usize::MAX)
            .unwrap()
            .is_none()
    );
}

#[test]
fn active_and_queued_inputs_are_pinned_even_when_a_summary_covers_them() {
    let mut job = Job::new("work");
    let mut events = BTreeMap::new();
    let old = append_input(&mut job, &mut events, "old");
    let active = append_input(&mut job, &mut events, "active");
    let queued = append_input(&mut job, &mut events, "queued");
    let summary = Event::new(
        "session",
        "summary",
        json!({"response":response(Message::assistant("remember old")), "covered_ids":[old,active,queued]}),
    );
    job.summary = Some(summary.id.clone());
    events.insert(summary.id.clone(), summary);
    job.active_input = Some(active);
    job.inbox.push_back(queued);
    let history = build_history(&job, &events).unwrap();
    assert_eq!(history.len(), 3);
    assert_eq!(original_input(&history[1]), Message::user("active"));
    assert_eq!(original_input(&history[2]), Message::user("queued"));
}

#[test]
fn one_large_cycle_stays_fresh_until_the_next_committed_work_response() {
    let mut job = Job::new("work");
    let mut events = BTreeMap::new();
    let call = call("large-read", "read_file");
    append_response(&mut job, &mut events, call_response(vec![call.clone()]));
    assert!(
        compaction_prefix(&job, &events, 1, usize::MAX)
            .unwrap()
            .is_none()
    );
    append(
        &mut job,
        &mut events,
        "tool_result",
        json!({"message":Message::tool_results(vec![call.result(vec![ToolResultContent::Json {value:json!({"text":"x".repeat(32*1024)})}])])}),
    );
    // A failed or cancelled work call did not consume these results.
    append(
        &mut job,
        &mut events,
        "model_failed",
        json!({"error":"cancelled before response"}),
    );
    assert!(
        compaction_prefix(&job, &events, 1, usize::MAX)
            .unwrap()
            .is_none()
    );
    let original = events.clone();
    let bounded = bounded_work_history(&job, &events, 8000).unwrap().unwrap();
    assert!(serde_json::to_string(&bounded).unwrap().chars().count() <= 8000);
    assert_eq!(
        bounded[0],
        build_history(&job, &events).unwrap()[0],
        "Native tool call arguments or identity changed"
    );
    assert_eq!(
        events, original,
        "Projection altered immutable audit events"
    );
    let next = self::call("next-work", "read_file");
    append_response(&mut job, &mut events, call_response(vec![next]));
    let (prefix, covered) = compaction_prefix(&job, &events, 1, usize::MAX)
        .unwrap()
        .unwrap();
    assert_eq!(prefix.len(), 3);
    assert_eq!(covered.len(), 2);
}

#[test]
fn ongoing_input_survives_compaction_of_its_older_tool_cycles() {
    let mut job = Job::new("work");
    let mut events = BTreeMap::new();
    let input = append_input(&mut job, &mut events, "keep working");
    job.active_input = Some(input.clone());
    for _ in 0..6 {
        append_response(
            &mut job,
            &mut events,
            response(Message::assistant("completed cycle")),
        );
    }
    let (prefix, covered_ids) = compaction_prefix(&job, &events, 1, usize::MAX)
        .unwrap()
        .unwrap();
    assert_eq!(original_input(&prefix[0]), Message::user("keep working"));
    assert!(!covered_ids.contains(&input));
    let summary = Event::new(
        "session",
        "summary",
        json!({"response":response(Message::assistant("earlier cycles completed")), "covered_ids":covered_ids}),
    );
    job.summary = Some(summary.id.clone());
    events.insert(summary.id.clone(), summary);
    let replay = build_history(&job, &events).unwrap();
    assert_eq!(original_input(&replay[1]), Message::user("keep working"));
    assert_eq!(replay.len(), 2);
}

#[test]
fn shared_user_instructions_keep_complete_text_and_can_enter_a_summary() {
    let mut job = Job::new("background work");
    let mut events = BTreeMap::new();
    let instruction =
        "Preserve every requirement: Unicode 中文, quoted \"constraints\", and the last sentence.";
    let mut shared = Event::new(
        "session",
        "input",
        json!({"message":Message::user(instruction),"source":"user"}),
    );
    shared.job_id = Some("other-job".into());
    shared.revision = 9;
    let shared_id = shared.id.clone();
    job.history.push(shared.id.clone());
    events.insert(shared.id.clone(), shared);
    for _ in 0..6 {
        append_response(
            &mut job,
            &mut events,
            response(Message::assistant("completed")),
        );
    }
    let history = build_history(&job, &events).unwrap();
    let Message::User { content } = &history[0] else {
        panic!("shared instruction lost user role")
    };
    let UserContent::Text(text) = &content[0] else {
        panic!("shared instruction lost text")
    };
    assert!(text.text.contains("status=SHARED"));
    assert!(text.text.contains("revision=9"));
    assert_eq!(original_input(&history[0]), Message::user(instruction));
    let (_, covered) = compaction_prefix(&job, &events, 1, usize::MAX)
        .unwrap()
        .unwrap();
    assert!(covered.contains(&shared_id));
}

#[test]
fn handed_off_active_input_stays_a_native_assigned_user_message() {
    let mut job = Job::new("handoff target");
    let mut events = BTreeMap::new();
    let mut input = Event::new(
        "session",
        "input",
        json!({"message":Message::user("do the assigned task"),"source":"user"}),
    );
    input.job_id = Some("handoff-source".into());
    job.active_input = Some(input.id.clone());
    job.history.push(input.id.clone());
    events.insert(input.id.clone(), input);
    let history = build_history(&job, &events).unwrap();
    assert_eq!(
        original_input(&history[0]),
        Message::user("do the assigned task")
    );
    assert!(
        serde_json::to_string(&history[0])
            .unwrap()
            .contains("status=ACTIVE")
    );
    assert!(
        !serde_json::to_string(&history[0])
            .unwrap()
            .contains("status=SHARED")
    );
}

#[test]
fn summary_prefix_fits_its_budget_without_splitting_a_long_tool_batch() {
    let mut job = Job::new("work");
    let mut events = BTreeMap::new();
    for index in 0..7 {
        let call = call(&format!("long-{index}"), "read_file");
        append_response(&mut job, &mut events, call_response(vec![call.clone()]));
        append(
            &mut job,
            &mut events,
            "tool_result",
            json!({"message":Message::tool_results(vec![call.result(vec![ToolResultContent::Json {value:json!({"text":"x".repeat(4000)})}])])}),
        );
    }
    let history = build_history(&job, &events).unwrap();
    let mut fitting = history[..4].to_vec();
    fitting.push(source_note(&job.history[..4]));
    let budget = serde_json::to_string(&fitting).unwrap().chars().count();
    let (prefix, ids) = compaction_prefix(&job, &events, 1, budget)
        .unwrap()
        .unwrap();
    assert_eq!(
        prefix.len(),
        5,
        "maximal fitting prefix should contain two whole cycles"
    );
    assert_eq!(ids.len(), 4);
    assert!(serde_json::to_string(&prefix).unwrap().chars().count() <= budget);
    let mut first_cycle = history[..2].to_vec();
    first_cycle.push(source_note(&job.history[..2]));
    let too_small = serde_json::to_string(&first_cycle).unwrap().chars().count() - 1;
    let (preview, _) = compaction_prefix(&job, &events, 1, too_small)
        .unwrap()
        .unwrap();
    assert!(serde_json::to_string(&preview).unwrap().chars().count() <= too_small);
    assert!(
        serde_json::to_string(&preview)
            .unwrap()
            .contains("Incomplete tool preview")
    );
}

#[test]
fn summary_retry_halves_original_source_and_keeps_complete_native_batches() {
    let mut job = Job::new("summary retry");
    let mut events = BTreeMap::new();
    let active = append_input(
        &mut job,
        &mut events,
        "Keep 中文, quotes \" and newlines\nverbatim.",
    );
    job.active_input = Some(active.clone());
    let queued = append_input(
        &mut job,
        &mut events,
        "Explain later; do not replace the task.",
    );
    job.inbox.push_back(queued.clone());
    for index in 0..6 {
        let first = call(&format!("first-{index}"), "read_file");
        let second = call(&format!("second-{index}"), "search_files");
        append_response(
            &mut job,
            &mut events,
            call_response(vec![first.clone(), second.clone()]),
        );
        append(
            &mut job,
            &mut events,
            "tool_result",
            json!({"message":Message::tool_results(vec![
                first.result(vec![ToolResultContent::Json {value:json!({"text":"quoted \" evidence\n".repeat(100)})}]),
                second.result(vec![ToolResultContent::Json {value:json!({"text":"matches".repeat(100)})}]),
            ])}),
        );
    }
    append_response(
        &mut job,
        &mut events,
        response(Message::assistant("Consumed the evidence.")),
    );
    let (source, covered) = compaction_prefix(&job, &events, 1, usize::MAX)
        .unwrap()
        .unwrap();
    let source_chars = serde_json::to_string(&source).unwrap().chars().count();
    append(
        &mut job,
        &mut events,
        "model_failed",
        json!({"response":response(Message::assistant("FAILED SUMMARY MUST NOT BE INPUT".repeat(100)))}),
    );
    let later = append_response(
        &mut job,
        &mut events,
        response(Message::assistant("LATER WORK MUST NOT BE INPUT")),
    );
    let durable_job = job.clone();
    let durable_events = events.clone();
    let (retry, retry_covered) = smaller_compaction_prefix(&job, &events, &covered, source_chars)
        .unwrap()
        .unwrap();
    let encoded = serde_json::to_string(&retry).unwrap();
    assert!(encoded.chars().count() <= source_chars / 2);
    assert!(retry_covered.len() < covered.len());
    assert!(!retry_covered.contains(&active));
    assert!(!retry_covered.contains(&queued));
    assert!(!retry_covered.contains(&later));
    assert!(!encoded.contains("FAILED SUMMARY"));
    assert!(!encoded.contains("LATER WORK"));
    assert_eq!(
        original_input(&retry[0]),
        Message::user("Keep 中文, quotes \" and newlines\nverbatim.")
    );
    assert_eq!(
        original_input(&retry[1]),
        Message::user("Explain later; do not replace the task.")
    );
    let mut calls = BTreeSet::new();
    for message in retry {
        match message {
            Message::Assistant { content, .. } => {
                assert!(calls.is_empty(), "an earlier native batch must be complete");
                for part in content {
                    if let AssistantContent::ToolCall(call) = part {
                        calls.insert(serde_json::to_string(&call.id).unwrap());
                    }
                }
            }
            Message::User { content } => {
                for part in content {
                    if let UserContent::ToolResult(result) = part {
                        assert!(calls.remove(&serde_json::to_string(&result.call).unwrap()));
                    }
                }
            }
            Message::System { .. } => {}
        }
    }
    assert!(calls.is_empty());
    assert_eq!(job, durable_job);
    assert_eq!(events, durable_events);
}

#[test]
fn summary_retry_stops_when_one_unchanged_cycle_cannot_fit_half_the_source() {
    let mut job = Job::new("indivisible source");
    let mut events = BTreeMap::new();
    let read = call("large-cycle", "read_file");
    append_response(&mut job, &mut events, call_response(vec![read.clone()]));
    append(
        &mut job,
        &mut events,
        "tool_result",
        json!({"message":Message::tool_results(vec![
            read.result(vec![ToolResultContent::Json {value:json!({"text":"x".repeat(8000)})}]),
        ])}),
    );
    append_response(
        &mut job,
        &mut events,
        response(Message::assistant("Consumed.")),
    );
    let (source, covered) = compaction_prefix(&job, &events, 1, usize::MAX)
        .unwrap()
        .unwrap();
    let chars = serde_json::to_string(&source).unwrap().chars().count();
    assert!(
        smaller_compaction_prefix(&job, &events, &covered, chars)
            .unwrap()
            .is_none()
    );
}

#[test]
fn candidate_summary_must_shrink_the_full_work_request_and_keeps_pending_inputs() {
    let mut job = Job::new("candidate summary");
    let mut events = BTreeMap::new();
    let old = append_input(&mut job, &mut events, &"old consumed evidence".repeat(200));
    let active = append_input(&mut job, &mut events, "Original ACTIVE request.");
    let queued = append_input(&mut job, &mut events, "Original QUEUED request.");
    job.active_input = Some(active.clone());
    job.inbox.push_back(queued.clone());
    let native = CompletionRequest::from(Vec::<Message>::new())
        .preamble("Work instructions and full tools stay.");
    let candidate = |text: String| {
        Event::new(
            "session",
            "summary",
            json!({"response":response(Message::assistant(text)),"covered_ids":[old,active,queued]}),
        )
    };
    assert!(
        summary_shrinks_work(
            &job,
            &events,
            &candidate("Consumed evidence.".into()),
            &native
        )
        .unwrap()
    );
    assert!(
        !summary_shrinks_work(
            &job,
            &events,
            &candidate("Longer summary".repeat(500)),
            &native
        )
        .unwrap()
    );
    let pending_only = Event::new(
        "session",
        "summary",
        json!({
            "response":response(Message::assistant("A replacement for pending inputs.")),
            "covered_ids":[active,queued],
        }),
    );
    assert!(
        !summary_shrinks_work(&job, &events, &pending_only, &native).unwrap(),
        "covering ACTIVE and QUEUED IDs must not let a summary erase their native messages"
    );
    assert!(job.summary.is_none());
    assert_eq!(
        original_input(&build_history(&job, &events).unwrap()[1]),
        Message::user("Original ACTIVE request.")
    );
}

#[test]
fn oversized_complete_tool_batch_has_explicit_native_previews_and_exact_source_ids() {
    let mut job = Job::new("large reads");
    let mut events = BTreeMap::new();
    let input = append_input(
        &mut job,
        &mut events,
        "Do not lose this full user requirement",
    );
    job.active_input = Some(input.clone());
    let first = call("large-a", "read_file");
    let second = call("large-b", "read_file");
    append_response(
        &mut job,
        &mut events,
        call_response(vec![first.clone(), second.clone()]),
    );
    let result = append(
        &mut job,
        &mut events,
        "tool_result",
        json!({"message":Message::tool_results(vec![first.result(vec![ToolResultContent::Json {value:json!({"text":"a".repeat(32*1024)})}]),second.result(vec![ToolResultContent::Json {value:json!({"text":"b".repeat(32*1024)})}])])}),
    );
    let original = events[&result].clone();
    let next = call("next-work", "read_file");
    append_response(&mut job, &mut events, call_response(vec![next]));
    let (prefix, covered) = compaction_prefix(&job, &events, 1, 16_000)
        .unwrap()
        .unwrap();
    assert!(serde_json::to_string(&prefix).unwrap().chars().count() <= 16_000);
    assert_eq!(
        original_input(&prefix[0]),
        Message::user("Do not lose this full user requirement")
    );
    assert!(!covered.contains(&input));
    assert!(covered.contains(&result));
    let Message::User { content } = &prefix[2] else {
        panic!("missing native tool results")
    };
    for (part, call) in content.iter().zip([first, second]) {
        let UserContent::ToolResult(tool) = part else {
            panic!("wrong native content")
        };
        assert_eq!(tool.call, call.id);
        assert_eq!(tool.name, call.function.name);
        let ToolResultContent::Json { value } = &tool.content[0] else {
            panic!("missing explicit preview")
        };
        assert_eq!(value["truncated"], true);
        assert_eq!(value["event_id"], result);
    }
    assert_eq!(
        events[&result], original,
        "original audit body must remain intact"
    );
}

#[test]
fn fresh_native_call_arguments_are_never_silently_shortened_to_fit() {
    let mut job = Job::new("large edit");
    let mut events = BTreeMap::new();
    let mut edit = call("edit", "write_file");
    edit.function.arguments = json!({"mode":"replace","path":"large.rs","content":"x".repeat(20_000),"expected_sha256":null});
    append_response(&mut job, &mut events, call_response(vec![edit.clone()]));
    append(
        &mut job,
        &mut events,
        "tool_result",
        json!({"message":Message::tool_result(edit.id,ToolName::new("write_file").unwrap(),"installed")}),
    );
    let original = events.clone();
    assert!(bounded_work_history(&job, &events, 8000).unwrap().is_none());
    assert_eq!(events, original);
}

#[test]
fn failed_old_read_only_input_is_queued_and_current_write_request_is_unambiguously_active() {
    let mut job = Job::new("continuing project");
    let mut events = BTreeMap::new();
    let mut old = Event::new(
        "session",
        "input",
        json!({"source":"user","message":Message::user("Only explain the existing code. Do not edit files.")}),
    );
    old.job_id = Some(job.id.clone());
    old.revision = 1;
    job.inbox.push_back(old.id.clone());
    job.history.push(old.id.clone());
    events.insert(old.id.clone(), old.clone());
    let native: Message=serde_json::from_value(json!({"role":"user","content":[{"type":"text","text":"Modify the UTC validation now. 中文 🦀","additional_params":{"provider_signature":"keep-exact"}},{"type":"text","text":"Verify \"quoted\" constraints and\nkeep original formatting."}]})).unwrap();
    let mut current = Event::new(
        "session",
        "input",
        json!({"source":"user","message":native}),
    );
    current.job_id = Some(job.id.clone());
    current.revision = 5;
    job.active_input = Some(current.id.clone());
    job.history.push(current.id.clone());
    events.insert(current.id.clone(), current.clone());
    let durable = events.clone();
    let history = build_history(&job, &events).unwrap();
    let old_rendered = serde_json::to_string(&history[0]).unwrap();
    assert!(old_rendered.contains("status=QUEUED"));
    assert!(old_rendered.contains(&old.id));
    assert!(old_rendered.contains("not the current task"));
    let current_rendered = serde_json::to_string(&history[1]).unwrap();
    assert!(current_rendered.contains("status=ACTIVE"));
    assert!(current_rendered.contains(&current.id));
    assert!(current_rendered.contains("revision=5"));
    assert_eq!(original_input(&history[1]), native);
    assert_eq!(
        original_input(&history[0]),
        Message::user("Only explain the existing code. Do not edit files.")
    );
    assert_eq!(events, durable);
}
