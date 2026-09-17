#[cfg(test)]
use bone_app::SessionSeq;
use bone_app::{LoginState, ModelSelection, Profile, SessionId};
use unicode_segmentation::UnicodeSegmentation;

use super::{
    ConnectionForm, ConnectionKind, Effect, ModelChoice, ModelFacts, ModelForm, SecretText, Status,
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
    pub(crate) return_to_add_model: bool,
    pending_selection: Option<ModelSelection>,
    pending_reasoning: bool,
    pub(crate) screen: ModelScreen,
}

#[derive(Debug)]
pub(crate) enum ModelScreen {
    List {
        selected: usize,
    },
    Reasoning {
        selected: usize,
        selection: ModelSelection,
    },
    AddModel {
        selected: usize,
    },
    Add {
        selected: usize,
    },
    Advanced {
        selected: usize,
    },
    Manage {
        selected: usize,
    },
    Setup(Box<ConnectionForm>),
    ModelForm(Box<ModelForm>),
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
}

impl ModelPanel {
    pub(crate) fn new(session: Option<SessionId>) -> Self {
        Self {
            session,
            choices: Vec::new(),
            profiles: Vec::new(),
            return_to_add_model: false,
            pending_selection: None,
            pending_reasoning: false,
            screen: ModelScreen::List { selected: 0 },
        }
    }

    pub(crate) fn row_count(&self) -> usize {
        match &self.screen {
            ModelScreen::List { .. } => {
                self.choices.len() + if self.profiles.is_empty() { 1 } else { 3 }
            }
            ModelScreen::Reasoning { .. } => REASONING_EFFORTS.len(),
            ModelScreen::AddModel { .. } => self.profiles.len().max(1),
            ModelScreen::Add { .. } => 4,
            ModelScreen::Advanced { .. } => ConnectionKind::ADVANCED.len(),
            ModelScreen::Manage { .. } => self.profiles.len(),
            ModelScreen::Setup(_) | ModelScreen::ModelForm(_) | ModelScreen::Login { .. } => 0,
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
    models.choices = choices;
    models.profiles = profiles;
    let row_count = models.row_count();
    if let ModelScreen::List { selected } = &mut models.screen {
        *selected = preferred
            .as_ref()
            .and_then(|selection| {
                models
                    .choices
                    .iter()
                    .position(|choice| same_model(&choice.selection, selection))
            })
            .unwrap_or_else(|| (*selected).min(row_count.saturating_sub(1)));
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
        && matches!(models.screen, ModelScreen::List { .. })
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

    let keep_model_panel =
        matches!(&models.screen, ModelScreen::ModelForm(_)) || models.return_to_add_model;
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
    if keep_model_panel {
        return_to_models(state, effects);
    } else {
        dismiss(state, effects);
        refresh_model_facts(state, effects);
    }
}

fn reconcile_connection_save(state: &mut UiState, effects: &mut Vec<Effect>) {
    let current = state.selected;
    if state.model_operation.is_none()
        && matches!(
            &state.overlay,
            Some(Overlay::Models(models))
                if models.session == current && matches!(models.screen, ModelScreen::List { .. })
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
        let return_to_add_model = models.return_to_add_model;
        state.overlay = Some(Overlay::Models(models));
        if return_to_add_model {
            return_to_models(state, effects);
        } else {
            dismiss(state, effects);
            refresh_model_facts(state, effects);
        }
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
                ModelScreen::List { selected }
                | ModelScreen::Reasoning { selected, .. }
                | ModelScreen::AddModel { selected }
                | ModelScreen::Add { selected }
                | ModelScreen::Advanced { selected }
                | ModelScreen::Manage { selected },
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
                ModelScreen::List { selected }
                | ModelScreen::Reasoning { selected, .. }
                | ModelScreen::AddModel { selected }
                | ModelScreen::Add { selected }
                | ModelScreen::Advanced { selected }
                | ModelScreen::Manage { selected } => {
                    *selected = (*selected + 1).min(row_count.saturating_sub(1));
                }
                ModelScreen::Setup(_) | ModelScreen::ModelForm(_) | ModelScreen::Login { .. } => {}
            }
        }
        _ => {}
    }
}

pub(super) fn activate(state: &mut UiState, effects: &mut Vec<Effect>) {
    match &state.overlay {
        Some(Overlay::Objects(objects)) => {
            let selected = objects.selected;
            select_object(state, selected, effects);
        }
        Some(Overlay::Models(ModelPanel {
            screen: ModelScreen::List { selected },
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
            screen: ModelScreen::AddModel { selected },
            ..
        })) => {
            let selected = *selected;
            select_model(state, selected, effects);
        }
        Some(Overlay::Models(ModelPanel {
            screen: ModelScreen::Add { selected } | ModelScreen::Advanced { selected },
            ..
        })) => {
            let selected = *selected;
            choose_connection(state, selected, effects);
        }
        Some(Overlay::Models(ModelPanel {
            screen: ModelScreen::Manage { selected },
            ..
        })) => {
            let selected = *selected;
            select_model(state, selected, effects);
        }
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

pub(super) fn toggle_model_apply(state: &mut UiState) {
    if let Some(Overlay::Models(ModelPanel {
        screen: ModelScreen::ModelForm(form),
        ..
    })) = &mut state.overlay
        && !form.remove
        && form.pending_request.is_none()
    {
        form.apply = !form.apply;
        state.caret_visible = true;
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
    let ModelScreen::List { selected } = models.screen else {
        state.overlay = Some(Overlay::Models(models));
        return;
    };
    let Some(choice) = models.choices.get(selected).cloned() else {
        state.overlay = Some(Overlay::Models(models));
        return;
    };
    let Some(profile) = models
        .profiles
        .iter()
        .find(|profile| profile.id == choice.selection.profile)
        .cloned()
    else {
        state.overlay = Some(Overlay::Models(models));
        state.status = Some(Status::selection(
            state.selected,
            "This model's connection is no longer available",
        ));
        return;
    };
    if !profile.has_model(&choice.selection.model) {
        state.overlay = Some(Overlay::Models(models));
        state.status = Some(Status::selection(
            state.selected,
            "Only saved models can be removed",
        ));
        return;
    }
    models.screen =
        ModelScreen::ModelForm(Box::new(ModelForm::remove(profile, choice.selection.model)));
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
        ModelScreen::List { .. } => {
            if index >= models.row_count() {
                state.overlay = Some(Overlay::Models(models));
                return;
            }
            models.screen = ModelScreen::List { selected: index };
            if let Some(selection) = models
                .choices
                .get(index)
                .map(|choice| choice.selection.clone())
            {
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
                return;
            }
            let action = index.saturating_sub(models.choices.len());
            if models.profiles.is_empty() {
                models.return_to_add_model = true;
                models.screen = ModelScreen::Add { selected: 0 };
            } else if action == 0 {
                models.screen = ModelScreen::AddModel { selected: 0 };
            } else if action == 1 {
                models.screen = ModelScreen::Add { selected: 0 };
            } else {
                models.screen = ModelScreen::Manage { selected: 0 };
            }
        }
        ModelScreen::AddModel { .. } => {
            let Some(profile) = models.profiles.get(index).cloned() else {
                if models.profiles.is_empty() && index == 0 {
                    models.screen = ModelScreen::Add { selected: 0 };
                }
                state.overlay = Some(Overlay::Models(models));
                return;
            };
            models.screen = ModelScreen::ModelForm(Box::new(ModelForm::new(profile)));
        }
        ModelScreen::Manage { .. } => {
            let Some(profile) = models.profiles.get(index).cloned() else {
                state.overlay = Some(Overlay::Models(models));
                return;
            };
            models.screen = ModelScreen::Manage { selected: index };
            if profile.id == bone_app::ProfileId::chatgpt() {
                begin_login(state, models, effects);
                return;
            }
            let Some(form) = ConnectionForm::edit_selection(&profile) else {
                unreachable!("ChatGPT handled above")
            };
            models.screen = ModelScreen::Setup(Box::new(form));
        }
        _ => {
            state.overlay = Some(Overlay::Models(models));
            return;
        }
    }
    state.overlay = Some(Overlay::Models(models));
    state.status = None;
}

pub(super) fn choose_connection(state: &mut UiState, index: usize, effects: &mut Vec<Effect>) {
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
    let kind = match models.screen {
        ModelScreen::Add { .. } => match index {
            0 => {
                begin_login(state, models, effects);
                return;
            }
            1 | 2 => {
                let (id, kind) = if index == 1 {
                    (
                        bone_app::ProfileId::new("openai").unwrap(),
                        ConnectionKind::OpenAiApi,
                    )
                } else {
                    (
                        bone_app::ProfileId::new("anthropic").unwrap(),
                        ConnectionKind::AnthropicApi,
                    )
                };
                if let Some(profile) = models
                    .profiles
                    .iter()
                    .find(|profile| profile.id == id)
                    .cloned()
                {
                    models.screen = ModelScreen::Setup(Box::new(
                        ConnectionForm::edit_selection(&profile)
                            .expect("API profiles have an editable connection form"),
                    ));
                    state.overlay = Some(Overlay::Models(models));
                    state.status = None;
                    return;
                }
                kind
            }
            3 => {
                models.screen = ModelScreen::Advanced { selected: 0 };
                state.overlay = Some(Overlay::Models(models));
                state.status = None;
                return;
            }
            _ => {
                state.overlay = Some(Overlay::Models(models));
                return;
            }
        },
        ModelScreen::Advanced { .. } => {
            let Some(kind) = ConnectionKind::ADVANCED.get(index).copied() else {
                state.overlay = Some(Overlay::Models(models));
                return;
            };
            kind
        }
        _ => {
            state.overlay = Some(Overlay::Models(models));
            return;
        }
    };
    models.screen = ModelScreen::Setup(Box::new(ConnectionForm::new(kind)));
    state.overlay = Some(Overlay::Models(models));
    state.status = None;
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

fn return_to_models(state: &mut UiState, effects: &mut Vec<Effect>) {
    let Some(Overlay::Models(models)) = &mut state.overlay else {
        return;
    };
    let return_to_add_model = models.return_to_add_model;
    models.return_to_add_model = false;
    models.screen = if return_to_add_model {
        ModelScreen::AddModel { selected: 0 }
    } else {
        ModelScreen::List { selected: 0 }
    };
    let session = models.session;
    load_models(state, session, effects);
    refresh_model_facts(state, effects);
}

fn return_to_model_list(state: &mut UiState) {
    if let Some(Overlay::Models(models)) = &mut state.overlay {
        models.return_to_add_model = false;
        models.screen = ModelScreen::List { selected: 0 };
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
    if apply_pending || save_pending {
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
            screen: ModelScreen::Reasoning { .. },
            ..
        }) => {
            state.status = None;
            if let Some(Overlay::Models(models)) = &mut state.overlay {
                models.screen = ModelScreen::List { selected: 0 };
            }
        }
        Overlay::Models(ModelPanel {
            screen: ModelScreen::Setup(_),
            ..
        }) => {
            state.status = None;
            if let Some(Overlay::Models(models)) = &mut state.overlay
                && let ModelScreen::Setup(form) = &models.screen
            {
                models.screen = if form.edits_existing_connection() {
                    ModelScreen::Manage { selected: 0 }
                } else if form.kind.advanced() {
                    ModelScreen::Advanced { selected: 0 }
                } else {
                    ModelScreen::Add { selected: 0 }
                };
            }
        }
        Overlay::Models(ModelPanel {
            screen: ModelScreen::ModelForm(_),
            ..
        }) => {
            state.status = None;
            if let Some(Overlay::Models(models)) = &mut state.overlay {
                models.screen = ModelScreen::AddModel { selected: 0 };
            }
        }
        Overlay::Models(ModelPanel {
            screen: ModelScreen::Advanced { .. },
            ..
        }) => {
            state.status = None;
            if let Some(Overlay::Models(models)) = &mut state.overlay {
                models.screen = ModelScreen::Add { selected: 0 };
            }
        }
        Overlay::Models(ModelPanel {
            screen:
                ModelScreen::AddModel { .. } | ModelScreen::Add { .. } | ModelScreen::Manage { .. },
            ..
        }) => {
            state.status = None;
            return_to_model_list(state);
        }
        Overlay::Models(ModelPanel {
            screen: ModelScreen::List { .. },
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
    use crate::state::{Action, SetupField, UiEvent, WorkspaceTarget, update};

    fn choice(model: &str) -> ModelChoice {
        ModelChoice {
            selection: bone_app::ModelSelection::new(
                bone_app::ProfileId::new("test-api").unwrap(),
                model,
            )
            .unwrap(),
            profile_label: "Test API".into(),
            label: model.into(),
        }
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

    fn ready_models(state: &mut UiState, choices: Vec<ModelChoice>) -> u64 {
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
        models_loaded(state, state.selected, load, choices, vec![]);
        assert!(state.model_operation.is_none());
        load
    }

    fn setup_panel(state: &mut UiState, form: ConnectionForm) {
        let mut models = ModelPanel::new(state.selected);
        models.screen = ModelScreen::Setup(Box::new(form));
        state.overlay = Some(Overlay::Models(models));
    }

    #[test]
    fn add_model_enters_the_one_field_form_and_delete_returns_to_the_list() {
        let mut profile = bone_app::Profile::new(
            bone_app::ProfileId::new("custom-api").unwrap(),
            "Custom API",
            bone_app::EndpointConfig::OpenAiResponses {
                base_url: Some("http://127.0.0.1:8080/v1".into()),
            },
        )
        .unwrap();
        profile.add_model("model-a").unwrap();
        let selection = bone_app::ModelSelection::new(profile.id.clone(), "model-a").unwrap();
        let mut models = ModelPanel::new(None);
        models.profiles = vec![profile.clone()];
        models.screen = ModelScreen::AddModel { selected: 0 };
        state_with_models(&mut models, selection);
    }

    #[test]
    fn adding_a_connection_from_an_empty_catalogue_returns_to_add_model() {
        let mut state = UiState::default();
        let mut models = ModelPanel::new(None);
        models.screen = ModelScreen::List { selected: 0 };
        state.overlay = Some(Overlay::Models(models));
        let mut effects = Vec::new();
        select_model(&mut state, 0, &mut effects);
        assert!(matches!(
            state.overlay,
            Some(Overlay::Models(ModelPanel {
                screen: ModelScreen::Add { .. },
                return_to_add_model: true,
                ..
            }))
        ));

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
        assert!(matches!(
            state.overlay,
            Some(Overlay::Models(ModelPanel {
                screen: ModelScreen::AddModel { .. },
                return_to_add_model: false,
                ..
            }))
        ));
    }

    fn state_with_models(models: &mut ModelPanel, selection: bone_app::ModelSelection) {
        let mut state = UiState::default();
        models.choices = vec![ModelChoice {
            selection,
            profile_label: "Custom API".into(),
            label: "model-a".into(),
        }];
        state.overlay = Some(Overlay::Models(std::mem::replace(
            models,
            ModelPanel::new(None),
        )));
        let mut effects = Vec::new();
        select_model(&mut state, 0, &mut effects);
        assert!(matches!(
            state.overlay,
            Some(Overlay::Models(ModelPanel {
                screen: ModelScreen::ModelForm(_),
                ..
            }))
        ));
        let Some(Overlay::Models(models)) = state.overlay.as_mut() else {
            unreachable!()
        };
        models.screen = ModelScreen::List { selected: 0 };
        delete_model(&mut state, &mut effects);
        let request = match &state.overlay {
            Some(Overlay::Models(ModelPanel {
                screen: ModelScreen::ModelForm(form),
                ..
            })) => form.pending_request.expect("delete request"),
            _ => panic!("delete should use the model save path"),
        };
        connection_saved(&mut state, request, None, None, false, &mut effects);
        assert!(matches!(
            state.overlay,
            Some(Overlay::Models(ModelPanel {
                screen: ModelScreen::List { .. },
                ..
            }))
        ));
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
        models.screen = ModelScreen::Add { selected: 3 };
        state.overlay = Some(Overlay::Models(models));
        panel_previous(&mut state);
        assert!(matches!(
            &state.overlay,
            Some(Overlay::Models(ModelPanel {
                screen: ModelScreen::Add { selected: 2 },
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
                screen: ModelScreen::List { .. },
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
                screen: ModelScreen::List { .. },
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
                screen: ModelScreen::List { .. },
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
                screen: ModelScreen::List { .. },
                ..
            }))
        ));

        models_loaded(&mut state, None, load, vec![], vec![Profile::chatgpt()]);
        assert!(matches!(
            state.overlay,
            Some(Overlay::Models(ModelPanel {
                screen: ModelScreen::List { .. },
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
                screen: ModelScreen::List { selected: 1 },
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
    fn adding_a_responses_model_chooses_reasoning_after_the_connection_is_saved() {
        let profile = bone_app::Profile::new(
            bone_app::ProfileId::new("local").unwrap(),
            "Local",
            bone_app::EndpointConfig::OpenAiResponses {
                base_url: Some("http://127.0.0.1:11434/v1".into()),
            },
        )
        .unwrap();
        let mut form = ModelForm::new(profile.clone());
        form.model = "local-model".into();
        let mut models = ModelPanel::new(None);
        models.profiles = vec![profile];
        models.screen = ModelScreen::ModelForm(Box::new(form));
        let mut state = UiState::default();
        state.overlay = Some(Overlay::Models(models));

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

        assert!(matches!(
            state.overlay,
            Some(Overlay::Models(ModelPanel {
                screen: ModelScreen::Reasoning { .. },
                ..
            }))
        ));
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
                screen: ModelScreen::List { selected: 0 },
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
                screen: ModelScreen::List { selected: 0 },
                ..
            }))
        ));

        login_changed(&mut state, 41, LoginState::Succeeded, &mut Vec::new());
        assert!(matches!(
            &state.overlay,
            Some(Overlay::Models(ModelPanel {
                screen: ModelScreen::List { .. },
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
                screen: ModelScreen::List { .. },
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
    fn chatgpt_onboarding_starts_login_without_inventing_a_model() {
        let mut state = UiState::default();
        let mut models = ModelPanel::new(None);
        models.screen = ModelScreen::Add { selected: 0 };
        models.profiles.push(Profile::chatgpt());
        state.overlay = Some(Overlay::Models(models));
        let mut effects = Vec::new();
        choose_connection(&mut state, 0, &mut effects);
        assert!(matches!(
            effects.as_slice(),
            [Effect::Login { profile, .. }] if *profile == bone_app::ProfileId::chatgpt()
        ));
    }

    #[test]
    fn chatgpt_onboarding_returns_to_add_model_after_login() {
        let mut state = UiState::default();
        let mut models = ModelPanel::new(None);
        models.return_to_add_model = true;
        models.screen = ModelScreen::Login {
            request: 41,
            state: LoginState::Connecting,
        };
        state.overlay = Some(Overlay::Models(models));

        let mut effects = Vec::new();
        login_changed(&mut state, 41, LoginState::Succeeded, &mut effects);

        assert!(matches!(
            state.overlay,
            Some(Overlay::Models(ModelPanel {
                screen: ModelScreen::AddModel { .. },
                return_to_add_model: false,
                ..
            }))
        ));
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
        models.screen = ModelScreen::Add { selected: 0 };
        models.profiles.push(Profile::chatgpt());
        state.overlay = Some(Overlay::Models(models));
        let mut effects = Vec::new();

        choose_connection(&mut state, 0, &mut effects);

        assert!(matches!(effects.as_slice(), [Effect::Login { .. }]));
    }

    #[test]
    fn add_routes_an_existing_official_connection_to_key_management() {
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
        models.screen = ModelScreen::Add { selected: 1 };
        models.profiles.push(profile);
        models.choices.push(ModelChoice {
            selection,
            profile_label: "OpenAI API".into(),
            label: "GPT-5.6 Sol".into(),
        });
        state.overlay = Some(Overlay::Models(models));
        let mut effects = Vec::new();

        choose_connection(&mut state, 1, &mut effects);

        assert!(effects.is_empty());
        assert!(matches!(
            &state.overlay,
            Some(Overlay::Models(ModelPanel {
                screen: ModelScreen::Setup(form),
                ..
            })) if form.edits_existing_connection()
                && form.fields() == [SetupField::Key, SetupField::Model]
        ));
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
