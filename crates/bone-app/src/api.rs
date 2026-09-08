use std::{fmt, path::PathBuf, sync::Arc};

use bone_agent::{ExternalEffect, InputOutcome, OutcomeKind, ToolOutcome};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::{ConfigProblem, RuntimeConfig};

macro_rules! uuid_id {
    ($name:ident) => {
        #[derive(
            Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub const fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }

            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

uuid_id!(WorkspaceId);
uuid_id!(SessionId);
uuid_id!(RequestId);
uuid_id!(RuntimeId);

#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct InputId(pub u64);

#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct SessionSeq(pub u64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct JobRef {
    pub runtime: RuntimeId,
    pub id: u64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct CallRef {
    pub runtime: RuntimeId,
    pub id: u64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct QuestionId {
    pub runtime: RuntimeId,
    pub record: u64,
    pub reply_to: InputId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitInput {
    pub request_id: RequestId,
    pub text: String,
    pub reply_to: Option<QuestionId>,
}

impl SubmitInput {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            request_id: RequestId::new(),
            text: text.into(),
            reply_to: None,
        }
    }

    pub fn answer(mut self, question: QuestionId) -> Self {
        self.reply_to = Some(question);
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SubmissionReceipt {
    pub input: InputId,
    pub saved_at: SessionSeq,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub id: WorkspaceId,
    pub root: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub workspace: WorkspaceId,
    pub title: String,
    pub archived: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RuntimeState {
    Detached,
    Starting,
    Running {
        id: RuntimeId,
        config: Box<RuntimeConfig>,
    },
    Closing {
        id: RuntimeId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum InputState {
    Queued {
        problem: Option<ConfigProblem>,
    },
    Posting {
        runtime: RuntimeId,
    },
    Accepted {
        runtime: RuntimeId,
    },
    WaitingForUser {
        runtime: RuntimeId,
        question: QuestionId,
        text: String,
    },
    RoutingFailed {
        runtime: RuntimeId,
        message: String,
    },
    Finished {
        runtime: RuntimeId,
        outcome: InputOutcome,
    },
    Rejected {
        message: String,
    },
    Cancelled,
    Interrupted {
        runtime: RuntimeId,
    },
}

impl InputState {
    pub fn terminal(&self) -> bool {
        matches!(
            self,
            Self::Finished { .. }
                | Self::Rejected { .. }
                | Self::Cancelled
                | Self::Interrupted { .. }
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InputView {
    pub id: InputId,
    pub request_id: RequestId,
    pub text: String,
    pub reply_to: Option<QuestionId>,
    pub state: InputState,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum JobState {
    Ready,
    Running,
    Waiting(WaitReason),
    Paused,
    Finished {
        outcome: OutcomeKind,
        summary: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum JobOwner {
    User,
    Job(JobRef),
    Routing,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum WaitReason {
    Tool(CallRef),
    Timer,
    User,
    Job(JobRef),
    Result(JobRef),
    Inquiry,
    Coordination,
    Commit,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct JobReport {
    pub summary: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct JobView {
    pub id: JobRef,
    pub owner: JobOwner,
    pub inputs: Vec<InputId>,
    pub goal: String,
    pub scope: String,
    pub done_when: String,
    pub state: JobState,
    pub report: Option<JobReport>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ActivityKind {
    Coordinate,
    Work,
    Compact,
    Tool { name: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActivityView {
    pub call: CallRef,
    pub job: Option<JobRef>,
    pub kind: ActivityKind,
    pub progress: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UnresolvedWriteView {
    pub workspace: WorkspaceId,
    pub session: SessionId,
    pub call: CallRef,
    pub job: Option<JobRef>,
    pub status: UnresolvedWriteStatus,
    pub tool: String,
    pub arguments: serde_json::Value,
    pub outcome: Option<ToolOutcome>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum UnresolvedWriteStatus {
    Pending,
    Finished,
}

/// A runtime problem that a frontend can handle without parsing display text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AppProblem {
    Configuration(ConfigProblem),
    LoginRequired(crate::ProfileId),
    ProfileBusy(crate::ProfileId),
    Provider(String),
    Storage(String),
    Tools(String),
    Agent(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteResolution {
    pub external_effect: ExternalEffect,
    pub evidence: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionView {
    pub session: SessionInfo,
    pub runtime: RuntimeState,
    pub draft: String,
    pub inputs: Vec<InputView>,
    pub jobs: Vec<JobView>,
    pub activity: Vec<ActivityView>,
    pub history_through: SessionSeq,
    pub problem: Option<AppProblem>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub sequence: SessionSeq,
    pub occurred_at: i64,
    pub event: SessionEvent,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryPage {
    pub items: Vec<HistoryEntry>,
    pub next_cursor: SessionSeq,
    pub has_more: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SessionEvent {
    InputSubmitted {
        input: InputId,
        request_id: RequestId,
        text: String,
        reply_to: Option<QuestionId>,
    },
    InputAccepted {
        input: InputId,
        runtime: RuntimeId,
    },
    InputRejected {
        input: InputId,
        message: String,
    },
    InputCancelled {
        input: InputId,
    },
    RuntimeStarted {
        runtime: RuntimeId,
        config: Box<RuntimeConfig>,
    },
    RuntimeClosed {
        runtime: RuntimeId,
    },
    Reply {
        job: JobRef,
        inputs: Vec<InputId>,
        text: String,
    },
    QuestionAsked {
        question: QuestionId,
        inputs: Vec<InputId>,
        text: String,
    },
    RoutingFailed {
        runtime: RuntimeId,
        inputs: Vec<InputId>,
        message: String,
    },
    JobFinished {
        job: JobRef,
        outcome: OutcomeKind,
        summary: String,
        remaining: Vec<String>,
    },
    InputFinished {
        runtime: RuntimeId,
        input: InputId,
        outcome: InputOutcome,
    },
    ToolFinished {
        call: CallRef,
        job: JobRef,
        tool: String,
        outcome: ToolOutcome,
    },
    Interrupted {
        runtime: RuntimeId,
        inputs: Vec<InputId>,
    },
    WriteResolved {
        call: CallRef,
        external_effect: ExternalEffect,
        evidence: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobControl {
    Pause,
    Resume,
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandReceipt {
    Applied,
    Unchanged,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CloseReport {
    pub unresolved_writes: Vec<UnresolvedWriteView>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AppShutdownReport {
    pub unresolved_writes: Vec<UnresolvedWriteView>,
}

#[derive(Clone, Debug)]
pub struct AppOptions {
    pub data_dir: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoginState {
    Connecting,
    DeviceCode {
        verification_uri: String,
        user_code: String,
    },
    Succeeded,
    Failed {
        message: String,
    },
    Cancelled,
}

impl AppOptions {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }
}

pub(crate) fn default_view(info: SessionInfo) -> Arc<SessionView> {
    Arc::new(SessionView {
        session: info,
        runtime: RuntimeState::Detached,
        draft: String::new(),
        inputs: Vec::new(),
        jobs: Vec::new(),
        activity: Vec::new(),
        history_through: SessionSeq(0),
        problem: None,
    })
}
