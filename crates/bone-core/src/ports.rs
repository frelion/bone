use std::{future::Future, pin::Pin, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, watch};

use crate::{
    CallId, CheckpointDraft, CompactInput, ConversationInput, ConversationStep, InputId, JobId,
    JobView, Seq, WorkInput, WorkProposal,
};

pub type PortFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub id: InputId,
    pub text: String,
    pub reply_to: Option<InputId>,
    /// The clarification record this input answers.
    ///
    /// Hosts that expose durable question IDs should set this field with
    /// [`Input::answering`]. [`Input::replying_to`] remains available when the
    /// caller only needs the current question for an input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_question: Option<Seq>,
}

impl Input {
    pub fn new(id: InputId, text: impl Into<String>) -> Self {
        Self {
            id,
            text: text.into(),
            reply_to: None,
            expected_question: None,
        }
    }

    pub fn replying_to(mut self, input: InputId) -> Self {
        self.reply_to = Some(input);
        self.expected_question = None;
        self
    }

    pub fn answering(mut self, input: InputId, expected_question: Seq) -> Self {
        self.reply_to = Some(input);
        self.expected_question = Some(expected_question);
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
    Thinking,
    WaitingForUser { question: String, question_seq: Seq },
    ConversationFailed { message: String },
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

/// A tool name is a stable capability identity supplied by the trusted host.
/// Reconfiguration may change availability, but must not repurpose an existing name
/// or change its effect. Job permissions constrain model calls, not host code.
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
    Converse,
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

impl Default for AgentView {
    fn default() -> Self {
        Self {
            sequence: Seq::ZERO,
            constraints: String::new(),
            inputs: Vec::new(),
            jobs: Vec::new(),
            calls: Vec::new(),
            records: Vec::new(),
        }
    }
}

/// Whether a host control changed agent state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlOutcome {
    Applied,
    Unchanged,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AdmissionError {
    #[error("the agent is at input capacity")]
    Busy,
    #[error("the input ID already belongs to different content")]
    ConflictingInput,
    #[error("the referenced input is not waiting for a reply")]
    InvalidReply,
    #[error("the referenced question is no longer current")]
    StaleReply,
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

    /// Identifies this invocation within its agent runtime.
    ///
    /// IDs are unique only within one runtime. External tools using this ID for
    /// idempotency must combine it with a host-provided runtime or session key.
    pub fn id(&self) -> CallId {
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
    fn converse(
        &self,
        input: ConversationInput,
        context: CallContext,
    ) -> PortFuture<Result<ConversationStep, CallError>>;

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
    Converse(ConversationInput),
    Work(WorkInput),
    Compact(CompactInput),
    Tool(Arc<ToolCall>),
}

#[derive(Clone, Debug)]
pub(crate) enum Event {
    ConverseFinished {
        call: CallId,
        result: Result<ConversationStep, CallError>,
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
