use std::{fmt, path::PathBuf, sync::Arc};

use bone_core::{ExternalEffect, InputOutcome, OutcomeKind, ToolOutcome};
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
uuid_id!(AcceptanceRequestId);
uuid_id!(AcceptanceId);

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

/// A retry-safe request to create one Session.
///
/// Reusing `request_id` with the same workspace and title returns the Session
/// created by the first attempt. Reusing it with different content fails with
/// [`crate::Error::RequestConflict`]. A provisional initial title may be
/// replaced once by [`crate::Session::title_from_first_input`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSessionRequest {
    pub request_id: RequestId,
    pub workspace: WorkspaceId,
    pub title: String,
    /// Whether `title_from_first_input` may replace the initial title once.
    pub provisional: bool,
}

/// Durable, read-only facts needed to render one session in a workspace list.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session: SessionInfo,
    pub has_draft: bool,
    pub draft_bytes: u64,
    pub persisted_runtime: Option<RuntimeId>,
    pub history_through: SessionSeq,
}

/// A durable condition a frontend can route without interpreting display text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AttentionItem {
    WaitingForUser {
        session: SessionId,
        inputs: Vec<InputId>,
        runtime: RuntimeId,
        question: QuestionId,
        text: String,
    },
    UnresolvedWrite {
        session: SessionId,
        call: CallRef,
        status: UnresolvedWriteStatus,
    },
}

/// Read-only workspace state for navigation and global attention surfaces.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceOverview {
    pub workspace: WorkspaceInfo,
    pub sessions: Vec<SessionSummary>,
    pub attention: Vec<AttentionItem>,
    pub unresolved_writes: Vec<UnresolvedWriteView>,
    /// A pre-projection database still has a bounded, resumable attention
    /// backfill in progress. Current items are valid but may be incomplete.
    pub attention_projection_pending: bool,
}

/// Source-control baseline used for a workspace change query. A Git workspace
/// without a commit has no HEAD yet and therefore reports `head: None`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum WorkspaceBaseline {
    NotGit,
    Git { head: Option<String> },
}

/// One side of Git's two-column index/worktree status.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum GitFileState {
    Unchanged,
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Unmerged,
    Untracked,
    Unknown,
}

/// A workspace-relative file currently changed from Git HEAD.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceChangedFile {
    pub path: String,
    pub tracked: bool,
    pub index: GitFileState,
    pub worktree: GitFileState,
}

/// Opaque exclusive cursor for the next lexicographic changed-file page.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceChangeCursor {
    pub(crate) after: String,
    pub(crate) baseline: WorkspaceBaseline,
    pub(crate) root_identity: String,
}

/// A bounded page of current workspace changes. These are workspace facts and
/// are intentionally not attributed to a particular task or Session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceChangePage {
    pub baseline: WorkspaceBaseline,
    pub files: Vec<WorkspaceChangedFile>,
    pub next_cursor: Option<WorkspaceChangeCursor>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum WorkspaceFileSource {
    DiffAgainstHead,
    WorkingTree,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum WorkspaceFileMedia {
    Text,
    Binary,
    Missing,
}

/// Strictly byte-limited content for one changed file. `truncated` means the
/// caller must not interpret the returned text as the complete file or diff.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceFileView {
    pub baseline: WorkspaceBaseline,
    pub path: String,
    pub source: WorkspaceFileSource,
    pub media: WorkspaceFileMedia,
    pub text: Option<String>,
    pub bytes_read: u64,
    pub total_bytes: Option<u64>,
    pub truncated: bool,
}

/// Opaque continuation for a stable, byte-oriented workspace file read.
///
/// The cursor binds the Git baseline, path, source and observed content
/// identity. A caller cannot use it to continue another file, and a changed
/// working tree invalidates it instead of silently joining different versions.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkspaceFileCursor {
    pub(crate) baseline: WorkspaceBaseline,
    pub(crate) path: String,
    pub(crate) source: WorkspaceFileSource,
    pub(crate) offset: u64,
    pub(crate) identity: String,
}

/// One byte-bounded page of a changed file or its diff against Git HEAD.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkspaceFilePage {
    pub baseline: WorkspaceBaseline,
    pub path: String,
    pub source: WorkspaceFileSource,
    pub media: WorkspaceFileMedia,
    pub text: Option<String>,
    pub offset: u64,
    pub bytes_read: u64,
    pub total_bytes: Option<u64>,
    pub next_cursor: Option<WorkspaceFileCursor>,
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

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct ResultRef {
    pub session: SessionId,
    pub job: JobRef,
    pub version: SessionSeq,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResultSummary {
    pub result: ResultRef,
    pub outcome: OutcomeKind,
    pub summary: String,
    pub remaining: Vec<String>,
}

/// Stable address of one session-scoped Core record explicitly cited by a
/// result. Core sequence numbers remain durable across runtime replacements.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub session: SessionId,
    pub record: u64,
}

/// A result as a durable product artifact, including only the number of
/// explicitly cited sources. Source bodies are read separately and bounded.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResultArtifact {
    pub result: ResultRef,
    pub outcome: OutcomeKind,
    pub summary: String,
    pub remaining: Vec<String>,
    pub evidence_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum EvidenceSourceKind {
    ToolResult,
    PublishedReport,
    Reply,
}

/// Whether a cited source has a public product representation. Private Core
/// records deliberately carry no title or body through this API.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum EvidenceAvailability {
    Available {
        kind: EvidenceSourceKind,
        title: String,
    },
    Private,
    Missing,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EvidenceSummary {
    pub source: EvidenceRef,
    pub availability: EvidenceAvailability,
}

/// Opaque result-bound cursor for the next explicit evidence reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct EvidenceCursor {
    result: ResultRef,
    offset: usize,
}

impl EvidenceCursor {
    pub(crate) const fn new(result: ResultRef, offset: usize) -> Self {
        Self { result, offset }
    }

    pub const fn result(self) -> ResultRef {
        self.result
    }

    pub const fn offset(self) -> usize {
        self.offset
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EvidencePage {
    pub result: ResultRef,
    pub items: Vec<EvidenceSummary>,
    pub next_cursor: Option<EvidenceCursor>,
    /// Legacy source projection is still advancing in bounded windows. A
    /// `Missing` item is not authoritative until this becomes false.
    pub projection_pending: bool,
}

/// One byte-bounded page of a source body. Offsets are byte offsets into the
/// UTF-8 body and returned boundaries are always valid character boundaries.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EvidenceSourcePage {
    pub source: EvidenceRef,
    pub availability: EvidenceAvailability,
    pub text: Option<String>,
    pub offset: u64,
    pub next_offset: Option<u64>,
    pub total_bytes: Option<u64>,
    pub projection_pending: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ResultPage {
    pub items: Vec<ResultSummary>,
    pub older_cursor: Option<HistoryCursor>,
    pub snapshot_through: SessionSeq,
    /// Older results from a pre-projection database may still be discovered by
    /// subsequent bounded refreshes. Frontends must not present an empty page
    /// as authoritative while this is true.
    pub projection_pending: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AcceptanceDecision {
    Accepted,
    PartiallyAccepted,
    AcceptedWithRisk,
    Rejected,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptanceSubmission {
    pub request_id: AcceptanceRequestId,
    pub result: ResultRef,
    pub decision: AcceptanceDecision,
    pub reason: String,
    pub rework: Option<SubmitInput>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AcceptanceRecord {
    pub id: AcceptanceId,
    pub request_id: AcceptanceRequestId,
    pub result: ResultRef,
    pub decision: AcceptanceDecision,
    pub reason: String,
    pub saved_at: SessionSeq,
    pub rework: Option<SubmissionReceipt>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AcceptanceReceipt {
    pub id: AcceptanceId,
    pub saved_at: SessionSeq,
    pub rework: Option<SubmissionReceipt>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct AcceptanceCursor {
    result: ResultRef,
    before: SessionSeq,
}

impl AcceptanceCursor {
    pub(crate) const fn new(result: ResultRef, before: SessionSeq) -> Self {
        Self { result, before }
    }

    pub const fn result(self) -> ResultRef {
        self.result
    }

    pub const fn before(self) -> SessionSeq {
        self.before
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AcceptancePage {
    pub items: Vec<AcceptanceRecord>,
    pub older_cursor: Option<AcceptanceCursor>,
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

/// Opaque cursor for reading older history from a stable journal snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct HistoryCursor {
    session: SessionId,
    before: SessionSeq,
    through: SessionSeq,
}

impl HistoryCursor {
    pub(crate) const fn new(session: SessionId, before: SessionSeq, through: SessionSeq) -> Self {
        Self {
            session,
            before,
            through,
        }
    }

    pub const fn session(self) -> SessionId {
        self.session
    }

    pub const fn before(self) -> SessionSeq {
        self.before
    }

    pub const fn snapshot_through(self) -> SessionSeq {
        self.through
    }
}

/// One ascending page from the most recent end of durable session history.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RecentHistoryPage {
    pub items: Vec<HistoryEntry>,
    pub older_cursor: Option<HistoryCursor>,
    pub snapshot_through: SessionSeq,
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
    RuntimeReconfigured {
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
    AcceptanceRecorded {
        acceptance: AcceptanceId,
        result: ResultRef,
        decision: AcceptanceDecision,
        reason: String,
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

/// A durable or live condition that prevents releasing a Session actor and
/// its exclusive writer lease without changing product behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SessionRetentionReason {
    ReleaseInProgress,
    Starting,
    ActiveWork,
    PersistenceInFlight,
    UnresolvedWrites,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SessionReleaseStatus {
    Released,
    NotOpen,
    Retained(SessionRetentionReason),
}

/// The authoritative result of one App-managed release attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionReleaseReceipt {
    pub session: SessionId,
    pub status: SessionReleaseStatus,
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

    /// Resolve BONE's platform user-data location inside the App boundary.
    /// `BONE_DATA_DIR` is an explicit deployment override.
    pub fn platform_default() -> crate::Result<Self> {
        if let Some(path) = std::env::var_os("BONE_DATA_DIR") {
            return Ok(Self::new(path));
        }
        #[cfg(target_os = "windows")]
        let path = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .map(|root| root.join("BONE"));
        #[cfg(target_os = "macos")]
        let path = std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|root| root.join("Library/Application Support/BONE"));
        #[cfg(all(unix, not(target_os = "macos")))]
        let path = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .map(|root| root.join("bone"))
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|root| root.join(".local/share/bone"))
            });
        path.map(Self::new).ok_or_else(|| {
            crate::Error::InvalidState(
                "cannot determine platform data directory; set BONE_DATA_DIR".into(),
            )
        })
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
