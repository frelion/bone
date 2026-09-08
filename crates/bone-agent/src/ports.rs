use std::{future::Future, pin::Pin, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, watch};

use crate::{
    CallId, CheckpointDraft, CompactInput, CoordinateInput, InputId, JobId, JobView,
    KernelDecision, Seq, WorkInput, WorkProposal,
};

pub type PortFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub id: InputId,
    pub text: String,
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

    pub fn replying_to(mut self, input: InputId) -> Self {
        self.reply_to = Some(input);
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputReceipt {
    pub id: InputId,
    pub accepted_at: Seq,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputStatus {
    Routing,
    WaitingForUser { question: String },
    RoutingFailed { message: String },
    Handled,
    Finished(InputOutcome),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputOutcome {
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputView {
    pub input: Input,
    pub accepted_at: Seq,
    pub status: InputStatus,
    pub required_jobs: Vec<JobId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            kind: CallErrorKind::Failed,
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolOutcome {
    pub result: Result<Value, CallError>,
    pub external_effect: ExternalEffect,
}

impl ToolOutcome {
    pub fn value(value: impl Into<Value>) -> Self {
        Self {
            result: Ok(value.into()),
            external_effect: ExternalEffect::None,
        }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            result: Err(CallError::failed(message)),
            external_effect: ExternalEffect::None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CallKind {
    Coordinate,
    Work,
    Compact,
    Tool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum CallStatus {
    Running,
    CancelRequested,
    Finished {
        error: Option<CallError>,
        external_effect: ExternalEffect,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CallView {
    pub id: CallId,
    pub kind: CallKind,
    pub job: Option<JobId>,
    pub tool: Option<Arc<ToolCall>>,
    pub status: CallStatus,
    pub progress: Option<CallProgress>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentView {
    pub sequence: Seq,
    pub constraints: String,
    pub inputs: Vec<InputView>,
    pub jobs: Vec<JobView>,
    pub calls: Vec<CallView>,
    pub records: Vec<Arc<crate::Record>>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AdmissionError {
    #[error("the agent is at input capacity")]
    Busy,
    #[error("the input ID already belongs to different content")]
    ConflictingInput,
    #[error("the referenced input is not waiting for a reply")]
    InvalidReply,
}

/// Cancellation and best-effort progress for one external call.
#[derive(Clone)]
pub struct CallContext {
    id: CallId,
    progress: mpsc::Sender<(CallId, CallProgress)>,
    cancellation: watch::Receiver<bool>,
}

impl CallContext {
    pub(crate) fn new(
        id: CallId,
        progress: mpsc::Sender<(CallId, CallProgress)>,
        cancellation: watch::Receiver<bool>,
    ) -> Self {
        Self {
            id,
            progress,
            cancellation,
        }
    }

    pub fn report_progress(&self, progress: CallProgress) -> bool {
        self.progress.try_send((self.id, progress)).is_ok()
    }

    pub(crate) fn id(&self) -> CallId {
        self.id
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

/// One invocation per method; implementations keep no private agent loop.
pub trait ModelPort: Send + Sync + 'static {
    fn coordinate(
        &self,
        input: CoordinateInput,
        context: CallContext,
    ) -> PortFuture<Result<KernelDecision, CallError>>;

    fn work(
        &self,
        input: WorkInput,
        context: CallContext,
    ) -> PortFuture<Result<WorkProposal, CallError>>;

    fn compact(
        &self,
        input: CompactInput,
        context: CallContext,
    ) -> PortFuture<Result<CheckpointDraft, CallError>>;
}

pub trait ToolPort: Send + Sync + 'static {
    fn specification(&self) -> ToolSpec;
    fn run(&self, arguments: Value, context: CallContext) -> PortFuture<ToolOutcome>;
}

#[derive(Clone, Debug)]
pub(crate) enum Call {
    Coordinate(CoordinateInput),
    Work(WorkInput),
    Compact(CompactInput),
    Tool(Arc<ToolCall>),
}

#[derive(Clone, Debug)]
pub(crate) enum Event {
    CoordinateFinished {
        call: CallId,
        result: Result<KernelDecision, CallError>,
    },
    WorkFinished {
        call: CallId,
        result: Result<WorkProposal, CallError>,
    },
    CompactFinished {
        call: CallId,
        result: Result<CheckpointDraft, CallError>,
    },
    ToolFinished {
        call: CallId,
        result: ToolOutcome,
    },
    Progress {
        call: CallId,
        progress: CallProgress,
    },
    WriteResolved {
        call: CallId,
        result: ToolOutcome,
    },
    Pause(JobId),
    Resume(JobId),
    Cancel(JobId),
    Retry(InputId),
    Stop,
    Tick,
}

#[derive(Clone, Debug)]
pub(crate) enum Effect {
    Start {
        id: CallId,
        call: Box<Call>,
        timeout: std::time::Duration,
    },
    Cancel(CallId),
    Notify(Arc<crate::Record>),
}
