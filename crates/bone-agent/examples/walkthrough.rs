//! A complete agent run with a deterministic model port.

use std::{error::Error, sync::Arc};

use bone_agent::{
    Agent, AgentLimits, Assignment, CallContext, CallError, CheckpointDraft, CompactInput,
    CoordinateInput, Input, InputId, InputStatus, JobChange, JobSpec, KernelDecision, ModelPort,
    PortFuture, WorkInput, WorkProposal, WorkStep,
};

struct DemoModel;

impl ModelPort for DemoModel {
    fn coordinate(
        &self,
        input: CoordinateInput,
        _: CallContext,
    ) -> PortFuture<Result<KernelDecision, CallError>> {
        let mut assignment = Assignment::new(JobSpec::new(
            "answer the request",
            "the supplied user input",
            "return a clear answer",
        ));
        assignment.inputs = input.inputs.iter().map(|input| input.id).collect();
        Box::pin(async move {
            Ok(KernelDecision::Apply {
                changes: vec![JobChange::Create(assignment)],
                constraints: None,
            })
        })
    }

    fn work(
        &self,
        input: WorkInput,
        _: CallContext,
    ) -> PortFuture<Result<WorkProposal, CallError>> {
        Box::pin(async move {
            Ok(WorkProposal::new(WorkStep::Finish(
                bone_agent::Completion::new(format!("finished {}", input.spec.goal)),
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
                bone_agent::RecordBody::InputFinished { input: id, .. } if id == input.id
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
