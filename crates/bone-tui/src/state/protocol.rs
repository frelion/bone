use std::sync::Arc;

use bone_app::{
    HistoryCursor, HistoryPage, InputId, QuestionId, RecentHistoryPage, RequestId, SessionId,
    SessionInfo, SessionView, SubmitInput,
};

use super::model::{Focus, SessionNavRow};
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditorTarget {
    Composer,
    SessionTitle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CursorMove {
    Left,
    Right,
    Up { width: u16 },
    Down { width: u16 },
    WordLeft,
    WordRight,
    LineStart,
    LineEnd,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EditCommand {
    Insert { text: String, typing: bool },
    Replace { text: String },
    DeleteBefore,
    DeleteAfter,
    Move { cursor: CursorMove, select: bool },
    Point { byte: usize, extend: bool },
    Clear,
    Undo,
    Redo,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    BeginPaneResize(crate::layout::PaneDivider),
    DragPane {
        widths: crate::layout::PaneWidths,
        finish: bool,
    },
    EndPaneResize,
    SetupText(super::SecretText),
    SetupBackspace,
    SetupClear,
    NextField,
    PreviousField,
    SelectField(super::SetupField),
    SaveConnection,
    ChooseConnectionKind(usize),
    Edit {
        target: EditorTarget,
        command: EditCommand,
    },
    CommitTitle,
    CancelTitle,
    StartSlashCommand,
    OpenModels,
    SelectObject(usize),
    OpenHistory(bone_app::SessionSeq),
    OpenJob(bone_app::JobRef),
    PanelPrevious,
    PanelNext,
    ActivatePanel,
    SelectModel(usize),
    ScrollPanel {
        amount: isize,
        max: usize,
    },

    Focus(Focus),
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    SelectPrevious,
    SelectNext,
    SelectSession(SessionId),
    OpenCandidate,
    ScrollSessions {
        start: usize,
    },
    SelectSlashPrevious,
    SelectSlashNext,
    CompleteSlash,
    ExecuteCommand(super::CommandKind),
    Submit,
    ClickSubmit,
    AnswerQuestion(QuestionId),
    LeaveAnswer,
    ConvertAnswer,
    RestoreInput(InputId),
    RetryInput(InputId),
    RetrySubmission,
    Escape,
    ScrollUp(usize),
    ScrollDown(usize),
    Stop,
    Quit,
    Terminate,
}

#[derive(Clone, Debug)]
pub enum UiEvent {
    ConnectionSaved {
        request: u64,
        session: Option<SessionId>,
        error: Option<String>,
        notice: Option<String>,
    },
    LoginChanged {
        request: u64,
        state: bone_app::LoginState,
    },
    ModelLabelLoaded {
        session: Option<SessionId>,
        request: u64,
        label: Option<String>,
        facts: Option<super::ModelFacts>,
    },
    ModelsFailed {
        session: Option<SessionId>,
        request: u64,
        error: String,
    },
    ModelsLoaded {
        session: Option<SessionId>,
        request: u64,
        choices: Vec<super::ModelChoice>,
        profiles: Vec<bone_app::Profile>,
    },
    ModelApplied {
        session: Option<SessionId>,
        request: u64,
        label: Option<String>,
        facts: Option<super::ModelFacts>,
        error: Option<String>,
    },

    Action(Action),
    WorkspaceOpened {
        label: String,
        rows: Vec<SessionNavRow>,
        last_active: Option<SessionId>,
        model_label: Option<String>,
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
        rows: Vec<SessionNavRow>,
    },
    DraftSaved {
        session: SessionId,
        generation: u64,
        revision: u64,
    },
    Submitted {
        session: SessionId,
        request_id: RequestId,
    },
    SubmitFailed {
        session: SessionId,
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
        request: u64,
        title: String,
    },
    SessionRenameFailed {
        session: SessionId,
        request: u64,
        message: String,
    },
    SessionAutoTitled {
        session: SessionId,
        request: u64,
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
    CaretBlink,
    Resized {
        width: u16,
        height: u16,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationKind {
    OpenSession,
    SaveDraft,
    RetryInput,
    Stop,
    LoadHistory,
    LoadOlderHistory,
    ReloadRecentHistory,
    RefreshOverview,
    AutoTitle,
    ReleaseSession,
    RememberSession,
}

#[derive(Clone, Debug)]
pub enum Effect {
    SaveConnection {
        request: u64,
        session: Option<SessionId>,
        profile: bone_app::Profile,
        key: Option<super::SecretText>,
        selection: Option<bone_app::ModelSelection>,
    },
    Login {
        profile: bone_app::ProfileId,
        request: u64,
    },
    LoadModelLabel {
        session: Option<SessionId>,
        request: u64,
    },
    CancelLogin,
    LoadModels {
        session: Option<SessionId>,
        request: u64,
    },
    SetModel {
        session: Option<SessionId>,
        request: u64,
        selection: bone_app::ModelSelection,
    },
    SetNamedModel {
        session: Option<SessionId>,
        request: u64,
        profile: String,
        model: String,
    },

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
        request: u64,
        title: String,
    },
    AutoTitle {
        session: SessionId,
        generation: u64,
        request: u64,
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
        input: SubmitInput,
    },
    RetryInput {
        session: SessionId,
        generation: u64,
        input: InputId,
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
        session: SessionId,
    },
    Shutdown,
}
