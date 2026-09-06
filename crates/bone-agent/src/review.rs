//! Deliberately small input view for interruption review.

use crate::{JobOutput, JobRequest, JobState, ModelInput, ModelTask, RecordKind};
use serde_json::{Value, json};

pub(crate) fn review_context(input: &ModelInput) -> Value {
    let ModelTask::ReviewInput { messages } = &input.task else {
        unreachable!("only review calls use the review projection");
    };
    let snapshot = &input.snapshot;
    // Preserve the original input to the current work. Follow-up work may have
    // an empty batch, so find its most recent preceding batch in this generation.
    let work_messages = snapshot.jobs.iter().rev().find_map(|job| {
        if job.generation != snapshot.generation {
            return None;
        }
        match &job.request {
            JobRequest::Work { messages } if !messages.is_empty() => Some(messages),
            _ => None,
        }
    });
    let user_context = snapshot
        .record
        .iter()
        .filter_map(|entry| match &entry.kind {
            RecordKind::UserMessage(message)
                if work_messages.is_some_and(|ids| ids.contains(&message.id)) =>
            {
                Some(message)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let work_note = snapshot.record.iter().rev().find_map(|entry| {
        let RecordKind::PlanAccepted { job } = entry.kind else {
            return None;
        };
        let job = snapshot.jobs.iter().find(|candidate| candidate.id == job)?;
        if job.generation != snapshot.generation {
            return None;
        }
        let JobState::Finished(outcome) = &job.state else {
            return None;
        };
        let Ok(JobOutput::Work(work)) = &outcome.result else {
            return None;
        };
        // Public notes can grow during pure reasoning; review needs only a
        // brief description. It never receives reply or tool result contents.
        Some(work.note.chars().take(1024).collect::<String>())
    });
    let jobs = snapshot
        .jobs
        .iter()
        .map(|job| {
            let (kind, tool) = match &job.request {
                JobRequest::Work { .. } => ("work", None),
                JobRequest::ReviewInput { .. } => ("input_review", None),
                JobRequest::Tool(call) => ("tool", Some(call.name.as_str())),
            };
            let status = match &job.state {
                JobState::Running => "running",
                JobState::CancelRequested => "cancel_requested",
                JobState::Finished(_) if job.is_unresolved() => "outcome_unknown",
                JobState::Finished(outcome) if outcome.result.is_err() => "failed",
                JobState::Finished(_) => "finished",
            };
            json!({
                "id": job.id, "kind": kind, "tool": tool, "status": status,
                "external_write": job.external_write,
                "progress": job.progress.as_ref().map(|progress| json!({
                    "message": progress.message.chars().take(1024).collect::<String>(),
                    "percent": progress.percent
                }))
            })
        })
        .collect::<Vec<_>>();
    json!({
        "record_cursor": snapshot.record_cursor,
        "messages": messages,
        "user_context": user_context,
        "requirement": snapshot.requirement,
        "work_note": work_note,
        "autonomous": snapshot.autonomous,
        "work": snapshot.work.or(snapshot.candidate),
        "jobs": jobs
    })
}
