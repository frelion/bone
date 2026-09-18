use std::sync::Arc;

use bone_app::{
    HistoryCursor, HistoryPage, InputId, QuestionId, RecentHistoryPage, RequestId, SessionId,
    SessionInfo, SessionView, SubmitInput,
};

use crate::editor::EditCommand;

use super::model::SessionNavRow;
#[cfg(test)]
use super::model::WorkspaceTarget;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditorTarget {
    Composer,
    SessionTitle,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    PressPointer {
        target: Option<Box<crate::layout::ClickTarget>>,
        position: (u16, u16),
        text: Option<(crate::ui::selection::TextPoint, Arc<str>)>,
    },
    DragTextSelection {
        point: Option<crate::ui::selection::TextPoint>,
    },
    ReleasePointer {
        click: Option<Box<Action>>,
        point: Option<crate::ui::selection::TextPoint>,
        text: Option<String>,
    },
    FinishEditorSelection {
        target: EditorTarget,
        byte: usize,
    },
    BeginPaneResize(crate::layout::PaneDivider),
    DragPane {
        widths: crate::layout::PaneWidths,
        finish: bool,
    },
    EndPointerCapture,
    PointEditor {
        target: EditorTarget,
        byte: usize,
        extend: bool,
        begin: bool,
    },
    SetupText(super::SecretText),
    SetupBackspace,
    SetupClear,
    NextField,
    PreviousField,
    SaveConnection,
    ChooseConnection(usize),
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
    RetryLogin,
    ToggleOverlayKeyboard,
    CloseOverlay,
    OverlayBack,
    CloseDetails,
    SelectModel(usize),
    SelectReasoning(usize),
    PreviousTab,
    NextTab,
    SelectTab(usize),
    EditConnection,
    DeleteConnection,
    ConfirmDeleteConnection,
    ModelText(String),
    ModelBackspace,
    ModelClear,
    DeleteModel,
    ScrollOverlay {
        amount: isize,
        max: usize,
    },
    ScrollDetails {
        amount: isize,
        max: usize,
    },

    #[cfg(test)]
    SetWorkspaceTarget(WorkspaceTarget),
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
    DismissCommands,
    PrepareCommand(super::CommandKind),
    ExecuteCommand {
        kind: super::CommandKind,
        argument: String,
    },
    CommandError(String),
    Submit,
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
}

#[derive(Debug)]
pub enum UiEvent {
    ConfigOperationFinished {
        action: &'static str,
        error: Option<String>,
    },
    ConnectionSaved {
        request: u64,
        session: Option<SessionId>,
        error: Option<String>,
        key_saved: bool,
    },
    ConnectionDeleted {
        request: u64,
        error: Option<String>,
    },
    LoginChanged {
        request: u64,
        state: bone_app::LoginState,
    },
    ModelFactsLoaded {
        session: Option<SessionId>,
        request: u64,
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
        facts: Option<super::ModelFacts>,
        error: Option<String>,
    },

    Action(Action),
    PointerMoved {
        column: u16,
        row: u16,
    },
    PointerLeft,
    CancelPointerCapture,
    WorkspaceOpened {
        label: String,
        rows: Vec<SessionNavRow>,
        last_active: Option<SessionId>,
        model_facts: super::ModelFacts,
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
    SessionAutoTitleFinished {
        session: SessionId,
        request: u64,
        result: Result<Option<String>, String>,
    },
    SessionReleased {
        generation: u64,
        receipt: bone_app::SessionReleaseReceipt,
    },
    SessionOperationFailed {
        kind: SessionOperationKind,
        session: SessionId,
        generation: u64,
        message: String,
    },
    OverviewFailed {
        generation: u64,
        message: String,
    },
    RememberSessionFailed {
        session: SessionId,
        generation: u64,
        message: String,
    },
    CaretBlink,
    ActivityTick,
    Resized,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionOperationKind {
    OpenSession,
    SaveDraft,
    RetryInput,
    Stop,
    LoadHistory,
    LoadOlderHistory,
    ReloadRecentHistory,
    ReleaseSession,
}

#[derive(Debug)]
pub enum Effect {
    CopyText(String),
    ReloadConfig,
    SaveConnection {
        request: u64,
        session: Option<SessionId>,
        workspace_default: bool,
        profile: bone_app::Profile,
        key: Option<super::SecretText>,
        selection: Option<bone_app::ModelSelection>,
    },
    Login {
        profile: bone_app::ProfileId,
        request: u64,
    },
    LoadModelFacts {
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
        workspace_default: bool,
        selection: bone_app::ModelSelection,
    },
    DeleteConnection {
        request: u64,
        profile: bone_app::ProfileId,
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
        generation: u64,
    },
    Shutdown,
}
