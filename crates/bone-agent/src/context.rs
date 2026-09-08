//! Scoped context for the Kernel's global directory and each worker's own job.

use crate::{CallRequest, ModelInput, ModelTask, RecordKind};
use serde_json::{Value, json};

pub(crate) fn model_context(input: &ModelInput) -> Result<Value, &'static str> {
    let snapshot = &input.snapshot;
    match &input.task {
        ModelTask::Kernel {
            inputs,
            source,
            request,
        } => {
            // A global directory, not a broadcast of job histories or tool data.
            let directory = snapshot
                .jobs
                .iter()
                .map(|job| {
                    json!({
                        "id": job.id,
                        "goal": job.goal,
                        "state": job.state,
                        "version": job.version,
                        "inputs": job.inputs,
                        "parent": job.parent,
                        "references": job.references,
                        "note": job.note.chars().take(1024).collect::<String>(),
                        "progress": job.progress.as_ref().map(|progress| json!({
                            "message": progress.message.chars().take(1024).collect::<String>(),
                            "percent": progress.percent
                        }))
                    })
                })
                .collect::<Vec<_>>();
            Ok(json!({
                "record_cursor": snapshot.record_cursor,
                "generation": snapshot.generation,
                "constraints": snapshot.constraints,
                "inputs": inputs,
                "source": source,
                "request": request,
                "directory": directory
            }))
        }
        ModelTask::Work { job, messages } => {
            let own_job = snapshot
                .jobs
                .iter()
                .find(|candidate| candidate.id == *job)
                .ok_or("model input is missing its own job")?;
            let references = snapshot
                .jobs
                .iter()
                .filter(|candidate| own_job.references.contains(&candidate.id))
                .map(|reference| {
                    json!({
                        "id": reference.id,
                        "goal": reference.goal,
                        "state": reference.state,
                        "version": reference.version,
                        "note": reference.note,
                        "results": reference.results,
                        "progress": reference.progress
                    })
                })
                .collect::<Vec<_>>();
            let tool_calls = snapshot
                .calls
                .iter()
                .filter(|call| {
                    call.job == Some(*job) && matches!(call.request, CallRequest::Tool(_))
                })
                .collect::<Vec<_>>();
            // Preserve this job's own accumulated work, including material from
            // a discarded proposal, without replaying the global event record.
            let material = snapshot.record.iter().filter(|entry| {
                matches!(&entry.kind, RecordKind::Material { job: owner, .. } if owner == job)
            }).collect::<Vec<_>>();
            Ok(json!({
                "record_cursor": snapshot.record_cursor,
                "generation": snapshot.generation,
                "constraints": snapshot.constraints,
                "messages": messages,
                "own_job": own_job,
                "references": references,
                "material": material,
                "tool_calls": tool_calls,
                "tools": snapshot.tools
            }))
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{
        CallId, CallOutcome, CallProgress, CallSnapshot, CallState, EffectId, Input, InputId,
        InputSnapshot, InputState, JobId, JobSnapshot, JobState, RecordEntry, Snapshot, ToolCall,
        ToolEffect, ToolSpec,
    };

    pub(crate) fn input(routing: bool) -> ModelInput {
        let original = Input::new(
            InputId(1),
            "原文：查明 A 的问题，保留 B。\nDo not discard this line.",
        );
        let jobs = (1..=4)
            .map(|id| JobSnapshot {
                id: JobId(id),
                goal: format!("job {id}"),
                state: JobState::Running,
                version: 2,
                inputs: vec![InputId(id)],
                parent: None,
                references: match id {
                    1 => vec![JobId(2)],
                    2 => vec![JobId(3)],
                    _ => vec![],
                },
                note: format!("public note {id}"),
                results: vec![json!(match id {
                    1 => "own-result",
                    2 => "referenced-result",
                    3 => "transitive-secret",
                    _ => "unrelated-secret",
                })],
                active_call: Some(CallId(id)),
                progress: Some(CallProgress {
                    message: "public progress".into(),
                    percent: Some(20),
                }),
            })
            .collect();
        let calls = [1, 4]
            .into_iter()
            .map(|id| CallSnapshot {
                id: CallId(id + 10),
                effect_id: EffectId(id + 10),
                job: Some(JobId(id)),
                request: CallRequest::Tool(ToolCall::new("read", json!({"path": "note.txt"}))),
                version: 2,
                generation: 1,
                as_of: 3,
                external_write: false,
                state: CallState::Finished(CallOutcome::artifact(if id == 1 {
                    "own-tool-result"
                } else {
                    "unrelated-tool-secret"
                })),
                progress: None,
            })
            .collect();
        ModelInput {
            task: if routing {
                ModelTask::Kernel {
                    inputs: vec![original],
                    source: Some(JobId(1)),
                    request: Some("Please pause job 2".into()),
                }
            } else {
                ModelTask::Work {
                    job: JobId(1),
                    messages: vec![original],
                }
            },
            snapshot: Snapshot {
                record_cursor: 8,
                generation: 1,
                constraints: "Do not send automatically".into(),
                inputs: vec![InputSnapshot {
                    input: Input::new(InputId(9), "unrelated-input-secret"),
                    received_at: 2,
                    state: InputState::Pending,
                    required_jobs: vec![],
                }],
                jobs,
                calls,
                record: [1, 4]
                    .into_iter()
                    .map(|id| RecordEntry {
                        cursor: id,
                        kind: RecordKind::Material {
                            job: JobId(id),
                            note: if id == 1 {
                                "own-material"
                            } else {
                                "unrelated-material-secret"
                            }
                            .into(),
                        },
                    })
                    .collect(),
                tools: vec![ToolSpec {
                    name: "read".into(),
                    description: "tool-definition-sentinel".into(),
                    parameters: json!({"type": "object"}),
                    effect: ToolEffect::ReadOnly,
                }],
            },
        }
    }

    #[test]
    fn kernel_sees_original_input_and_public_directory_without_tool_or_result_contents() {
        let mut input = input(true);
        input.snapshot.jobs[0].note = "文".repeat(1100);
        let context = model_context(&input).unwrap();
        assert_eq!(
            context["inputs"][0]["text"],
            "原文：查明 A 的问题，保留 B。\nDo not discard this line."
        );
        assert_eq!(context["source"], 1);
        assert_eq!(context["request"], "Please pause job 2");
        assert_eq!(context["directory"].as_array().unwrap().len(), 4);
        assert_eq!(
            context["directory"][0]["note"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            1024
        );
        let body = context.to_string();
        for private in [
            "own-result",
            "referenced-result",
            "transitive-secret",
            "unrelated-secret",
            "own-tool-result",
            "unrelated-tool-secret",
            "tool-definition-sentinel",
            "unrelated-input-secret",
            "own-material",
            "unrelated-material-secret",
        ] {
            assert!(!body.contains(private), "Kernel context leaked {private}");
        }
        assert_eq!(context["constraints"], "Do not send automatically");
    }

    #[test]
    fn worker_sees_only_own_job_material_direct_references_and_owned_tools() {
        let context = model_context(&input(false)).unwrap();
        assert_eq!(context["own_job"]["id"], 1);
        assert_eq!(context["references"].as_array().unwrap().len(), 1);
        assert_eq!(context["references"][0]["id"], 2);
        assert_eq!(context["tool_calls"].as_array().unwrap().len(), 1);
        assert_eq!(context["tool_calls"][0]["job"], 1);
        let body = context.to_string();
        for evidence in [
            "own-result",
            "referenced-result",
            "own-tool-result",
            "own-material",
            "tool-definition-sentinel",
        ] {
            assert!(body.contains(evidence), "worker lost {evidence}");
        }
        for private in [
            "transitive-secret",
            "unrelated-secret",
            "unrelated-tool-secret",
            "unrelated-input-secret",
            "unrelated-material-secret",
        ] {
            assert!(!body.contains(private), "worker context leaked {private}");
        }
        assert!(context.get("snapshot").is_none());
        assert!(context.get("directory").is_none());
        assert_eq!(context["constraints"], "Do not send automatically");
    }

    #[test]
    fn missing_own_job_fails_instead_of_falling_back_to_the_global_snapshot() {
        let mut input = input(false);
        input.snapshot.jobs.retain(|job| job.id != JobId(1));
        assert_eq!(
            model_context(&input),
            Err("model input is missing its own job")
        );
    }
}
