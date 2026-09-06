use std::{sync::Arc, time::Duration};

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Notify, watch};

use crate::{Event, JobId, MessageId, Notice, WakeId};

/// One completed Kernel::step: its input, appended records, and requested effects.
/// Effects describe instructions issued, not proof of execution or external success.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepEvent {
    /// Strictly increasing within this runtime, including steps with no changes.
    pub sequence: u64,
    /// Monotonic runtime age when the event entered the kernel.
    pub elapsed: Duration,
    pub event: Event,
    pub records: Vec<RecordEntry>,
    pub effects: Vec<EffectSummary>,
}

/// Start refers to the input's record position instead of copying its full history.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum EffectSummary {
    Start {
        id: JobId,
        request: JobRequest,
        record_cursor: u64,
        revision: u64,
        generation: u64,
        timeout: Option<Duration>,
    },
    RequestCancel {
        id: JobId,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub text: String,
}

/// An owned snapshot; later events never modify a running call's input.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub record_cursor: u64,
    pub revision: u64,
    pub generation: u64,
    pub requirement: Option<String>,
    pub autonomous: bool,
    pub work: Option<JobId>,
    pub review: Option<JobId>,
    pub candidate: Option<JobId>,
    pub pending_messages: Vec<Message>,
    pub jobs: Vec<JobSnapshot>,
    pub record: Vec<RecordEntry>,
    pub tools: Vec<ToolSpec>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecordEntry {
    pub cursor: u64,
    pub kind: RecordKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RecordKind {
    UserMessage(Message),
    InputsHandled {
        job: JobId,
        messages: Vec<MessageId>,
    },
    RequirementUpdated {
        job: JobId,
        text: String,
    },
    /// The entire work proposal passed kernel checks before taking effect.
    PlanAccepted {
        job: JobId,
    },
    PlanDiscarded {
        job: JobId,
        reason: String,
    },
    WorkHeld {
        job: JobId,
        messages: Vec<MessageId>,
    },
    /// Classification alone does not mean the solver handled these messages.
    InputReviewed {
        job: JobId,
        messages: Vec<MessageId>,
        disposition: InputDisposition,
        note: String,
    },
    CancellationRequested {
        job: JobId,
    },
    Reminder {
        id: WakeId,
        job: Option<JobId>,
    },
    Notice(Notice),
}

/// A small job description, deliberately without a nested snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum JobRequest {
    Work { messages: Vec<MessageId> },
    ReviewInput { messages: Vec<MessageId> },
    Tool(ToolCall),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JobSnapshot {
    pub id: JobId,
    pub request: JobRequest,
    pub record_cursor: u64,
    pub revision: u64,
    pub generation: u64,
    /// Comes from the registered implementation, never from model output.
    pub external_write: bool,
    pub state: JobState,
    pub progress: Option<JobProgress>,
}

impl JobSnapshot {
    pub fn is_running(&self) -> bool {
        !matches!(self.state, JobState::Finished(_))
    }

    /// A returned future can leave an external write unresolved.
    pub fn is_unresolved(&self) -> bool {
        match &self.state {
            JobState::Finished(outcome) => outcome.external_effect == ExternalEffect::Unknown,
            _ => true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum JobState {
    Running,
    CancelRequested,
    Finished(JobOutcome),
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
    /// Solve the task and propose its next step using a fixed input batch.
    Work { messages: Vec<Message> },
    /// Interpret an interruption while a work decision is still outstanding.
    ReviewInput { messages: Vec<Message> },
}

/// Only a current work request can submit this proposal. All fields except
/// the material in note require a valid request and a fresh decision basis.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkResult {
    pub note: String,
    pub reply: Option<String>,
    pub requirement: Option<String>,
    pub autonomy: Autonomy,
    pub operation: Option<Operation>,
    pub next: Next,
}

/// A limited semantic judgment about a fixed batch, never a work plan.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputReview {
    pub disposition: InputDisposition,
    pub reply: Option<String>,
    pub note: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputDisposition {
    Keep,
    /// Ambiguous or substantive input belongs with the solver.
    #[default]
    Reconsider,
    Pause,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Autonomy {
    #[default]
    Keep,
    Run,
    Pause,
}

/// One operation per work result. Existing tools run independently.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Operation {
    Tool(ToolCall),
    Cancel(JobId),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Next {
    Continue,
    Wait { reconsider_after: Option<Duration> },
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
pub struct JobProgress {
    pub message: String,
    pub percent: Option<u8>,
}

/// Every invocation returns the same outcome envelope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JobOutcome {
    pub result: Result<JobOutput, JobError>,
    pub external_effect: ExternalEffect,
}

impl JobOutcome {
    pub fn work(result: WorkResult) -> Self {
        Self {
            result: Ok(JobOutput::Work(result)),
            external_effect: ExternalEffect::None,
        }
    }

    pub fn review(review: InputReview) -> Self {
        Self {
            result: Ok(JobOutput::InputReview(review)),
            external_effect: ExternalEffect::None,
        }
    }

    pub fn artifact(value: impl Into<Value>) -> Self {
        Self {
            result: Ok(JobOutput::Artifact(value.into())),
            external_effect: ExternalEffect::None,
        }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            result: Err(JobError::new(message)),
            external_effect: ExternalEffect::None,
        }
    }

    pub fn unknown(message: impl Into<String>) -> Self {
        Self {
            result: Err(JobError::new(message)),
            external_effect: ExternalEffect::Unknown,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum JobOutput {
    Work(WorkResult),
    InputReview(InputReview),
    Artifact(Value),
}

/// External effects are independent of local success or failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExternalEffect {
    /// Read-only, or a write confirmed not to have happened.
    None,
    Applied,
    /// The write might have happened; it continues to hold the write gate.
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobErrorKind {
    Failed,
    Cancelled,
    TimedOut,
    Panicked,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct JobError {
    pub kind: JobErrorKind,
    pub message: String,
}

impl JobError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            kind: JobErrorKind::Failed,
            message: message.into(),
        }
    }
}

/// Progress and cooperative cancellation, shared by models and tools.
/// Jobs cannot access the kernel or launch subsequent operations.
#[derive(Clone)]
pub struct JobContext {
    progress: watch::Sender<Option<JobProgress>>,
    progress_ready: Arc<Notify>,
    cancellation: watch::Receiver<bool>,
}

impl JobContext {
    pub(crate) fn new(
        progress: watch::Sender<Option<JobProgress>>,
        progress_ready: Arc<Notify>,
        cancellation: watch::Receiver<bool>,
    ) -> Self {
        Self {
            progress,
            progress_ready,
            cancellation,
        }
    }

    pub fn report_progress(&self, progress: JobProgress) -> bool {
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

/// Exactly one provider call per invocation, with no private agent loop.
/// Work and input review may run concurrently; no lock may span infer.
/// Futures must yield while waiting and release local resources when dropped.
pub trait ModelPort: Send + Sync + 'static {
    fn infer(&self, input: ModelInput, context: JobContext) -> BoxFuture<'static, JobOutcome>;
}

pub trait ToolPort: Send + Sync + 'static {
    fn specification(&self) -> ToolSpec;
    fn run(&self, arguments: Value, context: JobContext) -> BoxFuture<'static, JobOutcome>;
}
