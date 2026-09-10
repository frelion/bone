use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

use bone_app::{
    HistoryCursor, HistoryEntry, RequestId, SessionId, SessionInfo, SessionView, WorkspaceId,
};

use crate::layout::{SinglePane, TranscriptMetrics};

pub const HISTORY_CACHE_ITEMS: usize = 512;
pub const HISTORY_CACHE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Focus {
    Sessions,
    Conversation,
    #[default]
    Composer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionStatus {
    Ready,
    Draft,
    NeedsAttention,
    Recoverable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandSpec {
    pub kind: CommandKind,
    pub name: &'static str,
    pub usage: &'static str,
    pub summary: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandKind {
    New,
    Sessions,
    Rename,
    Help,
    Quit,
}

pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        kind: CommandKind::New,
        name: "new",
        usage: "[title]",
        summary: "Create a session",
    },
    CommandSpec {
        kind: CommandKind::Sessions,
        name: "sessions",
        usage: "",
        summary: "Focus sessions",
    },
    CommandSpec {
        kind: CommandKind::Rename,
        name: "rename",
        usage: "<title>",
        summary: "Rename this session",
    },
    CommandSpec {
        kind: CommandKind::Help,
        name: "help",
        usage: "",
        summary: "Show available commands",
    },
    CommandSpec {
        kind: CommandKind::Quit,
        name: "quit",
        usage: "",
        summary: "Save drafts and quit",
    },
];

#[derive(Clone, Debug)]
pub struct PendingSubmission {
    pub request_id: RequestId,
    pub text: String,
    pub draft_revision: u64,
    pub failed: bool,
}

#[derive(Clone, Debug)]
pub struct SessionUi {
    pub info: SessionInfo,
    pub generation: u64,
    pub snapshot: Option<Arc<SessionView>>,
    pub history: VecDeque<HistoryEntry>,
    pub history_bytes: usize,
    pub history_cursor: bone_app::SessionSeq,
    pub older_cursor: Option<HistoryCursor>,
    pub older_loading: bool,
    pub older_metrics: Option<Arc<TranscriptMetrics>>,
    pub history_loading: bool,
    pub recent_loading: bool,
    pub newer_history_missing: bool,
    pub draft: String,
    pub draft_cursor: usize,
    pub draft_revision: u64,
    pub saved_draft_revision: u64,
    pub saved_draft: String,
    pub hydrated: bool,
    pub submitting: Option<PendingSubmission>,
    pub scroll_from_tail: usize,
    pub unread: usize,
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
            older_cursor: None,
            older_loading: false,
            older_metrics: None,
            history_loading: false,
            recent_loading: false,
            newer_history_missing: false,
            draft: String::new(),
            draft_cursor: 0,
            draft_revision: 0,
            saved_draft_revision: 0,
            saved_draft: String::new(),
            hydrated: false,
            submitting: None,
            scroll_from_tail: 0,
            unread: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PendingCreate {
    pub request_id: RequestId,
    pub source: DraftSource,
    pub title: String,
    pub provisional: bool,
    pub failed: bool,
}

#[derive(Clone, Debug)]
pub enum DraftSource {
    Orphan {
        revision: u64,
        text: String,
    },
    Session {
        id: SessionId,
        generation: u64,
        revision: u64,
        text: String,
    },
}

#[derive(Clone, Debug)]
pub struct UiState {
    pub workspace: Option<(WorkspaceId, String)>,
    pub model_label: Option<String>,
    pub sessions: Vec<SessionInfo>,
    pub session_statuses: BTreeMap<SessionId, SessionStatus>,
    pub selected: Option<SessionId>,
    pub session_ui: BTreeMap<SessionId, SessionUi>,
    pub focus: Focus,
    pub orphan_draft: String,
    pub orphan_cursor: usize,
    pub orphan_revision: u64,
    pub pending_create: Option<PendingCreate>,
    pub slash_selection: usize,
    pub slash_dismissed: Option<(Option<SessionId>, u64)>,
    pub status: Option<String>,
    pub dirty: bool,
    pub quitting: bool,
    pub(crate) overview_generation: u64,
    pub(crate) overview_pending: bool,
    next_generation: u64,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            workspace: None,
            model_label: None,
            sessions: Vec::new(),
            session_statuses: BTreeMap::new(),
            selected: None,
            session_ui: BTreeMap::new(),
            focus: Focus::Composer,
            orphan_draft: String::new(),
            orphan_cursor: 0,
            orphan_revision: 0,
            pending_create: None,
            slash_selection: 0,
            slash_dismissed: None,
            status: None,
            dirty: true,
            quitting: false,
            overview_generation: 0,
            overview_pending: false,
            next_generation: 0,
        }
    }
}

impl UiState {
    pub fn selected_ui(&self) -> Option<&SessionUi> {
        self.selected.and_then(|id| self.session_ui.get(&id))
    }

    pub fn selected_ui_mut(&mut self) -> Option<&mut SessionUi> {
        self.selected.and_then(|id| self.session_ui.get_mut(&id))
    }

    pub fn draft(&self) -> &str {
        self.selected_ui()
            .map_or(&self.orphan_draft, |ui| &ui.draft)
    }

    pub fn single_pane(&self) -> SinglePane {
        if self.focus == Focus::Sessions {
            SinglePane::Sessions
        } else {
            SinglePane::Conversation
        }
    }

    pub fn slash_matches(&self) -> Vec<&'static CommandSpec> {
        if self.slash_dismissed == Some(self.draft_identity()) {
            return Vec::new();
        }
        let Some(query) = self.draft().trim_start().strip_prefix('/') else {
            return Vec::new();
        };
        if query.contains(char::is_whitespace) {
            return Vec::new();
        }
        COMMANDS
            .iter()
            .filter(|command| command.name.starts_with(query))
            .collect()
    }

    pub fn draft_identity(&self) -> (Option<SessionId>, u64) {
        self.selected_ui()
            .map_or((None, self.orphan_revision), |ui| {
                (Some(ui.info.id), ui.draft_revision)
            })
    }

    pub(crate) fn generation(&mut self) -> u64 {
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        self.next_generation
    }
}

pub(crate) fn history_entry_bytes(entry: &HistoryEntry) -> usize {
    serde_json::to_vec(entry).map_or(0, |bytes| bytes.len())
}
