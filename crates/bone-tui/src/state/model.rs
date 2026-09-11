pub use crate::run::models::{ModelChoice, ModelFacts};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

use bone_app::{
    HistoryCursor, HistoryEntry, QuestionId, RequestId, SessionId, SessionInfo, SessionView,
    WorkspaceId,
};

use crate::layout::{SinglePane, TranscriptMetrics};

pub const HISTORY_CACHE_ITEMS: usize = 512;
// Reserve the other half of the 32 MiB cache budget for editor history and reader layout.
pub const HISTORY_CACHE_BYTES: usize = 16 * 1024 * 1024;

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
    Model,
    Details,
    Answer,
    Recover,
    Retry,
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
        kind: CommandKind::Model,
        name: "model",
        usage: "[profile model]",
        summary: "Models, API keys & accounts",
    },
    CommandSpec {
        kind: CommandKind::Answer,
        name: "answer",
        usage: "",
        summary: "Answer the current question",
    },
    CommandSpec {
        kind: CommandKind::Details,
        name: "details",
        usage: "",
        summary: "Choose a task or tool result",
    },
    CommandSpec {
        kind: CommandKind::Recover,
        name: "recover",
        usage: "",
        summary: "Restore the latest cancelled input",
    },
    CommandSpec {
        kind: CommandKind::Retry,
        name: "retry",
        usage: "",
        summary: "Retry the original request",
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
    pub reply_to: Option<QuestionId>,
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
    pub editor: crate::editor::EditorState,
    pub draft_revision: u64,
    pub saved_draft_revision: u64,
    pub saved_draft: String,
    pub answer_drafts: BTreeMap<QuestionId, super::answer::AnswerDraft>,
    pub selected_answer: Option<QuestionId>,
    pub hydrated: bool,
    pub submitting: Option<PendingSubmission>,
    pub bootstrap_submission: Option<PendingSubmission>,
    pub scroll_from_tail: usize,
    pub read_anchor: Option<crate::layout::ContentAnchor>,
    pub transcript_metrics: Option<Arc<TranscriptMetrics>>,
    pub unread: usize,
}

impl SessionUi {
    pub fn active_answer(&self) -> Option<&super::answer::AnswerDraft> {
        self.selected_answer
            .and_then(|id| self.answer_drafts.get(&id))
    }

    pub fn working(&self) -> bool {
        self.snapshot.as_ref().is_some_and(|snapshot| {
            !snapshot.activity.is_empty()
                || snapshot.jobs.iter().any(|job| {
                    matches!(
                        job.state,
                        bone_app::JobState::Running | bone_app::JobState::Waiting(_)
                    )
                })
                || snapshot.inputs.iter().any(|input| {
                    matches!(
                        input.state,
                        bone_app::InputState::Posting { .. }
                            | bone_app::InputState::Accepted { .. }
                            | bone_app::InputState::WaitingForUser { .. }
                    )
                })
        })
    }

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
            editor: Default::default(),
            draft_revision: 0,
            saved_draft_revision: 0,
            saved_draft: String::new(),
            answer_drafts: BTreeMap::new(),
            selected_answer: None,
            hydrated: false,
            submitting: None,
            bootstrap_submission: None,
            scroll_from_tail: 0,
            read_anchor: None,
            transcript_metrics: None,
            unread: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PendingCreate {
    pub first_input: Option<PendingSubmission>,
    pub request_id: RequestId,
    pub source: DraftSource,
    pub title: String,
    pub provisional: bool,
    pub failed: bool,
}

#[derive(Clone, Debug)]
pub enum DraftSource {
    None,
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
pub(crate) enum Panel {
    Objects {
        session: SessionId,
        choices: Vec<(super::reader::ReaderSource, String)>,
    },
    Rename,
    Login,
    Commands,
    Models,
    ModelSetup,
    ModelAdd,
    Help,
    Reader(super::reader::ReaderContent),
}

#[derive(Clone, Debug)]
pub struct UiState {
    pub(crate) pane_widths: crate::layout::PaneWidths,
    pub(crate) dragging_divider: Option<crate::layout::PaneDivider>,
    pub(crate) panel: Option<Panel>,
    pub(crate) login_request: Option<u64>,
    pub(crate) model_label_request: u64,
    pub(crate) login_state: bone_app::LoginState,
    pub(crate) panel_return: Focus,
    pub(crate) panel_selection: usize,
    pub(crate) panel_scroll: usize,
    pub(crate) model_choices: Vec<ModelChoice>,
    pub(crate) model_profiles: Vec<bone_app::Profile>,
    pub(crate) connection_form: Option<super::ConnectionForm>,
    pub(crate) rename_input: String,
    pub(crate) rename_target: Option<(SessionId, u64)>,
    pub(crate) models_loading: bool,
    pub(crate) model_request: u64,
    pub(crate) command_from_palette: bool,
    pub(crate) terminal_capabilities: crate::terminal::TerminalCapabilities,

    pub workspace: Option<(WorkspaceId, String)>,
    pub model_label: Option<String>,
    pub model_facts: Option<ModelFacts>,
    pub sessions: Vec<SessionInfo>,
    pub session_statuses: BTreeMap<SessionId, SessionStatus>,
    pub selected: Option<SessionId>,
    pub session_candidate: Option<SessionId>,
    pub session_scroll: Option<usize>,
    pub session_ui: BTreeMap<SessionId, SessionUi>,
    pub focus: Focus,
    pub orphan_draft: String,
    pub orphan_cursor: usize,
    pub orphan_editor: crate::editor::EditorState,
    pub preferred_column: Option<usize>,
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
            pane_widths: Default::default(),
            dragging_divider: None,
            panel: None,
            login_state: bone_app::LoginState::Connecting,
            login_request: None,
            model_label_request: 0,
            panel_return: Focus::Composer,
            panel_selection: 0,
            panel_scroll: 0,
            model_choices: Vec::new(),
            model_profiles: Vec::new(),
            connection_form: None,
            rename_input: String::new(),
            rename_target: None,
            models_loading: false,
            model_request: 0,
            command_from_palette: false,
            terminal_capabilities: Default::default(),
            workspace: None,
            model_label: None,
            model_facts: None,
            sessions: Vec::new(),
            session_statuses: BTreeMap::new(),
            selected: None,
            session_candidate: None,
            session_scroll: None,
            session_ui: BTreeMap::new(),
            focus: Focus::Composer,
            orphan_draft: String::new(),
            orphan_cursor: 0,
            orphan_editor: Default::default(),
            preferred_column: None,
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
    pub(crate) fn model_row_count(&self) -> usize {
        self.model_choices.len() + self.model_profiles.len() + 1
    }

    pub(crate) fn running_model(&self) -> Option<&bone_app::ResolvedModel> {
        if let Some(snapshot) = self.selected_ui().and_then(|ui| ui.snapshot.as_ref()) {
            return match &snapshot.runtime {
                bone_app::RuntimeState::Running { config, .. } => Some(&config.worker),
                _ => None,
            };
        }
        self.model_facts
            .as_ref()
            .and_then(|facts| facts.running.as_ref())
    }

    pub(crate) fn model_footer(&self) -> String {
        if let Some(running) = self.running_model() {
            let changed = self
                .model_facts
                .as_ref()
                .is_some_and(|facts| !facts.applied_to(Some(running)));
            return format!(
                "{}{}",
                running.selection.model,
                if changed {
                    " · saved change"
                } else {
                    " · running"
                }
            );
        }
        self.model_label
            .as_ref()
            .map_or_else(|| "Select model".into(), |label| format!("{label} · saved"))
    }

    pub(crate) fn model_configuration_summary(&self) -> String {
        fn describe(model: &bone_app::ResolvedModel) -> String {
            let options = model
                .selection
                .options
                .as_ref()
                .map(|options| format!(" · {}", serde_json::to_string(options).unwrap_or_default()))
                .unwrap_or_default();
            format!(
                "{}/{}{options}",
                model.selection.profile, model.selection.model
            )
        }
        let running = self.running_model();
        let saved = self
            .model_facts
            .as_ref()
            .and_then(|facts| facts.saved.as_ref().ok());
        let mut lines = Vec::new();
        if let Some(running) = running {
            lines.push(format!("Running: {}", describe(running)));
        }
        if let Some(saved) = saved {
            let label = if running.is_some()
                && self
                    .model_facts
                    .as_ref()
                    .is_some_and(|facts| !facts.applied_to(running))
            {
                "Saved, not running"
            } else {
                "Saved"
            };
            lines.push(format!("{label}: {}", describe(saved)));
        } else if self
            .model_facts
            .as_ref()
            .is_some_and(|facts| facts.saved.is_err())
        {
            lines.push("Saved configuration needs attention".into());
        } else if let Some(label) = &self.model_label {
            lines.push(format!("Saved: {label}"));
        }
        lines.join("\n")
    }

    pub fn selected_ui(&self) -> Option<&SessionUi> {
        self.selected.and_then(|id| self.session_ui.get(&id))
    }

    pub fn selected_ui_mut(&mut self) -> Option<&mut SessionUi> {
        self.selected.and_then(|id| self.session_ui.get_mut(&id))
    }

    pub fn draft(&self) -> &str {
        self.selected_ui().map_or(self.orphan_draft.as_str(), |ui| {
            ui.active_answer()
                .map_or(ui.draft.as_str(), |answer| answer.text.as_str())
        })
    }

    pub fn draft_cursor(&self) -> usize {
        self.selected_ui().map_or(self.orphan_cursor, |ui| {
            ui.active_answer()
                .map_or(ui.draft_cursor, |answer| answer.cursor)
        })
    }

    pub(crate) fn editor(&self) -> &crate::editor::EditorState {
        self.selected_ui().map_or(&self.orphan_editor, |ui| {
            ui.active_answer()
                .map_or(&ui.editor, |answer| &answer.editor)
        })
    }

    /// Mutable editor for the active answer, ordinary session draft, or orphan.
    /// Ordinary draft persistence deliberately continues to read SessionUi.draft.
    pub(crate) fn editor_mut(&mut self) -> crate::editor::EditBuffer<'_> {
        if let Some(ui) = self.selected.and_then(|id| self.session_ui.get_mut(&id)) {
            if let Some(answer) = ui
                .selected_answer
                .and_then(|id| ui.answer_drafts.get_mut(&id))
            {
                return crate::editor::EditBuffer::new(
                    &mut answer.text,
                    &mut answer.cursor,
                    &mut answer.revision,
                    &mut answer.editor,
                );
            }
            crate::editor::EditBuffer::new(
                &mut ui.draft,
                &mut ui.draft_cursor,
                &mut ui.draft_revision,
                &mut ui.editor,
            )
        } else {
            crate::editor::EditBuffer::new(
                &mut self.orphan_draft,
                &mut self.orphan_cursor,
                &mut self.orphan_revision,
                &mut self.orphan_editor,
            )
        }
    }

    pub fn single_pane(&self) -> SinglePane {
        if self.focus == Focus::Sessions {
            SinglePane::Sessions
        } else {
            SinglePane::Conversation
        }
    }

    pub fn slash_matches(&self) -> Vec<&'static CommandSpec> {
        if matches!(self.panel, Some(Panel::Commands)) {
            return COMMANDS.iter().collect();
        }
        if self.panel.is_some() {
            return Vec::new();
        }
        if self
            .selected_ui()
            .is_some_and(|ui| ui.selected_answer.is_some())
        {
            return Vec::new();
        }
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

#[cfg(test)]
mod model_fact_tests {
    use super::*;

    fn model() -> bone_app::ResolvedModel {
        bone_app::ResolvedModel {
            selection: bone_app::ModelSelection::new(bone_app::ProfileId::chatgpt(), "same-name")
                .unwrap(),
            profile: bone_app::Profile::chatgpt(),
        }
    }

    #[test]
    fn same_name_with_different_profile_or_options_is_not_applied() {
        let running = model();
        let mut other_profile = running.clone();
        other_profile.selection.profile = bone_app::ProfileId::new("another").unwrap();
        other_profile.profile.id = other_profile.selection.profile.clone();
        let mut other_options = running.clone();
        other_options.selection.options = Some(serde_json::from_value(serde_json::json!({ "type": "openai_responses", "reasoning": { "effort": "high" } })).unwrap());
        for saved in [other_profile, other_options] {
            let facts = ModelFacts {
                saved: Ok(saved),
                running: Some(running.clone()),
            };
            assert!(!facts.applied_to(Some(&running)));
            let state = UiState {
                model_label: Some("same-name".into()),
                model_facts: Some(facts),
                ..UiState::default()
            };
            assert_eq!(state.model_footer(), "same-name · saved change");
            assert!(
                state
                    .model_configuration_summary()
                    .contains("Saved, not running:")
            );
        }
    }

    #[test]
    fn detached_saved_model_is_neutral_and_runtime_snapshot_overrides_cached_running() {
        let running = model();
        let mut state = UiState {
            model_label: Some("same-name".into()),
            model_facts: Some(ModelFacts {
                saved: Ok(running.clone()),
                running: None,
            }),
            ..UiState::default()
        };
        assert_eq!(state.model_footer(), "same-name · saved");
        assert!(!state.model_configuration_summary().contains("not running"));
        state.model_facts.as_mut().unwrap().running = Some(running);
        assert_eq!(state.model_footer(), "same-name · running");
        let info = bone_app::SessionInfo {
            id: SessionId::new(),
            workspace: bone_app::WorkspaceId::new(),
            title: "test".into(),
            archived: false,
        };
        let mut ui = SessionUi::new(info.clone(), 1);
        ui.snapshot = Some(std::sync::Arc::new(bone_app::SessionView {
            session: info.clone(),
            runtime: bone_app::RuntimeState::Detached,
            draft: String::new(),
            inputs: vec![],
            jobs: vec![],
            activity: vec![],
            history_through: bone_app::SessionSeq(0),
            problem: None,
        }));
        state.selected = Some(info.id);
        state.session_ui.insert(info.id, ui);
        assert_eq!(state.model_footer(), "same-name · saved");
    }
}

#[cfg(test)]
mod model_apply_failure_tests {
    use super::*;
    #[test]
    fn saved_config_after_failed_apply_does_not_replace_running_footer() {
        let running = bone_app::ResolvedModel {
            selection: bone_app::ModelSelection::new(bone_app::ProfileId::chatgpt(), "old-running")
                .unwrap(),
            profile: bone_app::Profile::chatgpt(),
        };
        let mut saved = running.clone();
        saved.selection.model = "new-saved".into();
        let mut state = UiState::default();
        crate::state::update(
            &mut state,
            crate::state::UiEvent::ModelApplied {
                session: None,
                request: 0,
                label: Some("new-saved".into()),
                facts: Some(ModelFacts {
                    saved: Ok(saved),
                    running: Some(running),
                }),
                error: Some("request failed".into()),
            },
        );
        assert_eq!(state.model_footer(), "old-running · saved change");
        assert!(
            state
                .model_configuration_summary()
                .contains("Saved, not running: chatgpt/new-saved")
        );
        assert_eq!(state.status.as_deref(), Some("request failed"));
    }
}
