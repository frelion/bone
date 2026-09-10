//! A complete agent run with a deterministic model port.

use std::{error::Error, sync::Arc};

use bone_core::{
    Agent, AgentLimits, CallContext, CallError, CheckpointDraft, CompactInput, CoordinateInput,
    Input, InputId, InputStatus, KernelDecision, ModelPort, PortFuture, RouteDelivery, RouteTarget,
    WorkInput, WorkProposal, WorkStep,
};

struct DemoModel;

impl ModelPort for DemoModel {
    fn coordinate(
        &self,
        input: CoordinateInput,
        _: CallContext,
    ) -> PortFuture<Result<KernelDecision, CallError>> {
        let inputs = input.inputs.iter().map(|input| input.id).collect();
        Box::pin(async move {
            Ok(KernelDecision::Assign(vec![RouteDelivery {
                inputs,
                target: RouteTarget::New,
                handoff: "answer the request".into(),
            }]))
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
