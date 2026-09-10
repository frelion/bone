use std::sync::Arc;

use bone_app::{
    HistoryCursor, HistoryPage, RecentHistoryPage, RequestId, SessionId, SessionInfo, SessionView,
    SubmissionReceipt, SubmitInput, WorkspaceId,
};

use std::collections::BTreeMap;

use super::model::{Focus, SessionStatus};
use crate::layout::TranscriptMetrics;

#[derive(Clone, Debug)]
pub enum Action {
    Noop,
    Focus(Focus),
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    SelectPrevious,
    SelectNext,
    SelectSession(SessionId),
    SelectSlashPrevious,
    SelectSlashNext,
    CompleteSlash,
    ExecuteSlash(usize),
    Input(char),
    Paste(String),
    Backspace,
    Delete,
    CursorLeft,
    CursorRight,
    CursorHome,
    CursorEnd,
    InsertNewline,
    Submit,
    Escape,
    ScrollUp {
        amount: usize,
        metrics: Option<Arc<TranscriptMetrics>>,
    },
    ScrollDown(usize),
    Stop,
    Quit,
    Terminate,
}

#[derive(Clone, Debug)]
pub enum UiEvent {
    Action(Action),
    WorkspaceOpened {
        id: WorkspaceId,
        label: String,
        sessions: Vec<SessionInfo>,
        last_active: Option<SessionId>,
        model_label: Option<String>,
        statuses: BTreeMap<SessionId, SessionStatus>,
    },
    SessionOpened {
        session: SessionId,
        generation: u64,
        snapshot: Arc<SessionView>,
        history: RecentHistoryPage,
    },
    SessionChanged {
        session: SessionId,
        generation: u64,
        snapshot: Arc<SessionView>,
    },
    HistoryLoaded {
        session: SessionId,
        generation: u64,
        page: HistoryPage,
    },
    OlderHistoryLoaded {
        session: SessionId,
        generation: u64,
        page: RecentHistoryPage,
    },
    RecentHistoryReloaded {
        session: SessionId,
        generation: u64,
        page: RecentHistoryPage,
    },
    RefreshOverviewRequested,
    PersistDraftsRequested,
    OverviewLoaded {
        generation: u64,
        sessions: Vec<SessionInfo>,
        statuses: BTreeMap<SessionId, SessionStatus>,
    },
    DraftSaved {
        session: SessionId,
        generation: u64,
        revision: u64,
        text: String,
    },
    Submitted {
        session: SessionId,
        generation: u64,
        request_id: RequestId,
        receipt: SubmissionReceipt,
    },
    SubmitFailed {
        session: SessionId,
        generation: u64,
        request_id: RequestId,
        message: String,
    },
    SessionCreated {
        request_id: RequestId,
        info: SessionInfo,
    },
    SessionCreateFailed {
        request_id: RequestId,
        message: String,
    },
    SessionRenamed {
        session: SessionId,
        generation: u64,
        title: String,
    },
    SessionReleased {
        generation: u64,
        receipt: bone_app::SessionReleaseReceipt,
    },
    OperationFailed {
        kind: OperationKind,
        session: Option<SessionId>,
        generation: Option<u64>,
        message: String,
    },
    Resized,
    Tick,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationKind {
    OpenSession,
    CreateSession,
    SaveDraft,
    Submit,
    Stop,
    LoadHistory,
    LoadOlderHistory,
    ReloadRecentHistory,
    RefreshOverview,
    RenameSession,
    AutoTitle,
    ReleaseSession,
    RememberSession,
}

#[derive(Clone, Debug)]
pub enum Effect {
    OpenSession {
        session: SessionId,
        generation: u64,
    },
    CreateSession {
        request_id: RequestId,
        title: String,
        provisional: bool,
    },
    RenameSession {
        session: SessionId,
        generation: u64,
        title: String,
    },
    AutoTitle {
        session: SessionId,
        generation: u64,
        first_input: String,
    },
    SaveDraft {
        session: SessionId,
        generation: u64,
        revision: u64,
        text: String,
    },
    Submit {
        session: SessionId,
        generation: u64,
        input: SubmitInput,
    },
    Stop {
        session: SessionId,
        generation: u64,
    },
    LoadHistory {
        session: SessionId,
        generation: u64,
        after: bone_app::SessionSeq,
    },
    LoadOlderHistory {
        session: SessionId,
        generation: u64,
        cursor: HistoryCursor,
    },
    ReloadRecentHistory {
        session: SessionId,
        generation: u64,
    },
    RefreshOverview {
        generation: u64,
    },
    ReleaseSession {
        session: SessionId,
        generation: u64,
    },
    RememberSession {
        workspace: WorkspaceId,
        session: SessionId,
    },
    Shutdown,
}
