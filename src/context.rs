use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use rig_core::completion::{AssistantContent, CompletionRequest, CompletionResponse, Message};
use rig_core::message::UserContent;

use crate::state::{Event, Job};

fn event_message(event: &Event) -> Result<Option<Message>> {
    match event.kind.as_str() {
        "input" | "tool_result" | "tool_reconciled" | "context_note" => {
            let value = event.data.get("message").with_context(|| {
                format!("{} event {} has no native message", event.kind, event.id)
            })?;
            Ok(Some(serde_json::from_value(value.clone()).with_context(
                || format!("decoding native message in event {}", event.id),
            )?))
        }
        "model_message" => {
            let value = event
                .data
                .get("response")
                .context("model message has no native response")?;
            let response: CompletionResponse = serde_json::from_value(value.clone())?;
            Ok(response.message())
        }
        _ => Ok(None),
    }
}

fn summary(
    job: &Job,
    events: &BTreeMap<String, Event>,
) -> Result<(Option<Message>, BTreeSet<String>)> {
    let Some(id) = &job.summary else {
        return Ok((None, BTreeSet::new()));
    };
    let event = events
        .get(id)
        .with_context(|| format!("missing summary event {id}"))?;
    ensure!(
        event.kind == "summary",
        "job summary does not refer to a summary event"
    );
    let response: CompletionResponse = serde_json::from_value(
        event
            .data
            .get("response")
            .context("summary has no native response")?
            .clone(),
    )?;
    let covered: Vec<String> = serde_json::from_value(
        event
            .data
            .get("covered_ids")
            .context("summary has no covered history IDs")?
            .clone(),
    )?;
    let text = response.text();
    ensure!(!text.trim().is_empty(), "summary contains no text");
    Ok((
        Some(Message::system(format!(
            "Background summary of earlier work:\n{text}"
        ))),
        covered.into_iter().collect(),
    ))
}

fn pinned(job: &Job) -> BTreeSet<&str> {
    job.active_input
        .iter()
        .chain(job.inbox.iter())
        .map(String::as_str)
        .collect()
}

fn retained(
    job: &Job,
    events: &BTreeMap<String, Event>,
    covered: &BTreeSet<String>,
) -> Result<Vec<(String, Message)>> {
    let pinned = pinned(job);
    let mut seen = BTreeSet::new();
    let mut messages = Vec::new();
    for id in job
        .history
        .iter()
        .chain(job.active_input.iter())
        .chain(job.inbox.iter())
    {
        if !seen.insert(id) || (covered.contains(id) && !pinned.contains(id.as_str())) {
            continue;
        }
        let event = events
            .get(id)
            .with_context(|| format!("missing job history event {id}"))?;
        if pinned.contains(id.as_str()) {
            ensure!(
                event.kind == "input",
                "pending input ID does not refer to an input event"
            );
        }
        if let Some(mut message) = event_message(event)? {
            if event.kind == "input" {
                let (status, instruction) = if job.active_input.as_ref() == Some(id) {
                    (
                        "ACTIVE",
                        "This is the task assigned to this job now; fulfill it under the latest applicable session instructions.",
                    )
                } else if job.inbox.contains(id) {
                    (
                        "QUEUED",
                        "Retained for later continuation, not the current task. Do not execute or deliver this input in place of the ACTIVE task; an older conflicting instruction does not override a newer request.",
                    )
                } else if event.data["source"] == "user"
                    && event.job_id.as_ref().is_some_and(|owner| owner != &job.id)
                {
                    (
                        "SHARED",
                        "Public instruction from another job. Its relevant constraints and corrections remain authoritative, including when newer than the ACTIVE input. Do not take over its separately assigned work.",
                    )
                } else {
                    (
                        "HISTORICAL",
                        "Earlier input for background. Preserve continuing constraints, but do not repeat completed or superseded actions.",
                    )
                };
                let header = format!(
                    "[BONE INPUT id={}; revision={}; status={}; source={}; original_job={}]\n{}\nOriginal input follows:\n",
                    event.id,
                    event.revision,
                    status,
                    event.data["source"].as_str().unwrap_or("unknown"),
                    event.job_id.as_deref().unwrap_or("unknown"),
                    instruction
                );
                let Message::User { content } = &mut message else {
                    anyhow::bail!(
                        "input event {} does not contain a native user message",
                        event.id
                    );
                };
                // Add routing context in a separate native text part. Every
                // original part and its provider fields stay unchanged.
                content.insert(0, UserContent::text(header));
            }
            messages.push((id.clone(), message));
        }
    }
    Ok(messages)
}

pub fn build_history(job: &Job, events: &BTreeMap<String, Event>) -> Result<Vec<Message>> {
    let (summary, covered) = summary(job, events)?;
    let mut result = Vec::new();
    if let Some(summary) = summary {
        result.push(summary);
    }
    result.extend(
        retained(job, events, &covered)?
            .into_iter()
            .map(|(_, message)| message),
    );
    Ok(result)
}

/// Count the whole native request, including instructions, tools, and options.
pub fn serialized_chars(request: &CompletionRequest) -> Result<usize> {
    Ok(serde_json::to_string(request)?.chars().count())
}

fn source_note(ids: &[String]) -> Message {
    Message::user(format!(
        "Lookup index for the preceding transcript events, in order: {}. Keep only selected IDs needed to retrieve important evidence or unfinished requirements with job_inspect(event_id=...). Do not copy the full index into the summary, and do not describe model/tool event IDs as user-message IDs.",
        serde_json::to_string(ids).expect("string IDs serialize")
    ))
}

/// Return a completed history prefix for a native summarization call. Tool call
/// batches are indivisible; unfinished batches and pending inputs stay verbatim.
pub fn compaction_prefix(
    job: &Job,
    events: &BTreeMap<String, Event>,
    trigger_history_chars: usize,
    max_prefix_chars: usize,
) -> Result<Option<(Vec<Message>, Vec<String>)>> {
    let (background, covered) = summary(job, events)?;
    let entries = retained(job, events, &covered)?;
    let mut total = 0;
    if let Some(message) = &background {
        total += serde_json::to_string(message)?.chars().count();
    }
    for (_, message) in &entries {
        total += serde_json::to_string(message)?.chars().count();
    }
    if total <= trigger_history_chars {
        return Ok(None);
    }

    prefix_from_entries(job, background, entries, max_prefix_chars, true)
}

fn preview_tool_results(entries: &mut [(String, Message)], limit: usize) -> Result<bool> {
    let mut changed = false;
    for (id, message) in entries {
        if let Message::User { content } = message {
            for part in content {
                if let UserContent::ToolResult(result) = part {
                    let original = serde_json::to_string(&result.content)?;
                    if original.chars().count() <= limit {
                        continue;
                    }
                    result.content = vec![rig_core::message::ToolResultContent::Json {
                        value: serde_json::json!({
                            "truncated":true,"event_id":id,"preview":original.chars().take(limit/12).collect::<String>(),
                            "instruction":"Incomplete tool preview. Use job_inspect(event_id=...) to read the exact original result before claiming verification."
                        }),
                    }];
                    changed = true;
                }
            }
        }
    }
    Ok(changed)
}

fn prefix_from_entries(
    job: &Job,
    background: Option<Message>,
    mut entries: Vec<(String, Message)>,
    max_prefix_chars: usize,
    may_preview: bool,
) -> Result<Option<(Vec<Message>, Vec<String>)>> {
    let mut open_calls = BTreeSet::new();
    let mut cycle_open = false;
    let mut boundaries = Vec::new();
    for (index, (_, message)) in entries.iter().enumerate() {
        match message {
            Message::Assistant { content, .. } => {
                // A subsequent assistant before all results would make replay
                // ambiguous. Refuse to compact such a transcript.
                if !open_calls.is_empty() {
                    return Ok(None);
                }
                cycle_open = true;
                for part in content {
                    if let AssistantContent::ToolCall(call) = part {
                        let key = serde_json::to_string(&call.id)?;
                        if !open_calls.insert(key) {
                            return Ok(None);
                        }
                    }
                }
            }
            Message::User { content } => {
                for part in content {
                    if let UserContent::ToolResult(result) = part
                        && !open_calls.remove(&serde_json::to_string(&result.call)?)
                    {
                        return Ok(None);
                    }
                }
            }
            Message::System { .. } => {}
        }
        if cycle_open && open_calls.is_empty() {
            boundaries.push(index + 1);
            cycle_open = false;
        }
    }
    let Some(&candidate_end) = boundaries.last() else {
        return Ok(None);
    };
    let eligible: BTreeSet<_> = boundaries.into_iter().collect();
    let mut prefix_chars = background
        .as_ref()
        .map(|message| serde_json::to_string(message).map(|json| json.chars().count()))
        .transpose()?
        .unwrap_or(0);
    let mut message_count = usize::from(background.is_some());
    let mut end = None;
    for (index, (_, message)) in entries[..candidate_end].iter().enumerate() {
        prefix_chars += serde_json::to_string(message)?.chars().count();
        message_count += 1;
        let sources = entries[..=index]
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        let note_chars = serde_json::to_string(&source_note(&sources))?
            .chars()
            .count();
        let serialized_array_chars = prefix_chars + note_chars + message_count + 2;
        if serialized_array_chars > max_prefix_chars {
            break;
        }
        if eligible.contains(&(index + 1)) {
            end = Some(index + 1);
        }
    }
    let Some(end) = end else {
        if may_preview
            && preview_tool_results(&mut entries, (max_prefix_chars / 16).clamp(512, 2048))?
        {
            return prefix_from_entries(job, background, entries, max_prefix_chars, false);
        }
        return Ok(None);
    };
    let mut prefix = Vec::new();
    if let Some(background) = background {
        prefix.push(background);
    }
    let mut ids = BTreeSet::new();
    let pending = pinned(job);
    let mut new_covered = 0;
    for (id, message) in &entries[..end] {
        prefix.push(message.clone());
        // The input remains verbatim alongside the generated summary even
        // when the tool cycles it caused are compacted.
        if !pending.contains(id.as_str()) && ids.insert(id.clone()) {
            new_covered += 1;
        }
    }
    if new_covered == 0 {
        return Ok(None);
    }
    prefix.push(source_note(
        &entries[..end]
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>(),
    ));
    Ok(Some((prefix, ids.into_iter().collect())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::completion::Usage;
    use rig_core::message::{CallId, ToolCall, ToolFunction, ToolName, ToolResultContent};
    use serde_json::{Value, json};

    fn append(
        job: &mut Job,
        events: &mut BTreeMap<String, Event>,
        kind: &str,
        data: Value,
    ) -> String {
        let event = Event::new("session", kind, data);
        let id = event.id.clone();
        job.history.push(id.clone());
        events.insert(id.clone(), event);
        id
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
            original_ids.push(append(&mut job, &mut events, "model_message", json!({"response":response(Message::Assistant {id:None,content:vec![AssistantContent::ToolCall(first.clone()),AssistantContent::ToolCall(second.clone())]})})));
            original_ids.push(append(&mut job, &mut events, "tool_result", json!({"message":Message::tool_results(vec![first.result(vec![ToolResultContent::Json {value:json!({})}])])})));
            original_ids.push(append(&mut job, &mut events, "tool_result", json!({"message":Message::tool_results(vec![second.result(vec![ToolResultContent::Json {value:json!({})}])])})));
        }
        let (prefix, ids) = compaction_prefix(&job, &events, 1, usize::MAX)
            .unwrap()
            .unwrap();
        assert_eq!(prefix.len(), 19);
        assert_eq!(
            ids.into_iter().collect::<BTreeSet<_>>(),
            original_ids.into_iter().collect()
        );
    }

    #[test]
    fn compaction_preserves_an_unfinished_batch() {
        let mut job = Job::new("work");
        let mut events = BTreeMap::new();
        for _ in 0..5 {
            append(
                &mut job,
                &mut events,
                "model_message",
                json!({"response":response(Message::assistant("complete"))}),
            );
        }
        let pending = call("unfinished", "shell");
        let id = append(
            &mut job,
            &mut events,
            "model_message",
            json!({"response":response(Message::Assistant {id:None,content:vec![AssistantContent::ToolCall(pending)]})}),
        );
        let (_, ids) = compaction_prefix(&job, &events, 1, usize::MAX)
            .unwrap()
            .unwrap();
        assert!(!ids.contains(&id));
    }

    #[test]
    fn active_and_queued_inputs_are_pinned_even_when_a_summary_covers_them() {
        let mut job = Job::new("work");
        let mut events = BTreeMap::new();
        let old = append(
            &mut job,
            &mut events,
            "input",
            json!({"message":Message::user("old"),"source":"user"}),
        );
        let active = append(
            &mut job,
            &mut events,
            "input",
            json!({"message":Message::user("active"),"source":"user"}),
        );
        let queued = append(
            &mut job,
            &mut events,
            "input",
            json!({"message":Message::user("queued"),"source":"user"}),
        );
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
    fn one_large_complete_tool_cycle_can_be_compacted_but_an_unfinished_cycle_cannot() {
        let mut job = Job::new("work");
        let mut events = BTreeMap::new();
        let call = call("large-read", "read_file");
        append(
            &mut job,
            &mut events,
            "model_message",
            json!({"response":response(Message::Assistant {id:None,content:vec![AssistantContent::ToolCall(call.clone())]})}),
        );
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
        let input = append(
            &mut job,
            &mut events,
            "input",
            json!({"message":Message::user("keep working"),"source":"user"}),
        );
        job.active_input = Some(input.clone());
        for _ in 0..6 {
            append(
                &mut job,
                &mut events,
                "model_message",
                json!({"response":response(Message::assistant("completed cycle"))}),
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
        let instruction = "Preserve every requirement: Unicode 中文, quoted \"constraints\", and the last sentence.";
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
            append(
                &mut job,
                &mut events,
                "model_message",
                json!({"response":response(Message::assistant("completed"))}),
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
            append(
                &mut job,
                &mut events,
                "model_message",
                json!({"response":response(Message::Assistant {id:None,content:vec![AssistantContent::ToolCall(call.clone())]})}),
            );
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
    fn oversized_complete_tool_batch_has_explicit_native_previews_and_exact_source_ids() {
        let mut job = Job::new("large reads");
        let mut events = BTreeMap::new();
        let input = append(
            &mut job,
            &mut events,
            "input",
            json!({"message":Message::user("Do not lose this full user requirement"),"source":"user"}),
        );
        job.active_input = Some(input.clone());
        let first = call("large-a", "read_file");
        let second = call("large-b", "read_file");
        append(
            &mut job,
            &mut events,
            "model_message",
            json!({"response":response(Message::Assistant {id:None,content:vec![AssistantContent::ToolCall(first.clone()),AssistantContent::ToolCall(second.clone())]})}),
        );
        let result = append(
            &mut job,
            &mut events,
            "tool_result",
            json!({"message":Message::tool_results(vec![first.result(vec![ToolResultContent::Json {value:json!({"text":"a".repeat(32*1024)})}]),second.result(vec![ToolResultContent::Json {value:json!({"text":"b".repeat(32*1024)})}])])}),
        );
        let original = events[&result].clone();
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
}
