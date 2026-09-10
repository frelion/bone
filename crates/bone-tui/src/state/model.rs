use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
};

use crate::layout::HitTarget;
use bone_app::{
    AttentionItem, HistoryCursor, HistoryEntry, HistoryPage, QuestionId, RequestId, SessionId,
    SessionInfo, SessionView, WorkspaceId,
};

pub const HISTORY_CACHE_ITEMS: usize = 512;
pub const HISTORY_CACHE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MainView {
    #[default]
    Workbench,
    Sessions,
    Attention,
    Settings,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Focus {
    Global,
    Rail,
    Timeline,
    #[default]
    Composer,
    Detail,
    Actions,
    Dialog,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DetailKind {
    Work,
    Changes,
    Context,
    Artifacts,
    Records,
    Acceptance,
    Decision,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DetailState {
    pub kind: DetailKind,
    pub title: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Dialog {
    NewSession {
        title: String,
    },
    RenameSession {
        session: SessionId,
        generation: u64,
        operation: u64,
        title: String,
        submitting: bool,
    },
    ConfirmQuit,
    Error(String),
    Acceptance {
        session: SessionId,
        generation: u64,
        request_id: bone_app::AcceptanceRequestId,
        rework_request_id: Option<RequestId>,
        result: bone_app::ResultRef,
        decision: bone_app::AcceptanceDecision,
        reason: String,
        rework: String,
        editing_rework: bool,
        submitting: bool,
    },
    ModelConfig {
        session: SessionId,
        generation: u64,
        role: ModelRole,
        profile: bone_app::ProfileId,
        model: String,
        submitting: bool,
    },
    ApiKey {
        session: SessionId,
        generation: u64,
        profile: bone_app::ProfileId,
        key: SecretText,
        submitting: bool,
    },
    ResolveWrite {
        session: SessionId,
        call: bone_app::CallRef,
        external_effect: bone_app::ExternalEffect,
        evidence: String,
        submitting: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelRole {
    Worker,
    Coordinator,
}

#[derive(Clone, Default, Eq, PartialEq)]
pub struct SecretText(String);

impl SecretText {
    pub fn push(&mut self, value: char) {
        self.0.push(value);
    }

    pub fn extend(&mut self, values: impl IntoIterator<Item = char>) {
        self.0.extend(values);
    }

    pub fn as_mut_string(&mut self) -> &mut String {
        &mut self.0
    }

    pub fn is_blank(&self) -> bool {
        self.0.trim().is_empty()
    }

    pub fn grapheme_count(&self) -> usize {
        use unicode_segmentation::UnicodeSegmentation;
        self.0.graphemes(true).count()
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl std::fmt::Debug for SecretText {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("<redacted>")
    }
}

#[derive(Clone, Debug)]
pub struct SessionUi {
    pub info: SessionInfo,
    pub generation: u64,
    pub snapshot: Option<Arc<SessionView>>,
    pub history: VecDeque<HistoryEntry>,
    pub history_bytes: usize,
    pub history_cursor: bone_app::SessionSeq,
    pub history_has_more: bool,
    pub history_loading: bool,
    pub older_cursor: Option<HistoryCursor>,
    pub older_loading: bool,
    pub recent_reloading: bool,
    pub history_window_stale: bool,
    pub newer_history_missing: bool,
    pub draft: String,
    pub draft_revision: u64,
    pub saved_draft_revision: u64,
    pub saved_draft: String,
    pub hydrated_once: bool,
    pub queued_draft_revision: u64,
    pub submitting: Option<PendingSubmission>,
    pub scroll_from_tail: usize,
    pub unread: usize,
    pub result_query: u64,
    pub result_loading: bool,
    pub result_confirmed_query: u64,
    pub results_older_loading: bool,
    pub results_window_stale: bool,
    pub result_pages: PageNavigation<HistoryCursor>,
    pub release_requested: bool,
    pub reply_to: Option<QuestionId>,
    pub saved_detail: Option<DetailState>,
    pub saved_detail_scroll: usize,
}

impl SessionUi {
    pub fn new(info: SessionInfo, generation: u64) -> Self {
        Self {
            info,
            generation,
            snapshot: None,
            history: VecDeque::new(),
            history_bytes: 0,
            history_cursor: bone_app::SessionSeq(0),
            history_has_more: true,
            history_loading: false,
            older_cursor: None,
            older_loading: false,
            recent_reloading: false,
            history_window_stale: false,
            newer_history_missing: false,
            draft: String::new(),
            draft_revision: 0,
            saved_draft_revision: 0,
            saved_draft: String::new(),
            hydrated_once: false,
            queued_draft_revision: 0,
            submitting: None,
            scroll_from_tail: 0,
            unread: 0,
            result_query: 0,
            result_loading: false,
            result_confirmed_query: 0,
            results_older_loading: false,
            results_window_stale: false,
            result_pages: PageNavigation::default(),
            release_requested: false,
            reply_to: None,
            saved_detail: None,
            saved_detail_scroll: 0,
        }
    }

    pub fn push_history(&mut self, page: HistoryPage) {
        for item in page.items {
            if self
                .history
                .back()
                .is_none_or(|old| old.sequence < item.sequence)
            {
                self.history_bytes = self
                    .history_bytes
                    .saturating_add(history_entry_bytes(&item));
                self.history.push_back(item);
            }
        }
        self.history_cursor = page.next_cursor;
        self.history_has_more = page.has_more;
    }
}

#[derive(Clone, Debug)]
pub struct PendingSubmission {
    pub request_id: RequestId,
    pub text: String,
    pub draft_revision: u64,
    pub reply_to: Option<QuestionId>,
    pub failed: bool,
    pub clear_on_receipt: bool,
}

#[derive(Clone, Debug)]
pub struct UiState {
    pub workspace: Option<(WorkspaceId, String)>,
    pub main: MainView,
    pub focus: Focus,
    pub global_selection: usize,
    pub detail_tab_selection: usize,
    pub action_selection: usize,
    /// The exact visible control currently selected inside a page/detail region.
    /// It is validated against the latest rendered hit regions before activation.
    pub focused_control: Option<HitTarget>,
    pub sessions: Vec<SessionInfo>,
    pub selected: Option<SessionId>,
    pub session_ui: BTreeMap<SessionId, SessionUi>,
    pub detail: Option<DetailState>,
    pub detail_scroll: usize,
    pub dialog: Option<Dialog>,
    pub status: Option<String>,
    pub attention: Vec<AttentionItem>,
    pub attention_projection_pending: bool,
    pub unresolved_writes: Vec<bone_app::UnresolvedWriteView>,
    pub attention_detail: Option<AttentionItem>,
    pub attention_selection: usize,
    pub write_resolution_applied: bool,
    pub settings: Option<SettingsData>,
    pub results: BTreeMap<SessionId, bone_app::ResultPage>,
    pub acceptances: BTreeMap<bone_app::ResultRef, bone_app::AcceptancePage>,
    pub acceptance_queries: BTreeMap<bone_app::ResultRef, u64>,
    pub acceptance_loading: BTreeSet<bone_app::ResultRef>,
    pub acceptance_windows_stale: BTreeSet<bone_app::ResultRef>,
    pub acceptance_pages: BTreeMap<bone_app::ResultRef, PageNavigation<bone_app::AcceptanceCursor>>,
    pub settings_profile: usize,
    pub settings_query: u64,
    pub login_states: BTreeMap<bone_app::ProfileId, bone_app::LoginState>,
    pub login_queries: BTreeMap<bone_app::ProfileId, u64>,
    pub session_management_operations: BTreeMap<SessionId, u64>,
    pub workspace_changes: WorkspaceChangesUi,
    pub artifact: ArtifactUi,
    pub dirty: bool,
    pub quitting: bool,
    pub(crate) next_generation: u64,
    pub(crate) overview_generation: u64,
    pub(crate) overview_pending: bool,
    pub(crate) next_session_management_operation: u64,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            workspace: None,
            main: MainView::Workbench,
            focus: Focus::Composer,
            global_selection: 0,
            detail_tab_selection: 0,
            action_selection: 0,
            focused_control: None,
            sessions: Vec::new(),
            selected: None,
            session_ui: BTreeMap::new(),
            detail: None,
            detail_scroll: 0,
            dialog: None,
            status: None,
            attention: Vec::new(),
            attention_projection_pending: false,
            unresolved_writes: Vec::new(),
            attention_detail: None,
            attention_selection: 0,
            write_resolution_applied: false,
            settings: None,
            results: BTreeMap::new(),
            acceptances: BTreeMap::new(),
            acceptance_queries: BTreeMap::new(),
            acceptance_loading: BTreeSet::new(),
            acceptance_windows_stale: BTreeSet::new(),
            acceptance_pages: BTreeMap::new(),
            settings_profile: 0,
            settings_query: 0,
            login_states: BTreeMap::new(),
            login_queries: BTreeMap::new(),
            session_management_operations: BTreeMap::new(),
            workspace_changes: WorkspaceChangesUi::default(),
            artifact: ArtifactUi::default(),
            dirty: true,
            quitting: false,
            next_generation: 1,
            overview_generation: 0,
            overview_pending: false,
            next_session_management_operation: 1,
        }
    }
}

/// Frontend-only navigation and request identity for App-owned workspace
/// change facts. The page and file body are both bounded by the App API.
#[derive(Clone, Debug, Default)]
pub struct WorkspaceChangesUi {
    pub query: u64,
    pub loading: bool,
    pub page: Option<bone_app::WorkspaceChangePage>,
    pub selection: usize,
    pub pages: PageNavigation<bone_app::WorkspaceChangeCursor>,
    pub file_query: u64,
    pub file_loading: bool,
    pub file: Option<bone_app::WorkspaceFilePage>,
    pub file_cursor: Option<bone_app::WorkspaceFileCursor>,
    pub file_back: Vec<Option<bone_app::WorkspaceFileCursor>>,
    pub file_request_cursor: Option<bone_app::WorkspaceFileCursor>,
    pub file_request_append: bool,
    pub file_request_path: Option<String>,
    pub file_request_source: Option<bone_app::WorkspaceFileSource>,
    pub file_control_selection: usize,
    pub(crate) file_layout: RefCell<TextLayoutCache>,
}

#[derive(Clone, Debug, Default)]
pub struct ArtifactUi {
    pub query: u64,
    pub loading: bool,
    pub artifact: Option<bone_app::ResultArtifact>,
    pub evidence: Option<bone_app::EvidencePage>,
    pub selection: usize,
    pub evidence_pages: PageNavigation<bone_app::EvidenceCursor>,
    pub source_query: u64,
    pub source_loading: bool,
    pub source: Option<EvidenceReaderUi>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageDirection {
    Refresh,
    Forward,
    Back,
}

#[derive(Clone, Debug)]
pub struct PageRequest<C> {
    pub cursor: Option<C>,
    pub direction: PageDirection,
}

#[derive(Clone, Debug)]
pub struct PageNavigation<C> {
    pub current: Option<C>,
    pub back: Vec<Option<C>>,
    pub pending: Option<PageRequest<C>>,
}

impl<C> Default for PageNavigation<C> {
    fn default() -> Self {
        Self {
            current: None,
            back: Vec::new(),
            pending: None,
        }
    }
}

impl<C: Clone + PartialEq> PageNavigation<C> {
    pub fn begin(&mut self, cursor: Option<C>, direction: PageDirection) -> bool {
        if self.pending.is_some() {
            return false;
        }
        self.pending = Some(PageRequest { cursor, direction });
        true
    }

    pub fn finish(&mut self) {
        let Some(request) = self.pending.take() else {
            return;
        };
        match request.direction {
            PageDirection::Refresh => self.back.clear(),
            PageDirection::Forward => {
                // Covers 65k workspace files at the production page size while
                // keeping even worst-case path-bearing cursors within budget.
                const MAX_BACK_PAGES: usize = 1024;
                if self.back.len() == MAX_BACK_PAGES {
                    self.back.remove(0);
                }
                self.back.push(self.current.clone());
            }
            PageDirection::Back => {
                if self.back.last() == Some(&request.cursor) {
                    self.back.pop();
                }
            }
        }
        self.current = request.cursor;
    }

    pub fn fail(&mut self) {
        self.pending = None;
    }
}

#[derive(Clone, Debug)]
pub struct EvidenceReaderUi {
    pub result: bone_app::ResultRef,
    pub source: bone_app::EvidenceRef,
    pub availability: bone_app::EvidenceAvailability,
    pub text: String,
    pub window_offset: u64,
    pub next_offset: Option<u64>,
    pub total_bytes: Option<u64>,
    pub projection_pending: bool,
    pub frontend_truncated: bool,
    pub(crate) layout: RefCell<TextLayoutCache>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TextLayoutCache {
    pub(crate) source_len: usize,
    pub(crate) width: u16,
    pub(crate) lines: Vec<(usize, usize)>,
}

#[derive(Clone, Debug)]
pub struct SettingsData {
    pub session: SessionId,
    pub generation: u64,
    pub query: u64,
    pub resolved: bone_app::ResolvedConfig,
    pub profiles: Vec<bone_app::Profile>,
}

impl UiState {
    pub fn selected_ui(&self) -> Option<&SessionUi> {
        self.selected.and_then(|id| self.session_ui.get(&id))
    }

    pub fn selected_ui_mut(&mut self) -> Option<&mut SessionUi> {
        self.selected.and_then(|id| self.session_ui.get_mut(&id))
    }

    /// The exact, freshly-confirmed result currently eligible for a user
    /// acceptance decision. A cached result is deliberately unavailable while
    /// its authoritative App refresh is in flight.
    pub fn acceptance_target(&self) -> Option<bone_app::ResultRef> {
        if self.detail_scroll != 0
            || !self
                .detail
                .as_ref()
                .is_some_and(|detail| detail.kind == DetailKind::Acceptance)
        {
            return None;
        }
        let session = self.selected?;
        let ui = self.session_ui.get(&session)?;
        if ui.result_loading
            || ui.result_confirmed_query != ui.result_query
            || ui.result_pages.current.is_some()
        {
            return None;
        }
        self.results
            .get(&session)?
            .items
            .last()
            .map(|summary| summary.result)
    }

    pub(crate) fn generation(&mut self) -> u64 {
        let value = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        value
    }

    pub(crate) fn session_management_operation(&mut self) -> u64 {
        let value = self.next_session_management_operation;
        self.next_session_management_operation = self
            .next_session_management_operation
            .wrapping_add(1)
            .max(1);
        value
    }
}

pub(crate) fn history_entry_bytes(entry: &HistoryEntry) -> usize {
    const BASE: usize = std::mem::size_of::<HistoryEntry>();
    let payload = match &entry.event {
        bone_app::SessionEvent::InputSubmitted { text, .. }
        | bone_app::SessionEvent::Reply { text, .. }
        | bone_app::SessionEvent::QuestionAsked { text, .. } => text.len(),
        bone_app::SessionEvent::InputRejected { message, .. }
        | bone_app::SessionEvent::RoutingFailed { message, .. } => message.len(),
        bone_app::SessionEvent::JobFinished {
            summary, remaining, ..
        } => summary.len() + remaining.iter().map(String::len).sum::<usize>(),
        bone_app::SessionEvent::AcceptanceRecorded { reason, .. } => reason.len(),
        bone_app::SessionEvent::ToolFinished { tool, outcome, .. } => {
            tool.len()
                + match &outcome.result {
                    Ok(value) => json_value_bytes(value),
                    Err(error) => error.message.len(),
                }
        }
        bone_app::SessionEvent::RuntimeStarted { config, .. }
        | bone_app::SessionEvent::RuntimeReconfigured { config, .. } => {
            config.workspace.as_os_str().as_encoded_bytes().len()
                + config.worker.profile.id.as_str().len()
                + config.worker.profile.label.len()
                + config.worker.selection.model.len()
                + config.coordinator.profile.id.as_str().len()
                + config.coordinator.profile.label.len()
                + config.coordinator.selection.model.len()
        }
        bone_app::SessionEvent::WriteResolved { evidence, .. } => evidence.len(),
        bone_app::SessionEvent::Interrupted { inputs, .. } => inputs
            .len()
            .saturating_mul(std::mem::size_of::<bone_app::InputId>()),
        _ => 0,
    };
    BASE.saturating_add(payload)
}

fn json_value_bytes(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            std::mem::size_of::<serde_json::Value>()
        }
        serde_json::Value::String(value) => value.len(),
        serde_json::Value::Array(values) => values.iter().map(json_value_bytes).sum(),
        serde_json::Value::Object(values) => values
            .iter()
            .map(|(key, value)| key.len().saturating_add(json_value_bytes(value)))
            .sum(),
    }
}
