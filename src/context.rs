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
            if event.kind == "input"
                && event.data["source"] == "user"
                && event.job_id.as_deref() != Some(job.id.as_str())
                && !pinned.contains(id.as_str())
            {
                message = Message::user(format!(
                    "Shared session instruction, not your assigned task; revision {}: {}",
                    event.revision,
                    serde_json::to_string(&message)?
                ));
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
        let serialized_array_chars = prefix_chars + message_count.saturating_sub(1) + 2;
        if serialized_array_chars > max_prefix_chars {
            break;
        }
        if eligible.contains(&(index + 1)) {
            end = Some(index + 1);
        }
    }
    let Some(end) = end else { return Ok(None) };
    let mut prefix = Vec::new();
    if let Some(background) = background {
        prefix.push(background);
    }
    let mut ids = covered;
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
        assert_eq!(prefix.len(), 18);
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
        assert_eq!(history[1], Message::user("active"));
        assert_eq!(history[2], Message::user("queued"));
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
        assert_eq!(prefix.len(), 2);
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
        assert_eq!(prefix[0], Message::user("keep working"));
        assert!(!covered_ids.contains(&input));
        let summary = Event::new(
            "session",
            "summary",
            json!({"response":response(Message::assistant("earlier cycles completed")), "covered_ids":covered_ids}),
        );
        job.summary = Some(summary.id.clone());
        events.insert(summary.id.clone(), summary);
        let replay = build_history(&job, &events).unwrap();
        assert_eq!(replay[1], Message::user("keep working"));
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
        assert!(
            text.text
                .starts_with("Shared session instruction, not your assigned task; revision 9:")
        );
        let serialized = text.text.split_once(": ").unwrap().1;
        assert_eq!(
            serde_json::from_str::<Message>(serialized).unwrap(),
            Message::user(instruction)
        );
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
        assert_eq!(
            build_history(&job, &events).unwrap(),
            vec![Message::user("do the assigned task")]
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
        let budget = serde_json::to_string(&history[..4])
            .unwrap()
            .chars()
            .count();
        let (prefix, ids) = compaction_prefix(&job, &events, 1, budget)
            .unwrap()
            .unwrap();
        assert_eq!(
            prefix.len(),
            4,
            "maximal fitting prefix should contain two whole cycles"
        );
        assert_eq!(ids.len(), 4);
        assert!(serde_json::to_string(&prefix).unwrap().chars().count() <= budget);
        let too_small = serde_json::to_string(&history[..2])
            .unwrap()
            .chars()
            .count()
            - 1;
        assert!(
            compaction_prefix(&job, &events, 1, too_small)
                .unwrap()
                .is_none()
        );
    }
}
