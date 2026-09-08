use crate::{CallId, EffectId, Event, InputId, JobId, Notice, WakeId};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tokio::sync::{Notify, watch};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Input {
    pub id: InputId,
    pub text: String,
    /// Clarifies or corrects an unresolved input; not inferred by transport.
    pub reply_to: Option<InputId>,
}
impl Input {
    pub fn new(id: InputId, text: impl Into<String>) -> Self {
        Self {
            id,
            text: text.into(),
            reply_to: None,
        }
    }
    pub fn replying_to(mut self, id: InputId) -> Self {
        self.reply_to = Some(id);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AdmissionError {
    #[error("input capacity is temporarily full")]
    Busy,
    #[error("input ID was already used with different content")]
    ConflictingInput,
    #[error("the referenced input is not awaiting clarification or correction")]
    InvalidReply,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InputSnapshot {
    pub input: Input,
    pub received_at: u64,
    pub state: InputState,
    /// Delivery obligations, not every job merely affected by this input.
    pub required_jobs: Vec<JobId>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputState {
    Pending,
    Routing,
    Investigating { job: JobId },
    WaitingForUser { question: String },
    RoutingFailed { message: String },
    Handled,
    Finished(InputOutcome),
}
impl InputState {
    pub fn unresolved(&self) -> bool {
        !matches!(self, Self::Handled | Self::Finished(_))
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputOutcome {
    Completed,
    Cancelled,
    Failed { message: String },
}

/// An owned observation. Running calls' inputs never change underneath them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub record_cursor: u64,
    pub generation: u64,
    pub constraints: String,
    pub inputs: Vec<InputSnapshot>,
    pub jobs: Vec<JobSnapshot>,
    pub calls: Vec<CallSnapshot>,
    pub record: Vec<RecordEntry>,
    pub tools: Vec<ToolSpec>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JobSnapshot {
    pub id: JobId,
    pub goal: String,
    pub state: JobState,
    pub version: u64,
    pub inputs: Vec<InputId>,
    pub parent: Option<JobId>,
    /// Evidence relationships do not confer ownership or cancellation rights.
    pub references: Vec<JobId>,
    pub note: String,
    pub results: Vec<Value>,
    /// The one worker currently allowed to submit a proposal for this job.
    pub active_call: Option<CallId>,
    pub progress: Option<CallProgress>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobState {
    Ready,
    Running,
    Waiting(WaitReason),
    Paused,
    Completed,
    Cancelled,
    Failed { message: String },
}
impl JobState {
    pub fn terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Cancelled | Self::Failed { .. }
        )
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WaitReason {
    Tools,
    Timer,
    User { question: String },
    Result { job: JobId },
    Job { job: JobId },
    Coordination,
    Capacity,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CallSnapshot {
    pub id: CallId,
    pub effect_id: EffectId,
    pub job: Option<JobId>,
    pub request: CallRequest,
    pub version: u64,
    pub generation: u64,
    pub as_of: u64,
    /// Taken from the registered adapter, never from model output.
    pub external_write: bool,
    pub state: CallState,
    pub progress: Option<CallProgress>,
}
impl CallSnapshot {
    pub fn is_running(&self) -> bool {
        !matches!(self.state, CallState::Finished(_))
    }
    pub fn is_unresolved(&self) -> bool {
        match &self.state {
            CallState::Finished(outcome) => outcome.external_effect == ExternalEffect::Unknown,
            _ => true,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum CallState {
    Running,
    CancelRequested,
    Finished(CallOutcome),
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum CallRequest {
    Kernel {
        inputs: Vec<InputId>,
        source: Option<JobId>,
        request: Option<String>,
    },
    Work {
        job: JobId,
        interactive: bool,
    },
    Tool(ToolCall),
}
#[derive(Clone, Debug)]
pub enum Call {
    Model(ModelInput),
    Tool(ToolCall),
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelInput {
    pub task: ModelTask,
    pub snapshot: Snapshot,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ModelTask {
    Kernel {
        inputs: Vec<Input>,
        source: Option<JobId>,
        request: Option<String>,
    },
    Work {
        job: JobId,
        messages: Vec<Input>,
    },
}

/// Semantic routing only; code checks and commits the complete change set.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KernelDecision {
    pub changes: Vec<JobChange>,
    pub constraints: Option<String>,
    pub disposition: RoutingDisposition,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoutingDisposition {
    #[default]
    Apply,
    Investigate {
        goal: String,
    },
    Clarify {
        question: String,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum JobChange {
    Create(JobSpec),
    Update {
        job: JobId,
        goal: Option<String>,
        action: JobAction,
        inputs: Vec<InputId>,
        /// Wait for delivery, rather than merely acknowledge this control.
        required: bool,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobSpec {
    pub goal: String,
    pub inputs: Vec<InputId>,
    pub parent: Option<JobId>,
    pub references: Vec<JobId>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobAction {
    Keep,
    Pause,
    Resume,
    Cancel,
}

/// Full-capability work within one job; cannot edit other jobs' state.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkProposal {
    pub note: String,
    pub reply: Option<String>,
    pub operation: Option<ToolCall>,
    pub next: Next,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Next {
    Continue,
    Wait { reconsider_after: Option<Duration> },
    WaitForResult { job: JobId },
    WaitForJob { job: JobId },
    AskUser { question: String },
    Coordinate { request: String },
    Finish,
}
impl Default for Next {
    fn default() -> Self {
        Self::Wait {
            reconsider_after: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    pub name: String,
    pub arguments: Value,
}
impl ToolCall {
    pub fn new(name: impl Into<String>, arguments: Value) -> Self {
        Self {
            name: name.into(),
            arguments,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolEffect {
    ReadOnly,
    ExternalWrite,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub effect: ToolEffect,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallProgress {
    pub message: String,
    pub percent: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CallOutcome {
    pub result: Result<CallOutput, CallError>,
    pub external_effect: ExternalEffect,
}
impl CallOutcome {
    pub fn work(proposal: WorkProposal) -> Self {
        Self {
            result: Ok(CallOutput::Work(proposal)),
            external_effect: ExternalEffect::None,
        }
    }
    pub fn kernel(decision: KernelDecision) -> Self {
        Self {
            result: Ok(CallOutput::Kernel(decision)),
            external_effect: ExternalEffect::None,
        }
    }
    pub fn artifact(value: impl Into<Value>) -> Self {
        Self {
            result: Ok(CallOutput::Artifact(value.into())),
            external_effect: ExternalEffect::None,
        }
    }
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            result: Err(CallError::new(message)),
            external_effect: ExternalEffect::None,
        }
    }
    pub fn unknown(message: impl Into<String>) -> Self {
        Self {
            result: Err(CallError::new(message)),
            external_effect: ExternalEffect::Unknown,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum CallOutput {
    Kernel(KernelDecision),
    Work(WorkProposal),
    Artifact(Value),
}
/// Remote truth is independent of local cancellation, timeout, or job state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExternalEffect {
    None,
    Applied,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CallErrorKind {
    Failed,
    Cancelled,
    TimedOut,
    Panicked,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct CallError {
    pub kind: CallErrorKind,
    pub message: String,
}
impl CallError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            kind: CallErrorKind::Failed,
            message: message.into(),
        }
    }
}

/// One completed state transition and its authorized effects.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepEvent {
    pub sequence: u64,
    pub elapsed: Duration,
    pub event: Event,
    pub records: Vec<RecordEntry>,
    pub effects: Vec<EffectSummary>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum EffectSummary {
    Start {
        id: CallId,
        job: Option<JobId>,
        request: CallRequest,
        timeout: Option<Duration>,
    },
    RequestCancel {
        id: CallId,
    },
    WakeAfter {
        id: WakeId,
        delay: Duration,
    },
    CancelWake {
        id: WakeId,
    },
    Publish(Notice),
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecordEntry {
    pub cursor: u64,
    pub kind: RecordKind,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RecordKind {
    InputAccepted(Input),
    Material { job: JobId, note: String },
    ProposalDiscarded { call: CallId, reason: String },
    Notice(Notice),
}

/// Cooperative cancellation and coalesced progress; no access to kernel state.
#[derive(Clone)]
pub struct CallContext {
    progress: watch::Sender<Option<CallProgress>>,
    progress_ready: Arc<Notify>,
    cancellation: watch::Receiver<bool>,
}
impl CallContext {
    pub(crate) fn new(
        progress: watch::Sender<Option<CallProgress>>,
        progress_ready: Arc<Notify>,
        cancellation: watch::Receiver<bool>,
    ) -> Self {
        Self {
            progress,
            progress_ready,
            cancellation,
        }
    }
    pub fn report_progress(&self, progress: CallProgress) -> bool {
        if self.progress.is_closed() {
            return false;
        }
        self.progress.send_replace(Some(progress));
        self.progress_ready.notify_one();
        true
    }
    pub fn cancellation_requested(&self) -> bool {
        *self.cancellation.borrow()
    }
    pub async fn wait_for_cancellation(&mut self) {
        while !self.cancellation_requested() {
            if self.cancellation.changed().await.is_err() {
                break;
            }
        }
    }
}
/// Exactly one provider invocation, without a private loop or shared lock.
pub trait ModelPort: Send + Sync + 'static {
    fn infer(&self, input: ModelInput, context: CallContext) -> BoxFuture<'static, CallOutcome>;
}
pub trait ToolPort: Send + Sync + 'static {
    fn specification(&self) -> ToolSpec;
    fn run(&self, arguments: Value, context: CallContext) -> BoxFuture<'static, CallOutcome>;
}
