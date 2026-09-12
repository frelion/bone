pub use crate::run::models::{ModelChoice, ModelFacts};
use std::{collections::BTreeMap, sync::Arc};

use bone_app::{QuestionId, RequestId, SessionId, SessionInfo, SessionSummary, SessionView};

use crate::layout::SinglePane;

use super::{
    TranscriptState,
    panel::{ModelOperation, Panel},
    title::TitleState,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Focus {
    Sessions,
    SessionTitle,
    #[default]
    Composer,
    RightRail,
}

#[derive(Clone, Debug)]
pub(crate) struct SessionNavRow {
    pub(crate) summary: SessionSummary,
    pub(crate) needs_attention: bool,
}

impl SessionNavRow {
    pub(crate) fn provisional(info: SessionInfo) -> Self {
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| {
                i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
            });
        Self {
            summary: SessionSummary {
                session: info,
                created_at,
                message_count: 0,
                latest_reply_preview: None,
                projection_pending: false,
                has_draft: false,
                draft_bytes: 0,
                persisted_runtime: None,
                history_through: bone_app::SessionSeq(0),
            },
            needs_attention: false,
        }
    }

    pub(crate) fn id(&self) -> SessionId {
        self.summary.session.id
    }

    pub(crate) fn info(&self) -> &SessionInfo {
        &self.summary.session
    }
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
    pub id: SessionId,
    pub generation: u64,
    pub snapshot: Option<Arc<SessionView>>,
    pub(crate) transcript: TranscriptState,
    pub(crate) draft: crate::editor::EditorBuffer,
    pub saved_draft_revision: u64,
    pub answer_drafts: BTreeMap<QuestionId, super::answer::AnswerDraft>,
    pub selected_answer: Option<QuestionId>,
    pub hydrated: bool,
    pub submitting: Option<PendingSubmission>,
    pub bootstrap_submission: Option<PendingSubmission>,
}

impl SessionUi {
    #[cfg(test)]
    pub(crate) fn draft(&self) -> &str {
        self.draft.text()
    }

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

    pub fn new(id: SessionId, generation: u64) -> Self {
        Self {
            id,
            generation,
            snapshot: None,
            transcript: TranscriptState::default(),
            draft: Default::default(),
            saved_draft_revision: 0,
            answer_drafts: BTreeMap::new(),
            selected_answer: None,
            hydrated: false,
            submitting: None,
            bootstrap_submission: None,
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

#[derive(Debug)]
pub struct UiState {
    pub(crate) pane_widths: crate::layout::PaneWidths,
    pub(crate) dragging_divider: Option<crate::layout::PaneDivider>,
    pub(crate) panel: Option<Panel>,
    pub(crate) model_label_request: u64,
    pub(crate) model_operation: Option<ModelOperation>,
    pub(super) titles: TitleState,
    pub(crate) terminal_capabilities: crate::terminal::TerminalCapabilities,

    pub workspace_label: Option<String>,
    pub model_label: Option<String>,
    pub model_facts: Option<ModelFacts>,
    pub(crate) session_rows: Vec<SessionNavRow>,
    pub selected: Option<SessionId>,
    pub session_candidate: Option<SessionId>,
    pub session_scroll: Option<usize>,
    pub session_ui: BTreeMap<SessionId, SessionUi>,
    pub(crate) focus: Focus,
    last_center: Focus,
    pub(crate) caret_visible: bool,
    pub(crate) orphan_draft: crate::editor::EditorBuffer,
    pub pending_create: Option<PendingCreate>,
    pub slash_selection: usize,
    pub slash_dismissed: Option<(Option<SessionId>, u64)>,
    pub status: Option<String>,
    pub dirty: bool,
    pub quitting: bool,
    pub(crate) overview_request: Option<u64>,
    next_generation: u64,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            pane_widths: Default::default(),
            dragging_divider: None,
            panel: None,
            model_label_request: 0,
            model_operation: None,
            titles: TitleState::default(),
            terminal_capabilities: Default::default(),
            workspace_label: None,
            model_label: None,
            model_facts: None,
            session_rows: Vec::new(),
            selected: None,
            session_candidate: None,
            session_scroll: None,
            session_ui: BTreeMap::new(),
            focus: Focus::Composer,
            last_center: Focus::Composer,
            caret_visible: true,
            orphan_draft: Default::default(),
            pending_create: None,
            slash_selection: 0,
            slash_dismissed: None,
            status: None,
            dirty: true,
            quitting: false,
            overview_request: None,
            next_generation: 0,
        }
    }
}

impl UiState {
    /// Sets workspace focus while remembering the latest center-column target.
    pub(crate) fn set_focus(&mut self, focus: Focus) {
        self.focus = focus;
        if matches!(focus, Focus::SessionTitle | Focus::Composer) {
            self.last_center = focus;
        }
    }

    pub(crate) const fn last_center_focus(&self) -> Focus {
        self.last_center
    }

    pub(crate) fn remove_title_focus(&mut self) {
        if self.focus == Focus::SessionTitle {
            self.set_focus(Focus::Composer);
        }
        if self.last_center == Focus::SessionTitle {
            self.last_center = Focus::Composer;
        }
    }

    pub(crate) fn blinking_caret_active(&self) -> bool {
        match &self.panel {
            Some(Panel::Models(models)) if models.setup().is_some_and(|form| !form.saving) => true,
            Some(_) => false,
            None => {
                matches!(self.focus, Focus::SessionTitle | Focus::Composer)
                    && (self.focus != Focus::SessionTitle || self.selected.is_some())
            }
        }
    }

    /// Prepare the selected Session's title as a one-line editor. Re-entering
    /// the same title keeps any text that has not been committed yet.
    pub(crate) fn begin_title_edit(&mut self) -> bool {
        let Some(session) = self.selected.filter(|id| self.session_row(*id).is_some()) else {
            return false;
        };
        if self.titles.edit_target() != Some(session) {
            let title = self
                .session_title(session)
                .expect("selected Session has a navigation row")
                .to_owned();
            self.titles.begin_edit(session, title);
        }
        true
    }

    pub(crate) fn title_text(&self) -> Option<&str> {
        let selected = self.selected?;
        if let Some(editor) = self.titles.editor(selected) {
            Some(editor.text())
        } else {
            self.session_title(selected)
        }
    }

    pub(crate) fn session_row(&self, id: SessionId) -> Option<&SessionNavRow> {
        self.session_rows.iter().find(|row| row.id() == id)
    }

    pub(crate) fn session_row_mut(&mut self, id: SessionId) -> Option<&mut SessionNavRow> {
        self.session_rows.iter_mut().find(|row| row.id() == id)
    }

    pub(crate) fn session_title(&self, id: SessionId) -> Option<&str> {
        self.titles
            .desired(id)
            .or_else(|| self.session_row(id).map(|row| row.info().title.as_str()))
    }

    pub(crate) fn title_editor(&self) -> Option<&crate::editor::EditorBuffer> {
        self.selected
            .and_then(|session| self.titles.editor(session))
    }

    pub(crate) fn clear_title_edit(&mut self) {
        self.titles.clear_edit();
    }

    pub(crate) fn title_rename_pending(&self, session: SessionId) -> bool {
        self.titles.manual_pending(session)
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
        self.selected_ui().map_or(self.orphan_draft.text(), |ui| {
            ui.active_answer()
                .map_or(ui.draft.text(), |answer| answer.editor.text())
        })
    }

    pub fn draft_cursor(&self) -> usize {
        self.selected_ui().map_or(self.orphan_draft.cursor(), |ui| {
            ui.active_answer()
                .map_or(ui.draft.cursor(), |answer| answer.editor.cursor())
        })
    }

    pub(crate) fn editor(&self) -> &crate::editor::EditorBuffer {
        self.selected_ui().map_or(&self.orphan_draft, |ui| {
            ui.active_answer()
                .map_or(&ui.draft, |answer| &answer.editor)
        })
    }

    /// Mutable editor for the active answer, ordinary session draft, or orphan.
    pub(crate) fn editor_mut(&mut self) -> &mut crate::editor::EditorBuffer {
        if let Some(ui) = self.selected.and_then(|id| self.session_ui.get_mut(&id)) {
            if let Some(answer) = ui
                .selected_answer
                .and_then(|id| ui.answer_drafts.get_mut(&id))
            {
                return &mut answer.editor;
            }
            &mut ui.draft
        } else {
            &mut self.orphan_draft
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
        let Some(query) = self.slash_query() else {
            return Vec::new();
        };
        COMMANDS
            .iter()
            .filter(|command| command.name.starts_with(query))
            .collect()
    }

    /// Whether the Composer's command surface is open, independently of
    /// whether the current query matches a command.
    pub fn slash_palette_visible(&self) -> bool {
        self.slash_query().is_some()
    }

    fn slash_query(&self) -> Option<&str> {
        if self.focus != Focus::Composer
            || self.panel.is_some()
            || self
                .selected_ui()
                .is_some_and(|ui| ui.selected_answer.is_some())
            || self.slash_dismissed == Some(self.draft_identity())
        {
            return None;
        }
        let query = self.draft().trim_start().strip_prefix('/')?;
        (!query.contains(char::is_whitespace)).then_some(query)
    }

    pub fn draft_identity(&self) -> (Option<SessionId>, u64) {
        self.selected_ui()
            .map_or((None, self.orphan_draft.revision()), |ui| {
                (Some(ui.id), ui.draft.revision())
            })
    }

    pub(crate) fn generation(&mut self) -> u64 {
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        self.next_generation
    }
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
        let mut ui = SessionUi::new(info.id, 1);
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
