#[cfg(test)]
use bone_app::SessionSeq;
use bone_app::{LoginState, ModelSelection, Profile, SessionId};
use unicode_segmentation::UnicodeSegmentation;

use super::{
    ConnectionForm, Effect, KindChoice, ModelChoice, ModelFacts, ModelForm, SecretText, Status,
    UiState,
    details::{ReaderState, pin_reading},
    reader::{ReaderContent, ReaderSource},
};

pub(crate) const REASONING_EFFORTS: [bone_app::ReasoningEffort; 7] = [
    bone_app::ReasoningEffort::None,
    bone_app::ReasoningEffort::Minimal,
    bone_app::ReasoningEffort::Low,
    bone_app::ReasoningEffort::Medium,
    bone_app::ReasoningEffort::High,
    bone_app::ReasoningEffort::Xhigh,
    bone_app::ReasoningEffort::Max,
];

#[derive(Debug)]
pub(crate) enum Overlay {
    Objects(ObjectPanel),
    Models(ModelPanel),
    Help,
}

#[derive(Debug)]
pub(crate) struct ObjectPanel {
    pub(crate) session: SessionId,
    pub(crate) choices: Vec<(ReaderSource, String)>,
    pub(crate) selected: usize,
}

#[derive(Debug)]
pub(crate) struct ModelPanel {
    pub(crate) session: Option<SessionId>,
    pub(crate) choices: Vec<ModelChoice>,
    pub(crate) profiles: Vec<Profile>,
    /// The tab the panel shows: an index into `profiles`. `profiles.len()` is
    /// the trailing "add connection" tab, so a connection that was never
    /// configured occupies no tab.
    pub(crate) tab: usize,
    /// Set until a freshly opened panel has seen its first load reply. That one
    /// reply lands the strip on the connection the current model belongs to;
    /// every later reload keeps the tab the user chose.
    follow_current_model: bool,
    pending_selection: Option<ModelSelection>,
    pending_reasoning: bool,
    pub(crate) screen: ModelScreen,
}

/// One screen of the connections-then-models panel.
///
/// The first screen is always the tab strip: one tab per saved connection plus
/// the trailing add tab. Choosing a model, editing or deleting a connection and
/// adding one are all reachable from there.
#[derive(Debug)]
pub(crate) enum ModelScreen {
    /// A connection tab. `selected` is a row of [`ModelPanel::tab_row_count`]:
    /// `0..tab_models().len()` are that connection's models and the last row is
    /// the manual model input.
    Tab {
        selected: usize,
    },
    ModelInput {
        value: String,
    },
    Kind {
        selected: usize,
    },
    Setup(Box<ConnectionForm>),
    ModelForm(Box<ModelForm>),
    /// Deleting the current connection. `selected` is the tab row to come back
    /// to when the user declines, so cancelling costs nothing.
    ConfirmDelete {
        selected: usize,
    },
    Reasoning {
        selected: usize,
        selection: ModelSelection,
    },
    Login {
        request: u64,
        state: LoginState,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ModelOperation {
    pub(crate) session: Option<SessionId>,
    pub(crate) request: u64,
    pub(crate) kind: ModelOperationKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ModelOperationKind {
    Load,
    Apply,
    Delete,
}

impl ModelPanel {
    pub(crate) fn new(session: Option<SessionId>) -> Self {
        Self {
            session,
            choices: Vec::new(),
            profiles: Vec::new(),
            tab: 0,
            follow_current_model: true,
            pending_selection: None,
            pending_reasoning: false,
            screen: ModelScreen::Tab { selected: 0 },
        }
    }

    /// Whether the current tab is the trailing "add connection" tab.
    pub(crate) fn tab_is_add(&self) -> bool {
        self.tab == self.profiles.len()
    }

    /// The saved connection behind the current tab, if it is not the add tab.
    pub(crate) fn tab_profile(&self) -> Option<&Profile> {
        if self.tab_is_add() {
            None
        } else {
            self.profiles.get(self.tab)
        }
    }

    /// The models of the current tab: the catalog entries of its connection,
    /// followed by every model saved on the connection that the catalog did not
    /// publish. A model added by hand stays visible on its own connection.
    pub(crate) fn tab_models(&self) -> Vec<ModelSelection> {
        let Some(profile) = self.tab_profile() else {
            return Vec::new();
        };
        let mut models: Vec<ModelSelection> = self
            .choices
            .iter()
            .filter(|choice| choice.selection.profile == profile.id)
            .map(|choice| choice.selection.clone())
            .collect();
        for model in &profile.models {
            if models.iter().any(|saved| saved.model == *model) {
                continue;
            }
            if let Ok(selection) = ModelSelection::new(profile.id.clone(), model.clone()) {
                models.push(selection);
            }
        }
        models
    }

    /// The rows of the current tab: its models plus the manual input row.
    pub(crate) fn tab_row_count(&self) -> usize {
        self.tab_models().len() + 1
    }

    /// The row of `selection` inside the current tab, if the tab still shows it.
    fn tab_row_of(&self, selection: &ModelSelection) -> Option<usize> {
        self.tab_models()
            .iter()
            .position(|candidate| same_model(candidate, selection))
    }

    pub(crate) fn row_count(&self) -> usize {
        match &self.screen {
            ModelScreen::Tab { .. } => self.tab_row_count(),
            ModelScreen::Reasoning { .. } => REASONING_EFFORTS.len(),
            ModelScreen::Kind { .. } => KindChoice::ALL.len(),
            ModelScreen::ModelInput { .. }
            | ModelScreen::Setup(_)
            | ModelScreen::ModelForm(_)
            | ModelScreen::ConfirmDelete { .. }
            | ModelScreen::Login { .. } => 0,
        }
    }

    pub(crate) fn setup(&self) -> Option<&ConnectionForm> {
        match &self.screen {
            ModelScreen::Setup(form) => Some(form),
            _ => None,
        }
    }

    pub(crate) fn busy(&self, operation: Option<ModelOperation>) -> bool {
        operation.is_some_and(|operation| operation.session == self.session)
    }

    /// Keep the current tab inside `0..=profiles.len()`, preferring the
    /// connection it showed before the profile list changed.
    ///
    /// A save that rewrites the connection list keeps the user on the
    /// connection they were editing, and a new connection lands on its own tab
    /// because the trailing add tab shifts one to the right.
    fn clamp_tab(&mut self, previous: Option<bone_app::ProfileId>) {
        if let Some(previous) = previous
            && let Some(index) = self
                .profiles
                .iter()
                .position(|profile| profile.id == previous)
        {
            self.tab = index;
            return;
        }
        self.tab = self.tab.min(self.profiles.len());
    }

    /// Keep the selected row inside the current tab.
    fn clamp_selection(&mut self) {
        let last = self.tab_row_count().saturating_sub(1);
        if let ModelScreen::Tab { selected } = &mut self.screen {
            *selected = (*selected).min(last);
        }
    }
}

impl ModelOperation {
    fn matches(self, session: Option<SessionId>, request: u64, kind: ModelOperationKind) -> bool {
        self.session == session && self.request == request && self.kind == kind
    }
}

pub(super) fn open_models(state: &mut UiState, effects: &mut Vec<Effect>) {
    let session = state.selected;
    state.status = None;
    replace(state, Overlay::Models(ModelPanel::new(session)), effects);
    if state.model_facts.is_none() {
        request_model_facts(state, effects);
    }
    load_models(state, session, effects);
}

pub(super) fn open_help(state: &mut UiState, effects: &mut Vec<Effect>) {
    replace(state, Overlay::Help, effects);
}

fn load_models(state: &mut UiState, session: Option<SessionId>, effects: &mut Vec<Effect>) {
    let request = state.generation();
    state.model_operation = Some(ModelOperation {
        session,
        request,
        kind: ModelOperationKind::Load,
    });
    effects.push(Effect::LoadModels { session, request });
}

pub(super) fn refresh_model_facts(state: &mut UiState, effects: &mut Vec<Effect>) {
    state.model_facts = None;
    request_model_facts(state, effects);
}

fn reconcile_model_facts(state: &mut UiState, effects: &mut Vec<Effect>) {
    request_model_facts(state, effects);
}

fn request_model_facts(state: &mut UiState, effects: &mut Vec<Effect>) {
    state.model_facts_request = state.generation();
    effects.push(Effect::LoadModelFacts {
        session: state.selected,
        request: state.model_facts_request,
    });
}

pub(super) fn model_facts_loaded(
    state: &mut UiState,
    session: Option<SessionId>,
    request: u64,
    facts: Option<ModelFacts>,
) {
    if state.selected != session || state.model_facts_request != request {
        return;
    }
    state.model_facts = facts;
}

pub(super) fn models_loaded(
    state: &mut UiState,
    session: Option<SessionId>,
    request: u64,
    choices: Vec<ModelChoice>,
    profiles: Vec<Profile>,
) {
    if !state
        .model_operation
        .is_some_and(|operation| operation.matches(session, request, ModelOperationKind::Load))
    {
        return;
    }
    state.model_operation = None;
    if state.selected != session {
        return;
    }
    let preferred = state
        .running_model()
        .or_else(|| {
            state
                .model_facts
                .as_ref()
                .and_then(|facts| facts.saved.as_ref().ok())
        })
        .map(|model| model.selection.clone());
    let Some(Overlay::Models(models)) = &mut state.overlay else {
        return;
    };
    if models.session != session {
        return;
    }
    let previous_tab = models.tab_profile().map(|profile| profile.id.clone());
    models.choices = choices;
    models.profiles = profiles;
    // A fresh open lands on the connection the current model belongs to. Every
    // later reload keeps the tab the user chose — including the add tab, whose
    // index shifts onto a connection the user just saved.
    if std::mem::take(&mut models.follow_current_model)
        && let Some(selection) = preferred.as_ref()
    {
        models.tab = models
            .profiles
            .iter()
            .position(|profile| profile.id == selection.profile)
            .unwrap_or(0);
    } else {
        models.clamp_tab(previous_tab);
    }
    let preferred_row = preferred.as_ref().and_then(|selection| {
        models
            .tab_models()
            .iter()
            .position(|candidate| same_model(candidate, selection))
    });
    let last_row = models.tab_row_count().saturating_sub(1);
    if let ModelScreen::Tab { selected } = &mut models.screen {
        *selected = preferred_row.unwrap_or_else(|| (*selected).min(last_row));
    }
}

pub(super) fn models_failed(
    state: &mut UiState,
    session: Option<SessionId>,
    request: u64,
    error: String,
) {
    if !state
        .model_operation
        .is_some_and(|operation| operation.matches(session, request, ModelOperationKind::Load))
    {
        return;
    }
    state.model_operation = None;
    if state.selected != session {
        return;
    }
    if let Some(Overlay::Models(models)) = &mut state.overlay
        && models.session == session
        && matches!(models.screen, ModelScreen::Tab { .. })
        && state.status.is_none()
    {
        state.status = Some(Status::panel_request(session, request, error));
    }
}

pub(super) fn model_applied(
    state: &mut UiState,
    session: Option<SessionId>,
    request: u64,
    facts: Option<ModelFacts>,
    error: Option<String>,
    effects: &mut Vec<Effect>,
) {
    let current = state
        .model_operation
        .is_some_and(|operation| operation.matches(session, request, ModelOperationKind::Apply));
    if !current {
        if state.selected == session || session.is_none() {
            reconcile_model_facts(state, effects);
        }
        return;
    }
    state.model_operation = None;
    if state.selected != session {
        if session.is_none() {
            reconcile_model_facts(state, effects);
        }
        return;
    }
    let matching_panel = matches!(
        &state.overlay,
        Some(Overlay::Models(models))
            if models.session == session
    );
    let failed = error.is_some();
    state.model_facts_request = state.generation();
    state.model_facts = facts;
    if matching_panel || state.overlay.is_none() {
        if let Some(error) = error {
            let message = format!("Model switch failed: {error}");
            state.status = Some(Status::panel_request(session, request, message.clone()));
            if let Some(Overlay::Models(models)) = &mut state.overlay
                && matches!(models.screen, ModelScreen::Login { .. })
            {
                models.screen = ModelScreen::Login {
                    request,
                    state: LoginState::Failed { message },
                };
            }
        } else if state
            .status
            .as_ref()
            .is_some_and(|status| status.belongs_to_panel_request(session, request))
        {
            state.status = None;
        }
    }
    if !failed && matching_panel {
        dismiss(state, effects);
    }
}

pub(super) fn connection_saved(
    state: &mut UiState,
    request: u64,
    session: Option<SessionId>,
    error: Option<String>,
    key_saved: bool,
    effects: &mut Vec<Effect>,
) {
    let Some(panel) = state.overlay.take() else {
        reconcile_connection_save(state, effects);
        return;
    };
    let Overlay::Models(mut models) = panel else {
        state.overlay = Some(panel);
        reconcile_connection_save(state, effects);
        return;
    };
    let pending_request = match &models.screen {
        ModelScreen::Setup(form) => form.pending_request,
        ModelScreen::ModelForm(form) => form.pending_request,
        _ => None,
    };
    let Some(pending_request) = pending_request else {
        state.overlay = Some(Overlay::Models(models));
        reconcile_connection_save(state, effects);
        return;
    };
    if state.selected != session || models.session != session || pending_request != request {
        state.overlay = Some(Overlay::Models(models));
        reconcile_connection_save(state, effects);
        return;
    }

    if let Some(error) = error {
        models.pending_selection = None;
        models.pending_reasoning = false;
        let (key_was_sent, is_connection) = match &mut models.screen {
            ModelScreen::Setup(form) => {
                form.pending_request = None;
                (form.key_was_sent, true)
            }
            ModelScreen::ModelForm(form) => {
                form.pending_request = None;
                (false, false)
            }
            _ => (false, false),
        };
        let message = if is_connection && key_was_sent && !key_saved {
            format!("{error} Re-enter the API key before retrying.")
        } else {
            error
        };
        if key_saved || !is_connection {
            if let ModelScreen::Setup(form) = &mut models.screen {
                form.key_was_sent = false;
            }
            state.overlay = Some(Overlay::Models(models));
            state.status = Some(Status::panel_request(session, request, message));
            return_to_models(state, effects);
            return;
        }
        state.status = Some(Status::panel_request(session, request, message));
        state.overlay = Some(Overlay::Models(models));
        reconcile_model_facts(state, effects);
        return;
    }

    let pending_selection = models.pending_selection.take();
    let pending_reasoning = models.pending_reasoning;
    models.pending_reasoning = false;
    if state
        .status
        .as_ref()
        .is_some_and(|status| status.belongs_to_panel_request(session, request))
    {
        state.status = None;
    }
    state.overlay = Some(Overlay::Models(models));
    if pending_reasoning && let Some(selection) = pending_selection {
        if let Some(Overlay::Models(models)) = &mut state.overlay {
            models.screen = ModelScreen::Reasoning {
                selected: reasoning_index(&selection),
                selection,
            };
        }
        return;
    }
    // The connection list just changed, so the strip is the only screen that
    // can still be trusted: it reloads and keeps the current tab in range.
    return_to_models(state, effects);
}

fn reconcile_connection_save(state: &mut UiState, effects: &mut Vec<Effect>) {
    let current = state.selected;
    if state.model_operation.is_none()
        && matches!(
            &state.overlay,
            Some(Overlay::Models(models))
                if models.session == current && matches!(models.screen, ModelScreen::Tab { .. })
        )
    {
        load_models(state, current, effects);
    }
    reconcile_model_facts(state, effects);
}

pub(super) fn login_changed(
    state: &mut UiState,
    request: u64,
    login: LoginState,
    effects: &mut Vec<Effect>,
) {
    let selected = state.selected;
    let Some(panel) = state.overlay.take() else {
        return;
    };
    let Overlay::Models(mut models) = panel else {
        state.overlay = Some(panel);
        return;
    };
    if models.session != selected {
        state.overlay = Some(Overlay::Models(models));
        return;
    }
    let ModelScreen::Login {
        request: current,
        state: current_state,
    } = &mut models.screen
    else {
        state.overlay = Some(Overlay::Models(models));
        return;
    };
    if *current != request {
        state.overlay = Some(Overlay::Models(models));
        return;
    }
    let succeeded = matches!(login, LoginState::Succeeded);
    *current_state = login;
    if succeeded {
        if state
            .status
            .as_ref()
            .is_some_and(|status| status.belongs_to_panel_request(selected, request))
        {
            state.status = None;
        }
        state.overlay = Some(Overlay::Models(models));
        // Signing in may have created the ChatGPT connection, so the strip
        // reloads and keeps the tab in range instead of trusting a stale list.
        return_to_models(state, effects);
    } else {
        state.overlay = Some(Overlay::Models(models));
    }
}

pub(super) fn open_objects(state: &mut UiState, effects: &mut Vec<Effect>) -> bool {
    let Some(ui) = state.selected_ui() else {
        state.status = Some(Status::selection(None, "There is no session to inspect"));
        return false;
    };
    let session = ui.id;
    let mut choices = Vec::new();
    if let Some(snapshot) = &ui.snapshot {
        for job in snapshot.jobs.iter().rev() {
            let goal: String = job.goal.chars().take(64).collect();
            choices.push((
                ReaderSource::Job(job.id),
                format!("Task {} · {goal}", job.id.id),
            ));
        }
    }
    for entry in ui.transcript.entries().rev() {
        let label = match &entry.event {
            bone_app::SessionEvent::ToolFinished { tool, .. } => {
                let tool: String = tool.chars().take(64).collect();
                format!("Tool {tool} · history {}", entry.sequence.0)
            }
            bone_app::SessionEvent::JobFinished { job, .. } => {
                format!("Task {} result · history {}", job.id, entry.sequence.0)
            }
            _ => continue,
        };
        choices.push((ReaderSource::History(entry.sequence), label));
    }
    state.status = None;
    replace(
        state,
        Overlay::Objects(ObjectPanel {
            session,
            choices,
            selected: 0,
        }),
        effects,
    );
    true
}

pub(super) fn select_object(state: &mut UiState, index: usize, effects: &mut Vec<Effect>) {
    let Some(Overlay::Objects(objects)) = &mut state.overlay else {
        return;
    };
    if index >= objects.choices.len() {
        return;
    }
    objects.selected = index;
    open_object(state, effects);
}

fn open_object(state: &mut UiState, effects: &mut Vec<Effect>) {
    let Some((session, source)) = (match &state.overlay {
        Some(Overlay::Objects(objects)) => objects
            .choices
            .get(objects.selected)
            .map(|(source, _)| (objects.session, *source)),
        _ => None,
    }) else {
        return;
    };
    if state.selected != Some(session) {
        state.status = Some(Status::selection(
            state.selected,
            "The session changed; reopen /details",
        ));
        return;
    }
    let content = state.selected_ui().and_then(|ui| match source {
        ReaderSource::Job(job) => ui
            .snapshot
            .as_ref()
            .and_then(|snapshot| ReaderContent::from_job(snapshot, job)),
        ReaderSource::History(sequence) => ui
            .transcript
            .find(sequence)
            .and_then(|entry| ReaderContent::from_history(session, entry)),
    });
    if let Some(content) = content {
        pin_reading(state);
        state.details = Some(ReaderState { content, scroll: 0 });
        dismiss(state, effects);
        state.status = None;
    } else {
        state.status = Some(Status::selection(
            state.selected,
            "This object is no longer loaded; reopen /details",
        ));
    }
}

pub(super) fn panel_previous(state: &mut UiState) {
    match &mut state.overlay {
        Some(Overlay::Objects(objects)) => {
            objects.selected = objects.selected.saturating_sub(1);
        }
        Some(Overlay::Models(ModelPanel {
            screen:
                ModelScreen::Tab { selected }
                | ModelScreen::Reasoning { selected, .. }
                | ModelScreen::Kind { selected },
            ..
        })) => *selected = selected.saturating_sub(1),
        _ => {}
    }
}

pub(super) fn panel_next(state: &mut UiState) {
    match &mut state.overlay {
        Some(Overlay::Objects(objects)) => {
            objects.selected = (objects.selected + 1).min(objects.choices.len().saturating_sub(1));
        }
        Some(Overlay::Models(models)) => {
            let row_count = models.row_count();
            match &mut models.screen {
                ModelScreen::Tab { selected }
                | ModelScreen::Reasoning { selected, .. }
                | ModelScreen::Kind { selected } => {
                    *selected = (*selected + 1).min(row_count.saturating_sub(1));
                }
                ModelScreen::ModelInput { .. }
                | ModelScreen::Setup(_)
                | ModelScreen::ModelForm(_)
                | ModelScreen::ConfirmDelete { .. }
                | ModelScreen::Login { .. } => {}
            }
        }
        _ => {}
    }
}

/// Move to the previous connection tab. Only the tab strip has a horizontal
/// axis, and the move clamps at the first tab instead of wrapping around.
pub(super) fn previous_tab(state: &mut UiState) {
    let Some(Overlay::Models(models)) = &mut state.overlay else {
        return;
    };
    if !matches!(models.screen, ModelScreen::Tab { .. }) {
        return;
    }
    models.tab = models.tab.min(models.profiles.len()).saturating_sub(1);
    models.clamp_selection();
}

/// Move to the next connection tab, clamping at the trailing add tab.
pub(super) fn next_tab(state: &mut UiState) {
    let Some(Overlay::Models(models)) = &mut state.overlay else {
        return;
    };
    if !matches!(models.screen, ModelScreen::Tab { .. }) {
        return;
    }
    models.tab = (models.tab + 1).min(models.profiles.len());
    models.clamp_selection();
}

/// Show the tab the user clicked, leaving whatever sub-screen was open.
pub(super) fn select_tab(state: &mut UiState, index: usize, effects: &mut Vec<Effect>) {
    let Some(panel) = state.overlay.take() else {
        return;
    };
    let Overlay::Models(mut models) = panel else {
        state.overlay = Some(panel);
        return;
    };
    if models.session != state.selected
        || models.busy(state.model_operation)
        || index > models.profiles.len()
    {
        state.overlay = Some(Overlay::Models(models));
        return;
    }
    let cancels_login = matches!(models.screen, ModelScreen::Login { .. });
    models.tab = index;
    models.screen = ModelScreen::Tab { selected: 0 };
    state.overlay = Some(Overlay::Models(models));
    state.status = None;
    if cancels_login {
        effects.push(Effect::CancelLogin);
    }
}

pub(super) fn activate(state: &mut UiState, effects: &mut Vec<Effect>) {
    match &state.overlay {
        Some(Overlay::Objects(objects)) => {
            let selected = objects.selected;
            select_object(state, selected, effects);
        }
        Some(Overlay::Models(ModelPanel {
            screen: ModelScreen::Tab { selected },
            ..
        })) => {
            let selected = *selected;
            select_model(state, selected, effects);
        }
        Some(Overlay::Models(ModelPanel {
            screen: ModelScreen::Reasoning { selected, .. },
            ..
        })) => {
            let selected = *selected;
            select_reasoning(state, selected, effects);
        }
        Some(Overlay::Models(ModelPanel {
            screen: ModelScreen::Kind { selected },
            ..
        })) => {
            let selected = *selected;
            choose_connection(state, selected, effects);
        }
        Some(Overlay::Models(ModelPanel {
            screen: ModelScreen::ModelInput { .. },
            ..
        })) => apply_model_input(state, effects),
        Some(Overlay::Models(ModelPanel {
            screen: ModelScreen::Setup(_) | ModelScreen::ModelForm(_),
            ..
        })) => save_connection(state, effects),
        Some(Overlay::Models(ModelPanel {
            screen:
                ModelScreen::Login {
                    state: LoginState::Failed { .. } | LoginState::Cancelled,
                    ..
                },
            ..
        })) => retry_login(state, effects),
        _ => {}
    }
}

pub(super) fn setup_text(state: &mut UiState, mut value: SecretText) {
    let Some(text) = setup_text_mut(state) else {
        return;
    };
    let previous_len = text.len();
    for ch in value.take().chars().filter(|ch| !ch.is_control()) {
        if text.len() + ch.len_utf8() <= 16 * 1024 {
            text.push(ch);
        }
    }
    if text.len() != previous_len {
        state.caret_visible = true;
    }
}

pub(super) fn setup_clear(state: &mut UiState) {
    if let Some(text) = setup_text_mut(state)
        && !text.is_empty()
    {
        text.clear();
        state.caret_visible = true;
    }
}

pub(super) fn setup_backspace(state: &mut UiState) {
    let Some(text) = setup_text_mut(state) else {
        return;
    };
    if let Some((byte, _)) = text.grapheme_indices(true).next_back() {
        text.truncate(byte);
        state.caret_visible = true;
    }
}

pub(super) fn move_setup_field(state: &mut UiState, forward: bool) {
    if let Some(form) = setup_mut(state) {
        let previous = form.field;
        form.move_field(forward);
        if form.field != previous {
            state.caret_visible = true;
        }
    }
}

fn setup_mut(state: &mut UiState) -> Option<&mut ConnectionForm> {
    match &mut state.overlay {
        Some(Overlay::Models(ModelPanel {
            screen: ModelScreen::Setup(form),
            ..
        })) if form.pending_request.is_none() => Some(form),
        _ => None,
    }
}

fn setup_text_mut(state: &mut UiState) -> Option<&mut String> {
    match &mut state.overlay {
        Some(Overlay::Models(ModelPanel {
            screen: ModelScreen::Setup(form),
            ..
        })) if form.pending_request.is_none() => Some(form.text_mut()),
        Some(Overlay::Models(ModelPanel {
            screen: ModelScreen::ModelForm(form),
            ..
        })) if form.pending_request.is_none() => Some(form.text_mut()),
        _ => None,
    }
}

/// The manual model identifier editor shares the connection form's
/// grapheme-safe, 16 KiB bounded editing. A model ID is not a secret, so it
/// needs no redaction and accepts a paste.
fn model_input_mut(state: &mut UiState) -> Option<&mut String> {
    match &mut state.overlay {
        Some(Overlay::Models(ModelPanel {
            screen: ModelScreen::ModelInput { value },
            ..
        })) => Some(value),
        _ => None,
    }
}

pub(super) fn model_text(state: &mut UiState, text: String) {
    let Some(value) = model_input_mut(state) else {
        return;
    };
    let previous_len = value.len();
    for ch in text.chars().filter(|ch| !ch.is_control()) {
        if value.len() + ch.len_utf8() <= 16 * 1024 {
            value.push(ch);
        }
    }
    if value.len() != previous_len {
        state.caret_visible = true;
    }
}

pub(super) fn model_backspace(state: &mut UiState) {
    let Some(value) = model_input_mut(state) else {
        return;
    };
    if let Some((byte, _)) = value.grapheme_indices(true).next_back() {
        value.truncate(byte);
        state.caret_visible = true;
    }
}

pub(super) fn model_clear(state: &mut UiState) {
    if let Some(value) = model_input_mut(state)
        && !value.is_empty()
    {
        value.clear();
        state.caret_visible = true;
    }
}

/// Apply the manually typed model identifier to the current tab's connection.
pub(super) fn apply_model_input(state: &mut UiState, effects: &mut Vec<Effect>) {
    let Some(panel) = state.overlay.take() else {
        return;
    };
    let Overlay::Models(models) = panel else {
        state.overlay = Some(panel);
        return;
    };
    if models.session != state.selected || models.busy(state.model_operation) {
        state.overlay = Some(Overlay::Models(models));
        return;
    }
    let ModelScreen::ModelInput { value } = &models.screen else {
        state.overlay = Some(Overlay::Models(models));
        return;
    };
    let model = value.trim().to_owned();
    if model.is_empty() {
        state.overlay = Some(Overlay::Models(models));
        state.status = Some(Status::selection(state.selected, "Enter a model ID"));
        return;
    }
    let Some(profile) = models.tab_profile().cloned() else {
        state.overlay = Some(Overlay::Models(models));
        state.status = Some(Status::selection(
            state.selected,
            "Choose a connection before entering a model",
        ));
        return;
    };
    let Ok(selection) = ModelSelection::new(profile.id.clone(), model) else {
        state.overlay = Some(Overlay::Models(models));
        state.status = Some(Status::selection(
            state.selected,
            "This model ID is not valid",
        ));
        return;
    };
    apply_selection(state, models, selection, false, effects);
}

/// Remove a model saved on the current tab's connection.
///
/// A catalog entry that was never saved on the connection has nothing to
/// remove, so the confirmation screen is only reachable for saved models.
pub(super) fn delete_model(state: &mut UiState, effects: &mut Vec<Effect>) {
    let Some(panel) = state.overlay.take() else {
        return;
    };
    let Overlay::Models(mut models) = panel else {
        state.overlay = Some(panel);
        return;
    };
    if models.session != state.selected || models.busy(state.model_operation) {
        state.overlay = Some(Overlay::Models(models));
        return;
    }
    let ModelScreen::Tab { selected } = models.screen else {
        state.overlay = Some(Overlay::Models(models));
        return;
    };
    let Some(profile) = models.tab_profile().cloned() else {
        state.overlay = Some(Overlay::Models(models));
        return;
    };
    let Some(selection) = models.tab_models().get(selected).cloned() else {
        state.overlay = Some(Overlay::Models(models));
        return;
    };
    if !profile.has_model(&selection.model) {
        state.overlay = Some(Overlay::Models(models));
        state.status = Some(Status::selection(
            state.selected,
            "Only saved models can be removed",
        ));
        return;
    }
    models.screen = ModelScreen::ModelForm(Box::new(ModelForm::remove(profile, selection.model)));
    state.overlay = Some(Overlay::Models(models));
    state.status = None;
    save_connection(state, effects);
}

pub(super) fn select_model(state: &mut UiState, index: usize, effects: &mut Vec<Effect>) {
    let Some(panel) = state.overlay.take() else {
        return;
    };
    let Overlay::Models(mut models) = panel else {
        state.overlay = Some(panel);
        return;
    };
    if models.session != state.selected || models.busy(state.model_operation) {
        state.overlay = Some(Overlay::Models(models));
        return;
    }
    match models.screen {
        ModelScreen::Tab { .. } => {
            if models.tab_is_add() {
                models.screen = ModelScreen::Kind { selected: 0 };
                state.overlay = Some(Overlay::Models(models));
                state.status = None;
                return;
            }
            let rows = models.tab_models();
            if index >= rows.len() {
                if index == rows.len() {
                    // The last row of every connection tab types a model ID
                    // the catalog does not publish.
                    models.screen = ModelScreen::ModelInput {
                        value: String::new(),
                    };
                    state.overlay = Some(Overlay::Models(models));
                    state.status = None;
                } else {
                    state.overlay = Some(Overlay::Models(models));
                }
                return;
            }
            let selection = rows[index].clone();
            models.screen = ModelScreen::Tab { selected: index };
            if supports_reasoning(&models, &selection) {
                models.screen = ModelScreen::Reasoning {
                    selected: reasoning_index(&selection),
                    selection,
                };
                state.overlay = Some(Overlay::Models(models));
                state.status = None;
            } else {
                apply_selection(state, models, selection, false, effects);
            }
        }
        _ => {
            state.overlay = Some(Overlay::Models(models));
        }
    }
}
pub(super) fn choose_connection(state: &mut UiState, index: usize, effects: &mut Vec<Effect>) {
    let Some(panel) = state.overlay.take() else {
        return;
    };
    let Overlay::Models(mut models) = panel else {
        state.overlay = Some(panel);
        return;
    };
    if models.session != state.selected || models.busy(state.model_operation) {
        state.overlay = Some(Overlay::Models(models));
        return;
    }
    let Some(choice) = KindChoice::ALL.get(index).copied() else {
        state.overlay = Some(Overlay::Models(models));
        return;
    };
    // A connection kind that is already saved has a tab: choosing it opens that
    // tab instead of asking for the same details a second time.
    if let Some(tab) = models
        .profiles
        .iter()
        .position(|profile| choice.already_connected(profile))
    {
        models.tab = tab;
        models.screen = ModelScreen::Tab { selected: 0 };
        state.overlay = Some(Overlay::Models(models));
        state.status = Some(Status::selection_notice(
            state.selected,
            format!("Already connected: {}", choice.label()),
        ));
        return;
    }
    match choice {
        KindChoice::ChatGpt => begin_login(state, models, effects),
        KindChoice::Api(kind) => {
            models.screen = ModelScreen::Setup(Box::new(ConnectionForm::new(kind)));
            state.overlay = Some(Overlay::Models(models));
            state.status = None;
        }
    }
}

/// Open the current connection for editing. Only the tab strip reacts, and the
/// add tab has nothing to edit.
pub(super) fn edit_connection(state: &mut UiState, effects: &mut Vec<Effect>) {
    let Some(panel) = state.overlay.take() else {
        return;
    };
    let Overlay::Models(mut models) = panel else {
        state.overlay = Some(panel);
        return;
    };
    if models.session != state.selected
        || models.busy(state.model_operation)
        || !matches!(models.screen, ModelScreen::Tab { .. })
    {
        state.overlay = Some(Overlay::Models(models));
        return;
    }
    let Some(profile) = models.tab_profile().cloned() else {
        state.overlay = Some(Overlay::Models(models));
        return;
    };
    if profile.id == bone_app::ProfileId::chatgpt() {
        begin_login(state, models, effects);
        return;
    }
    let Some(form) = ConnectionForm::edit_selection(&profile) else {
        state.overlay = Some(Overlay::Models(models));
        return;
    };
    models.screen = ModelScreen::Setup(Box::new(form));
    state.overlay = Some(Overlay::Models(models));
    state.status = None;
}

/// Ask for confirmation before deleting the current connection. Only the tab
/// strip reacts, and the add tab has nothing to delete.
pub(super) fn delete_connection(state: &mut UiState) {
    let Some(panel) = state.overlay.take() else {
        return;
    };
    let Overlay::Models(mut models) = panel else {
        state.overlay = Some(panel);
        return;
    };
    if models.session != state.selected
        || models.busy(state.model_operation)
        || !matches!(models.screen, ModelScreen::Tab { .. })
        || models.tab_profile().is_none()
    {
        state.overlay = Some(Overlay::Models(models));
        return;
    }
    let selected = match models.screen {
        ModelScreen::Tab { selected } => selected,
        _ => 0,
    };
    models.screen = ModelScreen::ConfirmDelete { selected };
    state.overlay = Some(Overlay::Models(models));
    state.status = None;
}

/// Delete the connection the user just confirmed.
///
/// The dialog closes immediately and the tab screen reports progress through
/// the pending operation, so a second confirmation cannot queue a second delete
/// while the first one is still in flight.
pub(super) fn confirm_delete_connection(state: &mut UiState, effects: &mut Vec<Effect>) {
    let Some(panel) = state.overlay.take() else {
        return;
    };
    let Overlay::Models(mut models) = panel else {
        state.overlay = Some(panel);
        return;
    };
    if models.session != state.selected
        || models.busy(state.model_operation)
        || !matches!(models.screen, ModelScreen::ConfirmDelete { .. })
    {
        state.overlay = Some(Overlay::Models(models));
        return;
    }
    let Some(profile) = models.tab_profile().cloned() else {
        models.screen = ModelScreen::Tab { selected: 0 };
        state.overlay = Some(Overlay::Models(models));
        return;
    };
    let session = models.session;
    let request = state.generation();
    models.screen = ModelScreen::Tab { selected: 0 };
    state.overlay = Some(Overlay::Models(models));
    state.status = None;
    state.model_operation = Some(ModelOperation {
        session,
        request,
        kind: ModelOperationKind::Delete,
    });
    effects.push(Effect::DeleteConnection {
        request,
        profile: profile.id,
    });
}

/// The result of [`Effect::DeleteConnection`].
///
/// The request alone decides whether this reply is current: a delete that
/// outlived a session switch must still clear its pending operation, or the
/// panel would stay busy forever. A stale reply changes nothing.
pub(super) fn connection_deleted(
    state: &mut UiState,
    request: u64,
    error: Option<String>,
    effects: &mut Vec<Effect>,
) {
    let current = state.model_operation.is_some_and(|operation| {
        operation.request == request && operation.kind == ModelOperationKind::Delete
    });
    if !current {
        return;
    }
    state.model_operation = None;
    if let Some(error) = error {
        state.status = Some(Status::panel_request(
            state.selected,
            request,
            format!("Connection was not deleted: {error}"),
        ));
        return;
    }
    if state
        .status
        .as_ref()
        .is_some_and(|status| status.belongs_to_panel_request(state.selected, request))
    {
        state.status = None;
    }
    // The deleted connection is gone from the strip; reload so the tab the
    // panel shows is a tab that exists.
    let session = match &mut state.overlay {
        Some(Overlay::Models(models)) if models.session == state.selected => {
            models.screen = ModelScreen::Tab { selected: 0 };
            Some(models.session)
        }
        _ => None,
    };
    if let Some(session) = session {
        load_models(state, session, effects);
    }
    reconcile_model_facts(state, effects);
}

pub(super) fn select_reasoning(state: &mut UiState, index: usize, effects: &mut Vec<Effect>) {
    let Some(panel) = state.overlay.take() else {
        return;
    };
    let Overlay::Models(mut models) = panel else {
        state.overlay = Some(panel);
        return;
    };
    if models.session != state.selected || models.busy(state.model_operation) {
        state.overlay = Some(Overlay::Models(models));
        return;
    }
    let ModelScreen::Reasoning { selection, .. } = &models.screen else {
        state.overlay = Some(Overlay::Models(models));
        return;
    };
    let Some(effort) = REASONING_EFFORTS.get(index).copied() else {
        state.overlay = Some(Overlay::Models(models));
        return;
    };
    let mut selection = selection.clone();
    selection.options = (effort != bone_app::ReasoningEffort::None).then(|| {
        bone_app::ModelOptions::OpenAiResponses {
            reasoning: bone_app::Reasoning::new().effort(effort),
        }
    });
    models.screen = ModelScreen::Reasoning {
        selected: index,
        selection: selection.clone(),
    };
    apply_selection(state, models, selection, false, effects);
}

fn same_model(left: &ModelSelection, right: &ModelSelection) -> bool {
    left.profile == right.profile && left.model == right.model
}

fn model_effort(selection: &ModelSelection) -> Option<bone_app::ReasoningEffort> {
    match selection.options.as_ref()? {
        bone_app::ModelOptions::OpenAiResponses { reasoning } => reasoning.effort_level(),
    }
}

fn supports_reasoning(models: &ModelPanel, selection: &ModelSelection) -> bool {
    models
        .profiles
        .iter()
        .find(|profile| profile.id == selection.profile)
        .is_some_and(|profile| {
            matches!(
                profile.endpoint,
                bone_app::EndpointConfig::ChatGptSubscription
                    | bone_app::EndpointConfig::OpenAiResponses { .. }
            )
        })
}

fn reasoning_index(selection: &ModelSelection) -> usize {
    model_effort(selection)
        .and_then(|effort| {
            REASONING_EFFORTS
                .iter()
                .position(|candidate| *candidate == effort)
        })
        .unwrap_or(0)
}

fn begin_login(state: &mut UiState, mut models: ModelPanel, effects: &mut Vec<Effect>) {
    let request = state.generation();
    models.screen = ModelScreen::Login {
        request,
        state: LoginState::Connecting,
    };
    state.overlay = Some(Overlay::Models(models));
    state.status = None;
    effects.push(Effect::Login {
        profile: bone_app::ProfileId::chatgpt(),
        request,
    });
}

fn retry_login(state: &mut UiState, effects: &mut Vec<Effect>) {
    let Some(panel) = state.overlay.take() else {
        return;
    };
    let Overlay::Models(models) = panel else {
        state.overlay = Some(panel);
        return;
    };
    begin_login(state, models, effects);
}

fn apply_selection(
    state: &mut UiState,
    models: ModelPanel,
    selection: ModelSelection,
    force: bool,
    effects: &mut Vec<Effect>,
) {
    let saved_is_selected = state
        .model_facts
        .as_ref()
        .and_then(|facts| facts.saved.as_ref().ok())
        .is_some_and(|current| current.selection == selection);
    let running_is_selected = state
        .running_model()
        .is_some_and(|current| current.selection == selection);
    if saved_is_selected && running_is_selected && !force {
        state.overlay = Some(Overlay::Models(models));
        dismiss(state, effects);
        return;
    }
    let session = models.session;
    let request = state.generation();
    state.model_operation = Some(ModelOperation {
        session,
        request,
        kind: ModelOperationKind::Apply,
    });
    state.overlay = Some(Overlay::Models(models));
    state.status = None;
    effects.push(Effect::SetModel {
        session,
        request,
        workspace_default: needs_workspace_default(state),
        selection,
    });
}

fn needs_workspace_default(state: &UiState) -> bool {
    matches!(
        state.model_facts.as_ref().map(|facts| &facts.saved),
        Some(Err(bone_app::ConfigProblem::NeedsModel))
    )
}

pub(super) fn save_connection(state: &mut UiState, effects: &mut Vec<Effect>) {
    let Some(panel) = state.overlay.take() else {
        return;
    };
    let Overlay::Models(mut models) = panel else {
        state.overlay = Some(panel);
        return;
    };
    if models.session != state.selected {
        state.overlay = Some(Overlay::Models(models));
        return;
    }
    let request = state.generation();
    let (profile, selection, key) = match &mut models.screen {
        ModelScreen::Setup(form) => {
            if form.pending_request.is_some() {
                state.overlay = Some(Overlay::Models(models));
                return;
            }
            let (profile, selection) = match form.validated() {
                Ok(value) => value,
                Err(error) => {
                    state.overlay = Some(Overlay::Models(models));
                    state.status = Some(Status::selection(state.selected, error));
                    return;
                }
            };
            form.pending_request = Some(request);
            form.key_was_sent = !form.key.is_empty();
            let key = (!form.key.is_empty()).then(|| SecretText::from(form.key.take()));
            (profile, selection, key)
        }
        ModelScreen::ModelForm(form) => {
            if form.pending_request.is_some() {
                state.overlay = Some(Overlay::Models(models));
                return;
            }
            let (profile, selection) = match form.validated() {
                Ok(value) => value,
                Err(error) => {
                    state.overlay = Some(Overlay::Models(models));
                    state.status = Some(Status::selection(state.selected, error));
                    return;
                }
            };
            form.pending_request = Some(request);
            (profile, selection, None)
        }
        _ => {
            state.overlay = Some(Overlay::Models(models));
            return;
        }
    };
    let pending_reasoning = selection.as_ref().is_some_and(|_| {
        matches!(
            &profile.endpoint,
            bone_app::EndpointConfig::OpenAiResponses { .. }
        )
    });
    if pending_reasoning {
        models.pending_selection = selection.clone();
        models.pending_reasoning = true;
    }
    let selection = if pending_reasoning { None } else { selection };
    let session = models.session;
    state.overlay = Some(Overlay::Models(models));
    state.status = None;
    effects.push(Effect::SaveConnection {
        request,
        session,
        workspace_default: needs_workspace_default(state),
        profile,
        key,
        selection,
    });
}

/// Leave a sub-screen for the tab strip of the same connection, reloading the
/// catalog and the saved facts alongside it.
fn return_to_models(state: &mut UiState, effects: &mut Vec<Effect>) {
    let Some(Overlay::Models(models)) = &mut state.overlay else {
        return;
    };
    models.screen = ModelScreen::Tab { selected: 0 };
    let session = models.session;
    load_models(state, session, effects);
    refresh_model_facts(state, effects);
}

/// Leave a sub-screen for the tab strip without scheduling any work.
fn show_tab(state: &mut UiState, selected: usize) {
    if let Some(Overlay::Models(models)) = &mut state.overlay {
        models.screen = ModelScreen::Tab { selected };
    }
}

pub(super) fn escape(state: &mut UiState, effects: &mut Vec<Effect>) -> bool {
    let Some(panel) = &state.overlay else {
        return false;
    };
    let apply_pending = matches!(
        (panel, state.model_operation),
        (
            Overlay::Models(models),
            Some(ModelOperation {
                kind: ModelOperationKind::Apply,
                ..
            })
        ) if models.session == state.selected
    );
    let save_pending = match panel {
        Overlay::Models(models) => match &models.screen {
            ModelScreen::Setup(form) => form.pending_request.is_some(),
            ModelScreen::ModelForm(form) => form.pending_request.is_some(),
            _ => false,
        },
        _ => false,
    };
    let delete_pending = matches!(
        (panel, state.model_operation),
        (
            Overlay::Models(models),
            Some(ModelOperation {
                kind: ModelOperationKind::Delete,
                ..
            })
        ) if models.session == state.selected
    );
    if apply_pending || save_pending || delete_pending {
        state.status = Some(Status::selection_notice(
            state.selected,
            "Finishing the model change…",
        ));
        return true;
    }
    state.overlay_scroll = 0;
    match panel {
        Overlay::Models(ModelPanel {
            screen: ModelScreen::Login { .. },
            ..
        }) => {
            effects.push(Effect::CancelLogin);
            state.status = None;
            return_to_models(state, effects);
        }
        Overlay::Models(ModelPanel {
            screen: ModelScreen::Reasoning { selection, .. },
            ..
        }) => {
            // Back to the row the reasoning picker was opened from.
            let selected = selection.clone();
            state.status = None;
            let row = match &state.overlay {
                Some(Overlay::Models(models)) => models.tab_row_of(&selected).unwrap_or(0),
                _ => 0,
            };
            show_tab(state, row);
        }
        Overlay::Models(ModelPanel {
            screen: ModelScreen::ModelInput { .. },
            ..
        }) => {
            // Back to the manual-input row the editor was opened from.
            let row = match &state.overlay {
                Some(Overlay::Models(models)) => models.tab_row_count().saturating_sub(1),
                _ => 0,
            };
            state.status = None;
            show_tab(state, row);
        }
        // Declining the delete returns to the row the user was on, so `d`
        // followed by `n` costs nothing.
        Overlay::Models(ModelPanel {
            screen: ModelScreen::ConfirmDelete { selected },
            ..
        }) => {
            let selected = *selected;
            state.status = None;
            show_tab(state, selected);
        }
        Overlay::Models(ModelPanel {
            screen: ModelScreen::Setup(_) | ModelScreen::ModelForm(_) | ModelScreen::Kind { .. },
            ..
        }) => {
            state.status = None;
            show_tab(state, 0);
        }
        Overlay::Models(ModelPanel {
            screen: ModelScreen::Tab { .. },
            ..
        })
        | Overlay::Objects(_)
        | Overlay::Help => dismiss(state, effects),
    }
    true
}

pub(super) fn dismiss_session(state: &mut UiState, effects: &mut Vec<Effect>) {
    let belongs_to_selected = match &state.overlay {
        Some(Overlay::Objects(objects)) => Some(objects.session) == state.selected,
        Some(Overlay::Models(models)) => models.session == state.selected,
        _ => false,
    };
    if belongs_to_selected {
        dismiss(state, effects);
    }
}

pub(super) fn dismiss(state: &mut UiState, effects: &mut Vec<Effect>) {
    cancel_login(state, effects);
    state.overlay = None;
    state.overlay_scroll = 0;
    state.leave_overlay();
}

fn replace(state: &mut UiState, panel: Overlay, effects: &mut Vec<Effect>) {
    cancel_login(state, effects);
    state.overlay = Some(panel);
    state.overlay_scroll = 0;
}

fn cancel_login(state: &UiState, effects: &mut Vec<Effect>) {
    if matches!(
        &state.overlay,
        Some(Overlay::Models(ModelPanel {
            screen: ModelScreen::Login { .. },
            ..
        }))
    ) {
        effects.push(Effect::CancelLogin);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Action, ConnectionKind, SetupField, UiEvent, WorkspaceTarget, update};

    /// A connection whose endpoint never claims a reasoning picker, so a row
    /// click applies the model directly.
    fn profile_with_id(id: &str) -> Profile {
        Profile::new(
            bone_app::ProfileId::new(id).unwrap(),
            id,
            bone_app::EndpointConfig::OpenAiChatCompletions {
                base_url: Some(format!("https://{id}.example.test/v1")),
            },
        )
        .unwrap()
    }

    fn choice_for(profile: &Profile, model: &str) -> ModelChoice {
        ModelChoice {
            selection: bone_app::ModelSelection::new(profile.id.clone(), model).unwrap(),
            profile_label: profile.label.clone(),
            label: model.into(),
        }
    }

    fn choice(model: &str) -> ModelChoice {
        choice_for(&profile_with_id("test-api"), model)
    }

    /// The connections a choice list belongs to, one per profile id.
    fn profiles_of(choices: &[ModelChoice]) -> Vec<Profile> {
        let mut profiles: Vec<Profile> = Vec::new();
        for choice in choices {
            if profiles
                .iter()
                .any(|profile| profile.id == choice.selection.profile)
            {
                continue;
            }
            profiles.push(profile_with_id(choice.selection.profile.as_str()));
        }
        profiles
    }

    fn facts(model: &str) -> ModelFacts {
        ModelFacts {
            saved: Ok(bone_app::ResolvedModel {
                selection: bone_app::ModelSelection::new(bone_app::ProfileId::chatgpt(), model)
                    .unwrap(),
                profile: bone_app::Profile::chatgpt(),
            }),
            running: None,
        }
    }

    fn choice_facts(model: &str, running: bool) -> ModelFacts {
        let selection =
            bone_app::ModelSelection::new(bone_app::ProfileId::new("test-api").unwrap(), model)
                .unwrap();
        let resolved = bone_app::ResolvedModel {
            selection,
            profile: bone_app::Profile::new(
                bone_app::ProfileId::new("test-api").unwrap(),
                "Test API",
                bone_app::EndpointConfig::OpenAiResponses {
                    base_url: Some("https://example.test/v1".into()),
                },
            )
            .unwrap(),
        };
        ModelFacts {
            saved: Ok(resolved.clone()),
            running: running.then_some(resolved),
        }
    }

    fn request(state: &UiState, kind: ModelOperationKind) -> u64 {
        let operation = state.model_operation.expect("active model operation");
        assert_eq!(operation.kind, kind);
        operation.request
    }

    fn panel(state: &UiState) -> &ModelPanel {
        match &state.overlay {
            Some(Overlay::Models(models)) => models,
            _ => panic!("model panel"),
        }
    }

    fn screen(state: &UiState) -> &ModelScreen {
        &panel(state).screen
    }

    fn ready_panel(state: &mut UiState, profiles: Vec<Profile>, choices: Vec<ModelChoice>) -> u64 {
        if state.model_facts.is_none() {
            state.model_facts = Some(facts("existing"));
        }
        let mut effects = Vec::new();
        open_models(state, &mut effects);
        let load = request(state, ModelOperationKind::Load);
        assert!(matches!(
            effects.as_slice(),
            [Effect::LoadModels { request, .. }] if *request == load
        ));
        models_loaded(state, state.selected, load, choices, profiles);
        assert!(state.model_operation.is_none());
        load
    }

    fn ready_models(state: &mut UiState, choices: Vec<ModelChoice>) -> u64 {
        let profiles = profiles_of(&choices);
        ready_panel(state, profiles, choices)
    }

    fn setup_panel(state: &mut UiState, form: ConnectionForm) {
        let mut models = ModelPanel::new(state.selected);
        models.screen = ModelScreen::Setup(Box::new(form));
        state.overlay = Some(Overlay::Models(models));
    }

    #[test]
    fn a_saved_model_is_removed_from_its_tab_and_the_strip_reloads() {
        let mut profile = profile_with_id("custom-api");
        profile.add_model("model-a").unwrap();
        let mut state = UiState::default();
        let mut models = ModelPanel::new(None);
        models.profiles = vec![profile.clone()];
        models.choices = vec![choice_for(&profile, "model-a")];
        models.screen = ModelScreen::Tab { selected: 0 };
        state.overlay = Some(Overlay::Models(models));
        let mut effects = Vec::new();

        delete_model(&mut state, &mut effects);

        let Some(Overlay::Models(ModelPanel {
            screen: ModelScreen::ModelForm(form),
            ..
        })) = state.overlay.as_mut()
        else {
            panic!("removal should use the model form")
        };
        assert_eq!(form.model, "model-a");
        let request = form.pending_request.expect("remove request");
        connection_saved(&mut state, request, None, None, false, &mut effects);

        assert!(matches!(screen(&state), ModelScreen::Tab { selected: 0 }));
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::LoadModels { .. }))
        );
    }

    #[test]
    fn a_model_the_connection_never_saved_cannot_be_removed() {
        let profile = profile_with_id("custom-api");
        let mut state = UiState::default();
        let mut models = ModelPanel::new(None);
        models.profiles = vec![profile.clone()];
        models.choices = vec![choice_for(&profile, "catalog-only")];
        models.screen = ModelScreen::Tab { selected: 0 };
        state.overlay = Some(Overlay::Models(models));
        let mut effects = Vec::new();

        delete_model(&mut state, &mut effects);

        assert!(effects.is_empty());
        assert!(matches!(screen(&state), ModelScreen::Tab { selected: 0 }));
        assert_eq!(
            state.status_text(),
            Some("Only saved models can be removed")
        );
    }

    #[test]
    fn the_tab_strip_walks_connections_and_clamps_at_the_add_tab() {
        let first = profile_with_id("first");
        let second = profile_with_id("second");
        let mut state = UiState::default();
        ready_panel(
            &mut state,
            vec![first, second],
            vec![choice_for(&profile_with_id("second"), "model-a")],
        );

        assert_eq!(panel(&state).tab, 0);
        next_tab(&mut state);
        assert_eq!(panel(&state).tab, 1);
        assert_eq!(
            panel(&state)
                .tab_profile()
                .map(|profile| profile.id.clone()),
            Some(bone_app::ProfileId::new("second").unwrap())
        );
        next_tab(&mut state);
        assert!(panel(&state).tab_is_add());
        next_tab(&mut state);
        assert!(panel(&state).tab_is_add());
        previous_tab(&mut state);
        previous_tab(&mut state);
        assert_eq!(panel(&state).tab, 0);
        previous_tab(&mut state);
        assert_eq!(panel(&state).tab, 0);
    }

    #[test]
    fn a_reload_keeps_the_tab_inside_the_strip_and_lands_a_new_connection_on_its_own_tab() {
        let first = profile_with_id("first");
        let second = profile_with_id("second");
        let mut state = UiState::default();
        let mut effects = Vec::new();
        ready_panel(&mut state, vec![first.clone()], vec![]);

        // Saving from the add tab shifts the new connection under the cursor
        // instead of leaving it on the trailing tab.
        models_tab(&mut state, 1);
        load_models(&mut state, None, &mut effects);
        let load = request(&state, ModelOperationKind::Load);
        models_loaded(
            &mut state,
            None,
            load,
            vec![choice_for(&second, "model-a")],
            vec![first.clone(), second.clone()],
        );
        assert_eq!(
            panel(&state)
                .tab_profile()
                .map(|profile| profile.id.clone()),
            Some(second.id.clone())
        );

        // Deleting the connection the strip showed moves the tab back inside
        // the remaining tabs.
        models_tab(&mut state, 1);
        load_models(&mut state, None, &mut effects);
        let load = request(&state, ModelOperationKind::Load);
        models_loaded(&mut state, None, load, vec![], vec![first]);
        assert!(panel(&state).tab_is_add());
        assert_eq!(panel(&state).tab, 1);
    }

    fn models_tab(state: &mut UiState, tab: usize) {
        let Some(Overlay::Models(models)) = &mut state.overlay else {
            panic!("model panel")
        };
        models.tab = tab;
    }

    #[test]
    fn escape_leaves_every_sub_screen_for_the_tab_strip() {
        let profile = profile_with_id("custom-api");
        let mut state = UiState::default();
        let mut models = ModelPanel::new(None);
        models.profiles = vec![profile.clone()];
        models.choices = vec![
            choice_for(&profile, "model-a"),
            choice_for(&profile, "model-b"),
        ];
        models.tab = 0;
        models.screen = ModelScreen::ConfirmDelete { selected: 1 };
        state.overlay = Some(Overlay::Models(models));
        let mut effects = Vec::new();

        assert!(escape(&mut state, &mut effects));
        assert!(effects.is_empty());
        assert!(matches!(screen(&state), ModelScreen::Tab { selected: 1 }));

        models_screen(&mut state, ModelScreen::Kind { selected: 3 });
        assert!(escape(&mut state, &mut effects));
        assert!(matches!(screen(&state), ModelScreen::Tab { selected: 0 }));

        models_screen(&mut state, ModelScreen::ModelInput { value: "x".into() });
        assert!(escape(&mut state, &mut effects));
        assert!(matches!(screen(&state), ModelScreen::Tab { selected: 2 }));

        models_screen(
            &mut state,
            ModelScreen::Reasoning {
                selected: 0,
                selection: bone_app::ModelSelection::new(profile.id.clone(), "model-b").unwrap(),
            },
        );
        assert!(escape(&mut state, &mut effects));
        assert!(matches!(screen(&state), ModelScreen::Tab { selected: 1 }));

        // Only the tab strip itself dismisses the panel.
        assert!(escape(&mut state, &mut effects));
        assert!(state.overlay.is_none());
    }

    fn models_screen(state: &mut UiState, screen: ModelScreen) {
        let Some(Overlay::Models(models)) = &mut state.overlay else {
            panic!("model panel")
        };
        models.screen = screen;
    }

    #[test]
    fn a_confirmed_delete_records_its_request_and_closes_the_dialog() {
        let profile = profile_with_id("only");
        let mut state = UiState::default();
        let mut models = ModelPanel::new(None);
        models.profiles = vec![profile.clone()];
        models.screen = ModelScreen::Tab { selected: 0 };
        state.overlay = Some(Overlay::Models(models));
        let mut effects = Vec::new();

        delete_connection(&mut state);
        assert!(matches!(screen(&state), ModelScreen::ConfirmDelete { .. }));
        confirm_delete_connection(&mut state, &mut effects);

        let [
            Effect::DeleteConnection {
                request,
                profile: deleted,
            },
        ] = effects.as_slice()
        else {
            panic!("delete effect")
        };
        assert_eq!(*deleted, profile.id);
        assert!(matches!(screen(&state), ModelScreen::Tab { selected: 0 }));
        assert_eq!(
            state.model_operation,
            Some(ModelOperation {
                session: None,
                request: *request,
                kind: ModelOperationKind::Delete,
            })
        );

        // A second confirmation finds no dialog, and Escape cannot close the
        // panel out from under the delete that is still in flight.
        let mut second = Vec::new();
        confirm_delete_connection(&mut state, &mut second);
        assert!(second.is_empty());
        assert!(escape(&mut state, &mut second));
        assert!(second.is_empty());
        assert!(state.overlay.is_some());
        assert_eq!(state.status_text(), Some("Finishing the model change…"));

        // The reply clears the operation even when it arrives late: a stale
        // request changes nothing, and the current one clears the delete flag
        // so the panel is never permanently busy.
        connection_deleted(&mut state, request.wrapping_add(1), None, &mut second);
        assert_eq!(
            state.model_operation.map(|operation| operation.kind),
            Some(ModelOperationKind::Delete)
        );
        connection_deleted(&mut state, *request, None, &mut second);
        assert_ne!(
            state.model_operation.map(|operation| operation.kind),
            Some(ModelOperationKind::Delete)
        );
        assert!(
            second
                .iter()
                .any(|effect| matches!(effect, Effect::LoadModels { .. }))
        );
    }

    #[test]
    fn a_failed_delete_stays_visible_and_leaves_the_panel_usable() {
        let profile = profile_with_id("only");
        let mut state = UiState::default();
        let mut models = ModelPanel::new(None);
        models.profiles = vec![profile];
        models.screen = ModelScreen::Tab { selected: 0 };
        state.overlay = Some(Overlay::Models(models));
        let mut effects = Vec::new();
        delete_connection(&mut state);
        confirm_delete_connection(&mut state, &mut effects);
        let [Effect::DeleteConnection { request, .. }] = effects.as_slice() else {
            panic!("delete effect")
        };
        let request = *request;

        connection_deleted(
            &mut state,
            request,
            Some("keychain unavailable".into()),
            &mut Vec::new(),
        );

        assert!(state.model_operation.is_none());
        assert_eq!(
            state.status_text(),
            Some("Connection was not deleted: keychain unavailable")
        );
        assert!(matches!(screen(&state), ModelScreen::Tab { selected: 0 }));
        // The panel is no longer busy, so the strip moves again.
        next_tab(&mut state);
        assert!(panel(&state).tab_is_add());
    }

    #[test]
    fn the_manual_model_editor_belongs_to_the_current_tab() {
        let first = profile_with_id("first");
        let second = profile_with_id("second");
        let mut state = UiState::default();
        let mut models = ModelPanel::new(None);
        models.profiles = vec![first.clone(), second.clone()];
        models.choices = vec![
            choice_for(&first, "model-a"),
            choice_for(&second, "model-b"),
        ];
        models.tab = 1;
        models.screen = ModelScreen::Tab { selected: 1 };
        state.overlay = Some(Overlay::Models(models));
        let mut effects = Vec::new();

        // Row 1 is the manual row of the second connection, not its neighbour's
        // model.
        select_model(&mut state, 1, &mut effects);
        assert!(matches!(
            screen(&state),
            ModelScreen::ModelInput { value } if value.is_empty()
        ));

        model_text(&mut state, "  manual-model  ".into());
        model_backspace(&mut state);
        assert!(matches!(
            screen(&state),
            ModelScreen::ModelInput { value } if value == "  manual-model "
        ));
        model_clear(&mut state);
        apply_model_input(&mut state, &mut effects);
        assert!(effects.is_empty());
        assert_eq!(state.status_text(), Some("Enter a model ID"));

        models_screen(
            &mut state,
            ModelScreen::ModelInput {
                value: " manual-model ".into(),
            },
        );
        apply_model_input(&mut state, &mut effects);
        let [Effect::SetModel { selection, .. }] = effects.as_slice() else {
            panic!("manual input must apply a model")
        };
        assert_eq!(selection.profile, second.id);
        assert_eq!(selection.model, "manual-model");
    }

    #[test]
    fn the_add_tab_has_no_models_and_its_last_row_opens_the_kind_picker() {
        let first = profile_with_id("first");
        let mut state = UiState::default();
        let mut models = ModelPanel::new(None);
        models.profiles = vec![first.clone()];
        models.choices = vec![choice_for(&first, "model-a")];
        models.tab = 1;
        models.screen = ModelScreen::Tab { selected: 0 };
        state.overlay = Some(Overlay::Models(models));

        assert!(panel(&state).tab_is_add());
        assert!(panel(&state).tab_models().is_empty());
        assert_eq!(panel(&state).tab_row_count(), 1);

        let mut effects = Vec::new();
        select_model(&mut state, 0, &mut effects);
        assert!(matches!(screen(&state), ModelScreen::Kind { selected: 0 }));
        assert!(effects.is_empty());
    }

    #[test]
    fn a_stale_connection_never_shows_a_manual_model_from_another_connection() {
        let first = profile_with_id("first");
        let mut second = profile_with_id("second");
        second.add_model("manual-model").unwrap();
        let mut state = UiState::default();
        let mut models = ModelPanel::new(None);
        models.profiles = vec![first.clone(), second.clone()];
        models.choices = vec![choice_for(&first, "catalog-model")];
        models.tab = 0;
        state.overlay = Some(Overlay::Models(models));

        // The catalog did not publish `manual-model`; the connection did, so it
        // stays visible on its own tab and nowhere else.
        assert_eq!(
            panel(&state)
                .tab_models()
                .iter()
                .map(|selection| selection.model.clone())
                .collect::<Vec<_>>(),
            vec!["catalog-model".to_owned()]
        );
        models_tab(&mut state, 1);
        assert_eq!(
            panel(&state)
                .tab_models()
                .iter()
                .map(|selection| selection.model.clone())
                .collect::<Vec<_>>(),
            vec!["manual-model".to_owned()]
        );
    }

    #[test]
    fn adding_a_connection_from_the_add_tab_returns_to_the_tab_strip() {
        let mut state = UiState::default();
        let mut models = ModelPanel::new(None);
        models.screen = ModelScreen::Tab { selected: 0 };
        state.overlay = Some(Overlay::Models(models));
        let mut effects = Vec::new();
        select_model(&mut state, 0, &mut effects);
        assert!(matches!(screen(&state), ModelScreen::Kind { selected: 0 }));

        choose_connection(&mut state, 1, &mut effects);
        let Some(Overlay::Models(ModelPanel {
            screen: ModelScreen::Setup(form),
            ..
        })) = state.overlay.as_mut()
        else {
            panic!("connection form")
        };
        form.key = SecretText::from("secret".to_owned());
        save_connection(&mut state, &mut effects);
        let request = effects.iter().find_map(|effect| {
            if let Effect::SaveConnection { request, .. } = effect {
                Some(*request)
            } else {
                None
            }
        });
        connection_saved(
            &mut state,
            request.expect("save request"),
            None,
            None,
            true,
            &mut effects,
        );
        assert!(matches!(screen(&state), ModelScreen::Tab { selected: 0 }));
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::LoadModels { .. }))
        );
    }

    #[test]
    fn each_panel_instance_owns_only_its_legal_navigation_state() {
        let session = SessionId::new();
        let mut state = UiState::default();
        state.overlay = Some(Overlay::Objects(ObjectPanel {
            session,
            choices: vec![
                (ReaderSource::History(SessionSeq(1)), "one".into()),
                (ReaderSource::History(SessionSeq(2)), "two".into()),
            ],
            selected: 1,
        }));
        panel_previous(&mut state);
        assert!(matches!(
            &state.overlay,
            Some(Overlay::Objects(objects)) if objects.selected == 0
        ));

        let content = ReaderContent {
            session,
            source: ReaderSource::History(SessionSeq(1)),
            title: "reader".into(),
            text: "body".into(),
            numbered: Vec::new(),
            layout_cache: std::cell::RefCell::new(None),
        };
        state.details = Some(ReaderState { content, scroll: 7 });
        panel_next(&mut state);
        super::super::details::scroll_reader(&mut state, -2, 20);
        assert!(matches!(
            &state.details,
            Some(reader) if reader.scroll == 5
        ));

        let mut models = ModelPanel::new(None);
        models.screen = ModelScreen::Kind { selected: 3 };
        state.overlay = Some(Overlay::Models(models));
        panel_previous(&mut state);
        assert!(matches!(
            &state.overlay,
            Some(Overlay::Models(ModelPanel {
                screen: ModelScreen::Kind { selected: 2 },
                ..
            }))
        ));
    }

    #[test]
    fn closing_a_panel_never_rewrites_workspace_focus() {
        let mut state = UiState::default();
        state.set_workspace_target(WorkspaceTarget::Sessions);
        open_help(&mut state, &mut Vec::new());
        dismiss(&mut state, &mut Vec::new());
        assert!(state.overlay.is_none());
        assert_eq!(state.workspace_target(), WorkspaceTarget::Sessions);
        assert_eq!(state.last_center_target(), WorkspaceTarget::Composer);
    }

    #[test]
    fn replacement_cancels_login_once_before_starting_new_panel_work() {
        let mut state = UiState::default();
        let mut models = ModelPanel::new(None);
        models.screen = ModelScreen::Login {
            request: 41,
            state: LoginState::Connecting,
        };
        state.overlay = Some(Overlay::Models(models));
        let mut effects = Vec::new();

        open_models(&mut state, &mut effects);

        assert!(matches!(
            effects.as_slice(),
            [
                Effect::CancelLogin,
                Effect::LoadModelFacts { session: None, .. },
                Effect::LoadModels { session: None, .. }
            ]
        ));
        assert!(matches!(
            &state.overlay,
            Some(Overlay::Models(ModelPanel {
                screen: ModelScreen::Tab { .. },
                ..
            }))
        ));

        effects.clear();
        open_help(&mut state, &mut effects);
        assert!(effects.is_empty());
        assert!(matches!(state.overlay, Some(Overlay::Help)));
    }

    #[test]
    fn stale_load_receipts_cannot_finish_or_mutate_a_reopened_panel() {
        let mut state = UiState::default();
        let mut effects = Vec::new();
        open_models(&mut state, &mut effects);
        let old = request(&state, ModelOperationKind::Load);
        dismiss(&mut state, &mut effects);
        open_models(&mut state, &mut effects);
        let current = request(&state, ModelOperationKind::Load);
        assert_ne!(old, current);
        state.status = Some("new panel status".into());

        models_loaded(&mut state, None, old, vec![choice("stale")], vec![]);
        models_failed(&mut state, None, old, "stale failure".into());

        assert_eq!(request(&state, ModelOperationKind::Load), current);
        assert_eq!(state.status_text(), Some("new panel status"));
        assert!(matches!(
            &state.overlay,
            Some(Overlay::Models(models)) if models.choices.is_empty()
        ));

        models_loaded(&mut state, None, current, vec![choice("fresh")], vec![]);
        assert!(state.model_operation.is_none());
        assert!(matches!(
            &state.overlay,
            Some(Overlay::Models(models))
                if models.choices.first().unwrap().selection.model == "fresh"
        ));
    }

    #[test]
    fn missing_model_always_opens_the_model_list_regardless_of_receipt_order() {
        let mut state = UiState::default();
        let mut effects = Vec::new();
        open_models(&mut state, &mut effects);
        let load = request(&state, ModelOperationKind::Load);
        let facts_request = state.model_facts_request;

        models_loaded(&mut state, None, load, vec![], vec![Profile::chatgpt()]);
        assert!(matches!(
            state.overlay,
            Some(Overlay::Models(ModelPanel {
                screen: ModelScreen::Tab { .. },
                ..
            }))
        ));

        model_facts_loaded(
            &mut state,
            None,
            facts_request,
            Some(ModelFacts {
                saved: Err(bone_app::ConfigProblem::NeedsModel),
                running: None,
            }),
        );
        assert!(matches!(
            state.overlay,
            Some(Overlay::Models(ModelPanel {
                screen: ModelScreen::Tab { .. },
                ..
            }))
        ));

        let mut state = UiState::default();
        let mut effects = Vec::new();
        open_models(&mut state, &mut effects);
        let load = request(&state, ModelOperationKind::Load);
        let facts_request = state.model_facts_request;

        model_facts_loaded(
            &mut state,
            None,
            facts_request,
            Some(ModelFacts {
                saved: Err(bone_app::ConfigProblem::NeedsModel),
                running: None,
            }),
        );
        assert!(matches!(
            state.overlay,
            Some(Overlay::Models(ModelPanel {
                screen: ModelScreen::Tab { .. },
                ..
            }))
        ));

        models_loaded(&mut state, None, load, vec![], vec![Profile::chatgpt()]);
        assert!(matches!(
            state.overlay,
            Some(Overlay::Models(ModelPanel {
                screen: ModelScreen::Tab { .. },
                ..
            }))
        ));
        assert!(state.status.is_none());
    }

    #[test]
    fn model_list_opens_on_the_applied_choice() {
        let mut state = UiState::default();
        state.model_facts = Some(choice_facts("second", true));
        ready_models(&mut state, vec![choice("first"), choice("second")]);

        assert!(matches!(
            state.overlay,
            Some(Overlay::Models(ModelPanel {
                screen: ModelScreen::Tab { selected: 1 },
                ..
            }))
        ));
    }

    #[test]
    fn selecting_a_responses_model_opens_reasoning_before_apply() {
        let mut state = UiState::default();
        state.model_facts = Some(facts("existing"));
        let mut effects = Vec::new();
        open_models(&mut state, &mut effects);
        let load = request(&state, ModelOperationKind::Load);
        let selection =
            bone_app::ModelSelection::new(bone_app::ProfileId::chatgpt(), "gpt-5.6-terra").unwrap();
        models_loaded(
            &mut state,
            None,
            load,
            vec![ModelChoice {
                selection,
                profile_label: "ChatGPT".into(),
                label: "GPT-5.6 Terra".into(),
            }],
            vec![bone_app::Profile::chatgpt()],
        );

        effects.clear();
        select_model(&mut state, 0, &mut effects);
        assert!(effects.is_empty());
        assert!(matches!(
            state.overlay,
            Some(Overlay::Models(ModelPanel {
                screen: ModelScreen::Reasoning { selected: 0, .. },
                ..
            }))
        ));
        select_reasoning(&mut state, 3, &mut effects);
        let [Effect::SetModel { selection, .. }] = effects.as_slice() else {
            panic!("reasoning choice must apply a complete selection")
        };
        assert_eq!(
            model_effort(selection),
            Some(bone_app::ReasoningEffort::Medium)
        );
    }

    #[test]
    fn saving_a_connection_with_a_responses_model_chooses_reasoning_first() {
        let mut state = UiState::default();
        let mut form = ConnectionForm::new(ConnectionKind::CustomOpenAiResponses);
        form.base_url = "http://127.0.0.1:11434/v1".into();
        form.model = "local-model".into();
        setup_panel(&mut state, form);

        let mut effects = Vec::new();
        save_connection(&mut state, &mut effects);
        let request = effects
            .iter()
            .find_map(|effect| match effect {
                Effect::SaveConnection { request, .. } => Some(*request),
                _ => None,
            })
            .expect("save request");
        connection_saved(&mut state, request, None, None, false, &mut effects);

        assert!(matches!(screen(&state), ModelScreen::Reasoning { .. }));
        effects.clear();
        select_reasoning(&mut state, 2, &mut effects);
        assert!(matches!(
            effects.as_slice(),
            [Effect::SetModel { selection, .. }]
                if selection.model == "local-model"
                    && model_effort(selection) == Some(bone_app::ReasoningEffort::Low)
        ));
    }

    #[test]
    fn first_use_selects_a_model_then_reasoning_from_the_list() {
        let mut state = UiState::default();
        state.model_facts = Some(ModelFacts {
            saved: Err(bone_app::ConfigProblem::NeedsModel),
            running: None,
        });
        let mut effects = Vec::new();
        open_models(&mut state, &mut effects);
        let load = request(&state, ModelOperationKind::Load);
        models_loaded(
            &mut state,
            None,
            load,
            vec![ModelChoice {
                selection: bone_app::ModelSelection::new(
                    bone_app::ProfileId::chatgpt(),
                    "gpt-5.6-terra",
                )
                .unwrap(),
                profile_label: "ChatGPT".into(),
                label: "GPT-5.6 Terra".into(),
            }],
            vec![bone_app::Profile::chatgpt()],
        );
        assert!(matches!(
            state.overlay,
            Some(Overlay::Models(ModelPanel {
                screen: ModelScreen::Tab { selected: 0 },
                ..
            }))
        ));

        effects.clear();
        activate(&mut state, &mut effects);
        assert!(effects.is_empty());
        assert!(matches!(
            state.overlay,
            Some(Overlay::Models(ModelPanel {
                screen: ModelScreen::Reasoning { selected: 0, .. },
                ..
            }))
        ));
        activate(&mut state, &mut effects);
        assert!(matches!(
            effects.as_slice(),
            [Effect::SetModel {
                workspace_default: true,
                selection,
                ..
            }] if selection.options.is_none()
        ));
    }

    #[test]
    fn load_and_apply_have_distinct_identity_and_busy_blocks_repeat_apply() {
        let mut state = UiState::default();
        let load = ready_models(&mut state, vec![choice("gpt-test")]);
        let mut effects = Vec::new();
        select_model(&mut state, 0, &mut effects);
        let apply = request(&state, ModelOperationKind::Apply);
        assert_ne!(load, apply);
        assert!(matches!(
            effects.as_slice(),
            [Effect::SetModel {
                request,
                workspace_default: false,
                ..
            }] if *request == apply
        ));
        effects.clear();
        select_model(&mut state, 0, &mut effects);
        activate(&mut state, &mut effects);
        assert!(effects.is_empty());
        assert_eq!(request(&state, ModelOperationKind::Apply), apply);
    }

    #[test]
    fn escape_cannot_detach_an_in_flight_model_write() {
        let mut state = UiState::default();
        ready_models(&mut state, vec![choice("new")]);
        select_model(&mut state, 0, &mut Vec::new());
        let operation = state.model_operation;
        let mut effects = Vec::new();

        assert!(escape(&mut state, &mut effects));

        assert!(effects.is_empty());
        assert_eq!(state.model_operation, operation);
        assert!(matches!(state.overlay, Some(Overlay::Models(_))));
        assert_eq!(state.status_text(), Some("Finishing the model change…"));
    }

    #[test]
    fn choosing_the_applied_model_is_a_no_op() {
        let mut state = UiState::default();
        state.model_facts = Some(choice_facts("same", true));
        ready_models(&mut state, vec![choice("same")]);
        let mut effects = Vec::new();

        select_model(&mut state, 0, &mut effects);

        assert!(effects.is_empty());
        assert!(state.overlay.is_none());
        assert!(state.model_operation.is_none());
    }

    #[test]
    fn choosing_a_saved_but_unapplied_model_retries_it() {
        let mut state = UiState::default();
        state.model_facts = Some(choice_facts("same", false));
        ready_models(&mut state, vec![choice("same")]);
        let mut effects = Vec::new();

        select_model(&mut state, 0, &mut effects);

        assert!(matches!(effects.as_slice(), [Effect::SetModel { .. }]));
        assert_eq!(
            state.model_operation.map(|operation| operation.kind),
            Some(ModelOperationKind::Apply)
        );
    }

    #[test]
    fn active_work_allows_a_model_switch() {
        let session = SessionId::new();
        let runtime = bone_app::RuntimeId::new();
        let info = bone_app::SessionInfo {
            id: session,
            workspace: bone_app::WorkspaceId::new(),
            title: "Working".into(),
            archived: false,
        };
        let mut ui = crate::state::SessionUi::new(session, 1);
        ui.snapshot = Some(std::sync::Arc::new(bone_app::SessionView {
            session: info,
            runtime: bone_app::RuntimeState::Detached,
            draft: String::new(),
            inputs: vec![bone_app::InputView {
                id: bone_app::InputId(1),
                request_id: bone_app::RequestId::new(),
                text: "work".into(),
                reply_to: None,
                state: bone_app::InputState::Accepted { runtime },
            }],
            jobs: vec![],
            activity: vec![],
            history_through: SessionSeq(0),
            problem: None,
        }));
        let mut state = UiState::default();
        state.selected = Some(session);
        state.model_facts = Some(choice_facts("old", false));
        state.session_ui.insert(session, ui);
        ready_models(&mut state, vec![choice("new")]);
        let mut effects = Vec::new();

        select_model(&mut state, 0, &mut effects);

        assert!(
            matches!(effects.as_slice(), [Effect::SetModel { selection, .. }] if selection.model == "new")
        );
        assert!(state.model_operation.is_some());
        assert!(matches!(state.overlay, Some(Overlay::Models(_))));
    }

    #[test]
    fn stale_apply_reconciles_facts_without_touching_a_reopened_panel() {
        let mut state = UiState::default();
        ready_models(&mut state, vec![choice("old")]);
        select_model(&mut state, 0, &mut Vec::new());
        let old_apply = request(&state, ModelOperationKind::Apply);
        dismiss(&mut state, &mut Vec::new());
        let mut effects = Vec::new();
        open_models(&mut state, &mut effects);
        let current_load = request(&state, ModelOperationKind::Load);
        state.status = Some("new panel status".into());
        state.model_facts = Some(facts("visible model"));
        effects.clear();

        model_applied(
            &mut state,
            None,
            old_apply,
            None,
            Some("stale error".into()),
            &mut effects,
        );

        assert_eq!(request(&state, ModelOperationKind::Load), current_load);
        assert_eq!(state.status_text(), Some("new panel status"));
        assert_eq!(state.model_label(), Some("visible model"));
        assert!(matches!(
            &state.overlay,
            Some(Overlay::Models(models)) if models.choices.is_empty()
        ));
        assert!(matches!(
            effects.as_slice(),
            [Effect::LoadModelFacts { .. }]
        ));
    }

    #[test]
    fn stale_workspace_apply_reconciles_the_current_session() {
        let session = SessionId::new();
        let mut state = UiState::default();
        state.selected = Some(session);
        state.overlay = Some(Overlay::Models(ModelPanel::new(Some(session))));
        state.model_facts = Some(facts("visible model"));
        let mut effects = Vec::new();
        load_models(&mut state, Some(session), &mut effects);
        let operation = state.model_operation.expect("current load");
        effects.clear();

        model_applied(
            &mut state,
            None,
            operation.request.wrapping_add(1),
            None,
            None,
            &mut effects,
        );

        assert_eq!(state.model_operation, Some(operation));
        assert_eq!(state.model_label(), Some("visible model"));
        assert!(matches!(
            effects.as_slice(),
            [Effect::LoadModelFacts {
                session: Some(current),
                ..
            }] if *current == session
        ));
    }

    #[test]
    fn matching_apply_invalidates_older_facts_reads_and_only_closes_its_list() {
        let mut state = UiState::default();
        let mut effects = Vec::new();
        refresh_model_facts(&mut state, &mut effects);
        let stale_facts = state.model_facts_request;
        ready_models(&mut state, vec![choice("new")]);
        select_model(&mut state, 0, &mut Vec::new());
        let apply = request(&state, ModelOperationKind::Apply);

        model_applied(
            &mut state,
            None,
            apply,
            Some(facts("new")),
            None,
            &mut Vec::new(),
        );
        assert!(state.overlay.is_none());
        assert_eq!(state.model_label(), Some("new"));
        assert_ne!(state.model_facts_request, stale_facts);

        update(
            &mut state,
            UiEvent::ModelFactsLoaded {
                session: None,
                request: stale_facts,
                facts: Some(facts("old")),
            },
        );
        assert_eq!(state.model_label(), Some("new"));
    }

    #[test]
    fn successful_apply_does_not_clear_a_newer_unowned_status() {
        let mut state = UiState::default();
        ready_models(&mut state, vec![choice("new")]);
        select_model(&mut state, 0, &mut Vec::new());
        let apply = request(&state, ModelOperationKind::Apply);
        state.status = Some("newer status".into());

        model_applied(
            &mut state,
            None,
            apply,
            Some(facts("new")),
            None,
            &mut Vec::new(),
        );

        assert_eq!(state.status_text(), Some("newer status"));
    }

    #[test]
    fn failed_apply_keeps_the_picker_open_with_authoritative_current_and_saved_models() {
        let running = bone_app::ResolvedModel {
            selection: bone_app::ModelSelection::new(bone_app::ProfileId::chatgpt(), "old-running")
                .unwrap(),
            profile: bone_app::Profile::chatgpt(),
        };
        let mut saved = running.clone();
        saved.selection.model = "new-saved".into();
        let mut state = UiState::default();
        ready_models(&mut state, vec![choice("new-saved")]);
        select_model(&mut state, 0, &mut Vec::new());
        let apply = request(&state, ModelOperationKind::Apply);
        let mut effects = Vec::new();

        model_applied(
            &mut state,
            None,
            apply,
            Some(ModelFacts {
                saved: Ok(saved),
                running: Some(running),
            }),
            Some("request failed".into()),
            &mut effects,
        );

        assert!(state.model_operation.is_none());
        assert!(effects.is_empty());
        assert_eq!(
            state.model_footer(),
            "old-running · current · new-saved · configured"
        );
        assert!(
            state
                .model_configuration_summary()
                .contains("Configured: new-saved")
        );
        assert_eq!(
            state.status_text(),
            Some("Model switch failed: request failed")
        );
    }

    #[test]
    fn cancelled_login_reloads_saved_catalogue_and_rejects_its_late_receipt() {
        let mut state = UiState::default();
        let mut models = ModelPanel::new(None);
        models.screen = ModelScreen::Login {
            request: 41,
            state: LoginState::Connecting,
        };
        state.overlay = Some(Overlay::Models(models));
        let mut effects = Vec::new();
        assert!(escape(&mut state, &mut effects));
        assert_eq!(
            effects
                .iter()
                .filter(|effect| matches!(effect, Effect::CancelLogin))
                .count(),
            1
        );
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::LoadModels { .. }))
        );
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::LoadModelFacts { .. }))
        );
        assert!(matches!(
            &state.overlay,
            Some(Overlay::Models(ModelPanel {
                screen: ModelScreen::Tab { selected: 0 },
                ..
            }))
        ));

        login_changed(&mut state, 41, LoginState::Succeeded, &mut Vec::new());
        assert!(matches!(
            &state.overlay,
            Some(Overlay::Models(ModelPanel {
                screen: ModelScreen::Tab { .. },
                ..
            }))
        ));
    }

    #[test]
    fn failed_secret_save_keeps_the_form_safe_and_requires_reentry() {
        let mut state = UiState::default();
        state.orphan_draft = "ordinary draft".into();
        let secret = "secret-that-must-not-appear";
        let mut form = ConnectionForm::new(ConnectionKind::OpenAiApi);
        form.key = SecretText::from(secret.to_owned());
        setup_panel(&mut state, form);

        let mut effects = Vec::new();
        save_connection(&mut state, &mut effects);
        assert!(!format!("{state:?}").contains(secret));
        assert!(!format!("{effects:?}").contains(secret));
        let [
            Effect::SaveConnection {
                request,
                session,
                key: Some(key),
                ..
            },
        ] = effects.as_slice()
        else {
            panic!("save effect")
        };
        assert_eq!(key.as_str(), secret);
        let request = *request;
        let session = *session;
        assert!(matches!(
            &state.overlay,
            Some(Overlay::Models(ModelPanel {
                screen: ModelScreen::Setup(form),
                ..
            })) if form.pending_request == Some(request)
        ));
        let mut duplicate = Vec::new();
        save_connection(&mut state, &mut duplicate);
        assert!(duplicate.is_empty());
        connection_saved(
            &mut state,
            request,
            session,
            Some("could not apply connection".into()),
            false,
            &mut Vec::new(),
        );

        assert!(state.status_text().unwrap().contains("Re-enter"));
        assert!(matches!(
            &state.overlay,
            Some(Overlay::Models(ModelPanel {
                screen: ModelScreen::Setup(form),
                ..
            })) if form.pending_request.is_none() && form.key.is_empty()
        ));
        assert_eq!(state.orphan_draft.text(), "ordinary draft");
        assert!(state.model_operation.is_none());
        let mut retry = Vec::new();
        save_connection(&mut state, &mut retry);
        assert!(retry.is_empty());
        assert!(state.status_text().unwrap().contains("Re-enter"));
    }

    #[test]
    fn saved_key_failure_returns_to_models_without_requesting_the_secret_again() {
        let mut state = UiState::default();
        let mut form = ConnectionForm::new(ConnectionKind::OpenAiApi);
        form.key = SecretText::from("secret".to_owned());
        setup_panel(&mut state, form);
        let mut effects = Vec::new();
        save_connection(&mut state, &mut effects);
        let [
            Effect::SaveConnection {
                request, session, ..
            },
        ] = effects.as_slice()
        else {
            panic!("save effect")
        };
        let request = *request;
        let session = *session;
        let mut receipts = Vec::new();

        connection_saved(
            &mut state,
            request,
            session,
            Some("API key saved, but reload failed".into()),
            true,
            &mut receipts,
        );

        assert!(matches!(
            &state.overlay,
            Some(Overlay::Models(ModelPanel {
                screen: ModelScreen::Tab { .. },
                ..
            }))
        ));
        assert!(!state.status_text().unwrap().contains("Re-enter"));
        assert!(
            receipts
                .iter()
                .any(|effect| matches!(effect, Effect::LoadModels { .. }))
        );
    }

    #[test]
    fn the_kind_picker_starts_chatgpt_login_without_inventing_a_model() {
        let mut state = UiState::default();
        let mut models = ModelPanel::new(None);
        models.screen = ModelScreen::Kind { selected: 0 };
        state.overlay = Some(Overlay::Models(models));
        let mut effects = Vec::new();

        choose_connection(&mut state, 0, &mut effects);
        assert!(matches!(
            effects.as_slice(),
            [Effect::Login { profile, .. }] if *profile == bone_app::ProfileId::chatgpt()
        ));
    }

    #[test]
    fn signing_in_returns_to_the_tab_strip_and_reloads_the_strip() {
        let mut state = UiState::default();
        let mut models = ModelPanel::new(None);
        models.screen = ModelScreen::Login {
            request: 41,
            state: LoginState::Connecting,
        };
        state.overlay = Some(Overlay::Models(models));

        let mut effects = Vec::new();
        login_changed(&mut state, 41, LoginState::Succeeded, &mut effects);

        assert!(matches!(screen(&state), ModelScreen::Tab { selected: 0 }));
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::LoadModels { .. }))
        );
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::LoadModelFacts { .. }))
        );
    }

    #[test]
    fn first_model_for_an_existing_conversation_becomes_the_workspace_default() {
        let session = SessionId::new();
        let workspace = bone_app::WorkspaceId::new();
        let info = bone_app::SessionInfo {
            id: session,
            workspace,
            title: "First conversation".into(),
            archived: false,
        };
        let mut ui = crate::state::SessionUi::new(session, 1);
        ui.snapshot = Some(std::sync::Arc::new(bone_app::SessionView {
            session: info,
            runtime: bone_app::RuntimeState::Detached,
            draft: String::new(),
            inputs: vec![],
            jobs: vec![],
            activity: vec![],
            history_through: SessionSeq(0),
            problem: None,
        }));
        let mut state = UiState::default();
        state.selected = Some(session);
        state.session_ui.insert(session, ui);
        state.model_facts = Some(ModelFacts {
            saved: Err(bone_app::ConfigProblem::NeedsModel),
            running: None,
        });
        let mut models = ModelPanel::new(Some(session));
        models.screen = ModelScreen::Kind { selected: 0 };
        state.overlay = Some(Overlay::Models(models));
        let mut effects = Vec::new();

        choose_connection(&mut state, 0, &mut effects);

        assert!(matches!(effects.as_slice(), [Effect::Login { .. }]));
    }

    #[test]
    fn choosing_a_saved_official_kind_opens_its_tab_instead_of_asking_again() {
        let profile = Profile::new(
            bone_app::ProfileId::new("openai").unwrap(),
            "OpenAI API",
            bone_app::EndpointConfig::OpenAiResponses { base_url: None },
        )
        .unwrap();
        let selection = ModelSelection::new(profile.id.clone(), "gpt-5.6-sol").unwrap();
        let mut state = UiState::default();
        state.model_facts = Some(ModelFacts {
            saved: Ok(bone_app::ResolvedModel {
                selection: selection.clone(),
                profile: profile.clone(),
            }),
            running: None,
        });
        let mut models = ModelPanel::new(None);
        models.screen = ModelScreen::Kind { selected: 1 };
        models.profiles.push(profile.clone());
        models.choices.push(ModelChoice {
            selection,
            profile_label: "OpenAI API".into(),
            label: "GPT-5.6 Sol".into(),
        });
        state.overlay = Some(Overlay::Models(models));
        let mut effects = Vec::new();

        choose_connection(&mut state, 1, &mut effects);

        assert!(effects.is_empty());
        assert!(matches!(screen(&state), ModelScreen::Tab { selected: 0 }));
        assert_eq!(panel(&state).tab, 0);
        assert_eq!(
            state.status_text().map(str::to_owned),
            Some(format!(
                "Already connected: {}",
                ConnectionKind::OpenAiApi.label()
            ))
        );

        // `e` is the way back into its key and model fields.
        edit_connection(&mut state, &mut effects);
        assert!(matches!(
            screen(&state),
            ModelScreen::Setup(form)
                if form.edits_existing_connection()
                    && form.fields() == [SetupField::Key, SetupField::Model]
        ));
    }

    #[test]
    fn editing_a_chatgpt_tab_restarts_sign_in_without_a_form() {
        let mut state = UiState::default();
        let mut models = ModelPanel::new(None);
        models.profiles.push(Profile::chatgpt());
        state.overlay = Some(Overlay::Models(models));
        let mut effects = Vec::new();

        edit_connection(&mut state, &mut effects);

        assert!(matches!(screen(&state), ModelScreen::Login { .. }));
        assert!(matches!(
            effects.as_slice(),
            [Effect::Login { profile, .. }] if *profile == bone_app::ProfileId::chatgpt()
        ));
    }

    #[test]
    fn the_add_tab_has_nothing_to_edit_or_delete() {
        let mut state = UiState::default();
        let mut models = ModelPanel::new(None);
        state.overlay = Some(Overlay::Models(std::mem::replace(
            &mut models,
            ModelPanel::new(None),
        )));
        let mut effects = Vec::new();

        edit_connection(&mut state, &mut effects);
        delete_connection(&mut state);

        assert!(effects.is_empty());
        assert!(matches!(screen(&state), ModelScreen::Tab { .. }));
        assert!(panel(&state).tab_is_add());
    }

    #[test]
    fn successful_connection_save_does_not_clear_a_newer_unowned_status() {
        let mut state = UiState::default();
        let mut form = ConnectionForm::new(ConnectionKind::OpenAiApi);
        form.key = SecretText::from("secret".to_owned());
        setup_panel(&mut state, form);
        let mut effects = Vec::new();
        save_connection(&mut state, &mut effects);
        let [
            Effect::SaveConnection {
                request, session, ..
            },
        ] = effects.as_slice()
        else {
            panic!("save effect")
        };
        let request = *request;
        let session = *session;
        state.status = Some("newer status".into());

        connection_saved(&mut state, request, session, None, false, &mut Vec::new());

        assert_eq!(state.status_text(), Some("newer status"));
    }

    #[test]
    fn late_connection_receipt_preserves_a_new_form_and_its_status() {
        let mut state = UiState::default();
        let mut old = ConnectionForm::new(ConnectionKind::OpenAiApi);
        old.key = SecretText::from(String::from("secret"));
        setup_panel(&mut state, old);
        let effects = {
            let mut effects = Vec::new();
            save_connection(&mut state, &mut effects);
            effects
        };
        let [
            Effect::SaveConnection {
                request,
                session,
                key: Some(key),
                ..
            },
        ] = effects.as_slice()
        else {
            panic!("save effect")
        };
        assert_eq!(key.as_str(), "secret");
        let old_request = *request;
        let old_session = *session;

        let mut replacement = ConnectionForm::new(ConnectionKind::AnthropicApi);
        replacement.key = SecretText::from(String::from("replacement-secret"));
        setup_panel(&mut state, replacement);
        let mut replacement_effects = Vec::new();
        save_connection(&mut state, &mut replacement_effects);
        let [
            Effect::SaveConnection {
                request: replacement_request,
                ..
            },
        ] = replacement_effects.as_slice()
        else {
            panic!("replacement save effect")
        };
        let replacement_request = *replacement_request;
        state.status = Some("new pending status".into());
        let mut receipts = Vec::new();
        connection_saved(
            &mut state,
            old_request,
            old_session,
            None,
            false,
            &mut receipts,
        );

        assert_eq!(state.status_text(), Some("new pending status"));
        assert!(matches!(
            &state.overlay,
            Some(Overlay::Models(ModelPanel {
                screen: ModelScreen::Setup(form),
                ..
            })) if form.kind == ConnectionKind::AnthropicApi
                && form.pending_request == Some(replacement_request)
        ));
        assert!(matches!(
            receipts.as_slice(),
            [Effect::LoadModelFacts { .. }]
        ));
    }

    #[test]
    fn session_switch_dismisses_login_without_changing_focus() {
        let workspace = bone_app::WorkspaceId::new();
        let first = SessionId::new();
        let second = SessionId::new();
        let mut state = UiState::default();
        state.session_rows = [first, second]
            .into_iter()
            .map(|id| {
                super::super::SessionNavRow::provisional(bone_app::SessionInfo {
                    id,
                    workspace,
                    title: "session".into(),
                    archived: false,
                })
            })
            .collect();
        state.selected = Some(first);
        state.set_workspace_target(WorkspaceTarget::Sessions);
        let mut models = ModelPanel::new(Some(first));
        models.screen = ModelScreen::Login {
            request: 7,
            state: LoginState::Connecting,
        };
        state.overlay = Some(Overlay::Models(models));

        let effects = update(&mut state, UiEvent::Action(Action::SelectSession(second)));

        assert_eq!(state.selected, Some(second));
        assert_eq!(state.workspace_target(), WorkspaceTarget::Sessions);
        assert!(state.overlay.is_none());
        assert_eq!(
            effects
                .iter()
                .filter(|effect| matches!(effect, Effect::CancelLogin))
                .count(),
            1
        );
    }
}
