use std::{collections::VecDeque, sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};

use crate::{CallId, InputId, JobId, MonoTime, Seq, ToolCall};

/// The contract for one independently deliverable piece of work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobSpec {
    pub goal: String,
    pub scope: String,
    pub done_when: String,
}

impl JobSpec {
    pub fn new(
        goal: impl Into<String>,
        scope: impl Into<String>,
        done_when: impl Into<String>,
    ) -> Self {
        Self {
            goal: goal.into(),
            scope: scope.into(),
            done_when: done_when.into(),
        }
    }
}

/// Model-supplied contents of a new job. Ownership is always assigned by code.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assignment {
    pub spec: JobSpec,
    pub inputs: Vec<InputId>,
    pub evidence: Vec<Seq>,
    pub seed: Option<JobId>,
}

impl Assignment {
    pub fn new(spec: JobSpec) -> Self {
        Self {
            spec,
            inputs: Vec::new(),
            evidence: Vec::new(),
            seed: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Owner {
    User,
    Job(JobId),
    Routing(Seq),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportDraft {
    pub summary: String,
    pub evidence: Vec<Seq>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Completion {
    pub summary: String,
    pub evidence: Vec<Seq>,
    pub remaining: Vec<String>,
}

impl Completion {
    pub fn new(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            evidence: Vec::new(),
            remaining: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutcomeKind {
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobOutcome {
    pub kind: OutcomeKind,
    pub completion: Completion,
    pub revision: u64,
    pub as_of: Seq,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InquiryAnswer {
    pub inquiry: Seq,
    pub response: InquiryResponse,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InquiryResponse {
    Answer(ReportDraft),
    NeedsWork(String),
    Unavailable(String),
}

/// One model turn may advance a job by exactly one step.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkProposal {
    pub note: Option<String>,
    pub report: Option<ReportDraft>,
    pub answers: Vec<InquiryAnswer>,
    pub step: WorkStep,
}

impl WorkProposal {
    pub fn new(step: WorkStep) -> Self {
        Self {
            note: None,
            report: None,
            answers: Vec::new(),
            step,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum WorkStep {
    Continue,
    Tool(ToolCall),
    Delegate(Vec<Assignment>),
    Wait(Await),
    AskUser(String),
    Inquire {
        job: JobId,
        question: String,
    },
    ControlOwned {
        job: JobId,
        action: OwnedAction,
    },
    UpdateConstraints {
        source: InputId,
        expected_revision: u64,
        constraints: String,
    },
    Read(ReadQuery),
    PublishResult(ReportDraft),
    Reply(String),
    Finish(Completion),
    Fail(Completion),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Await {
    Tool(CallId),
    After(Duration),
    Job(JobId),
    Result { job: JobId, after: Seq },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReadQuery {
    Jobs {
        parent: Option<JobId>,
        after: Option<JobId>,
    },
    Job(JobId),
    Record {
        id: Seq,
        offset: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RouteTarget {
    Existing(JobId),
    New,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteDelivery {
    pub inputs: Vec<InputId>,
    pub target: RouteTarget,
    pub handoff: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum KernelDecision {
    Assign(Vec<RouteDelivery>),
    Read(ReadQuery),
    Clarify(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OwnedAction {
    Pause,
    Resume,
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobStatus {
    Ready,
    Running,
    Waiting(WaitView),
    Paused,
    Finished(Arc<JobOutcome>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WaitView {
    Tool(CallId),
    Until(MonoTimeView),
    User {
        question: Seq,
    },
    Job {
        job: JobId,
        revision: u64,
    },
    Result {
        job: JobId,
        revision: u64,
        after: Seq,
    },
    Inquiry(Seq),
    Coordination(Seq),
    Commit,
}

/// Serializable form of monotonic elapsed time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonoTimeView {
    pub seconds: u64,
    pub nanos: u32,
}

impl From<MonoTime> for MonoTimeView {
    fn from(value: MonoTime) -> Self {
        Self {
            seconds: value.0.as_secs(),
            nanos: value.0.subsec_nanos(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobView {
    pub id: JobId,
    pub spec: JobSpec,
    pub owner: Owner,
    pub revision: u64,
    pub status: JobStatus,
    pub inputs: Vec<InputId>,
    pub report: Option<Seq>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Job {
    pub spec: JobSpec,
    pub inputs: Vec<InputId>,
    pub owner: Owner,
    pub revision: u64,
    pub local_paused: bool,
    pub state: JobState,
    pub active_call: Option<CallId>,
    pub context: JobContext,
    pub report: Option<Seq>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum JobState {
    Ready,
    Waiting(WaitState),
    Finished(Arc<JobOutcome>),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum WaitState {
    Tool(CallId),
    Until(MonoTime),
    User {
        question: Seq,
    },
    Job {
        job: JobId,
        revision: u64,
    },
    Result {
        job: JobId,
        revision: u64,
        after: Seq,
    },
    Inquiry(Seq),
    #[allow(dead_code)]
    Coordination(Seq),
    Commit(PendingStep),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct PendingStep {
    pub call: CallId,
    pub step: WorkStep,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct JobContext {
    pub records: VecDeque<Seq>,
    /// Greatest local record sequence supplied to an accepted worker turn.
    /// Paged RecordView bodies can still have unread bytes after this sequence.
    pub read_through: Seq,
    pub checkpoint: Option<Arc<crate::Checkpoint>>,
}

impl WaitState {
    pub fn view(&self) -> WaitView {
        match self {
            Self::Tool(call) => WaitView::Tool(*call),
            Self::Until(time) => WaitView::Until((*time).into()),
            Self::User { question } => WaitView::User {
                question: *question,
            },
            Self::Job { job, revision } => WaitView::Job {
                job: *job,
                revision: *revision,
            },
            Self::Result {
                job,
                revision,
                after,
            } => WaitView::Result {
                job: *job,
                revision: *revision,
                after: *after,
            },
            Self::Inquiry(id) => WaitView::Inquiry(*id),
            Self::Coordination(id) => WaitView::Coordination(*id),
            Self::Commit(_) => WaitView::Commit,
        }
    }
}
