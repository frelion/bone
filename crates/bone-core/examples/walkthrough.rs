//! A complete agent run with a deterministic model port.

use std::{error::Error, sync::Arc};

use bone_core::{
    Agent, AgentLimits, Assignment, CallContext, CallError, CheckpointDraft, CompactInput,
    ConversationInput, ConversationStep, Input, InputId, InputOutcome, InputStatus, JobSpec,
    JobStatus, ModelPort, PortFuture, WorkInput, WorkProposal, WorkStep,
};

struct DemoModel;

impl ModelPort for DemoModel {
    fn converse(
        &self,
        input: ConversationInput,
        _: CallContext,
    ) -> PortFuture<Result<ConversationStep, CallError>> {
        let inputs = input.inputs.iter().map(|input| input.id).collect();
        Box::pin(async move {
            if input.jobs.is_empty() {
                Ok(ConversationStep::Start(vec![Assignment {
                    inputs,
                    ..Assignment::new(JobSpec::new(
                        "show one complete lifecycle",
                        "demo",
                        "report the result",
                    ))
                }]))
            } else if input
                .jobs
                .iter()
                .all(|job| matches!(job.status, JobStatus::Finished(_)))
            {
                Ok(ConversationStep::Reply {
                    inputs,
                    text: "The demonstration completed.".into(),
                    outcome: InputOutcome::Completed,
                })
            } else {
                Ok(ConversationStep::Wait)
            }
        })
    }

    fn work(
        &self,
        input: WorkInput,
        _: CallContext,
    ) -> PortFuture<Result<WorkProposal, CallError>> {
        Box::pin(async move {
            Ok(WorkProposal::new(WorkStep::Finish(
                bone_core::Completion::new(format!("finished {}", input.spec.goal)),
            )))
        })
    }

    fn compact(
        &self,
        _: CompactInput,
        _: CallContext,
    ) -> PortFuture<Result<CheckpointDraft, CallError>> {
        Box::pin(async {
            Ok(CheckpointDraft {
                summary: "nothing to compact in this example".into(),
                evidence: Vec::new(),
            })
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let agent = Agent::with_ports(Arc::new(DemoModel), Vec::new(), AgentLimits::default())?;
    let input = Input::new(InputId(1), "show one complete lifecycle");
    agent.post(input.clone()).await?;

    let mut observation = agent.observe().await?;
    let finished =
        observation.baseline.inputs.iter().any(|item| {
            item.input.id == input.id && matches!(item.status, InputStatus::Finished(_))
        });
    if !finished {
        loop {
            let record = observation.records.recv().await?;
            if matches!(
                record.body,
                bone_core::RecordBody::InputFinished { input: id, .. } if id == input.id
            ) {
                break;
            }
        }
    }

    let view = agent.observe().await?.baseline;
    println!(
        "{} job completed; {} records retained",
        view.jobs.len(),
        view.records.len()
    );
    agent.shutdown().await?;
    Ok(())
}
