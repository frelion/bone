use bone_app::{LoginState, Profile, SessionId, SessionSeq};
use unicode_segmentation::UnicodeSegmentation;

use super::{
    ConnectionForm, ConnectionKind, Effect, ModelChoice, ModelFacts, SecretText, SetupField,
    Status, UiState,
    reader::{ReaderContent, ReaderSource},
};

#[derive(Debug)]
pub(crate) enum Panel {
    Objects(ObjectPanel),
    Models(ModelPanel),
    Help,
    Reader(ReaderPanel),
}

#[derive(Debug)]
pub(crate) struct ObjectPanel {
    pub(crate) session: SessionId,
    pub(crate) choices: Vec<(ReaderSource, String)>,
    pub(crate) selected: usize,
}

#[derive(Debug)]
pub(crate) struct ReaderPanel {
    pub(crate) content: ReaderContent,
    pub(crate) scroll: usize,
}

#[derive(Debug)]
pub(crate) struct ModelPanel {
    pub(crate) session: Option<SessionId>,
    pub(crate) choices: Vec<ModelChoice>,
    pub(crate) profiles: Vec<Profile>,
    pub(crate) screen: ModelScreen,
}

#[derive(Debug)]
pub(crate) enum ModelScreen {
    List { selected: usize },
    Add { selected: usize },
    Setup(ConnectionForm),
    Login { request: u64, state: LoginState },
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
            screen: ModelScreen::List { selected: 0 },
        }
    }

    pub(crate) fn row_count(&self) -> usize {
        self.choices.len() + self.profiles.len() + 1
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
    replace(state, Panel::Models(ModelPanel::new(session)), effects);
    load_models(state, session, effects);
}

pub(super) fn set_named_model(
    state: &mut UiState,
    profile: String,
    model: String,
    effects: &mut Vec<Effect>,
) {
    let session = state.selected;
    state.status = None;
    replace(state, Panel::Models(ModelPanel::new(session)), effects);
    let request = state.generation();
    state.model_operation = Some(ModelOperation {
        session,
        request,
        kind: ModelOperationKind::Apply,
    });
    effects.push(Effect::SetNamedModel {
        session,
        request,
        profile,
        model,
    });
}

pub(super) fn open_help(state: &mut UiState, effects: &mut Vec<Effect>) {
    replace(state, Panel::Help, effects);
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

pub(super) fn refresh_model_label(state: &mut UiState, effects: &mut Vec<Effect>) {
    state.model_label = None;
    state.model_facts = None;
    request_model_label(state, effects);
}

fn reconcile_model_label(state: &mut UiState, effects: &mut Vec<Effect>) {
    request_model_label(state, effects);
}

fn request_model_label(state: &mut UiState, effects: &mut Vec<Effect>) {
    state.model_label_request = state.generation();
    effects.push(Effect::LoadModelLabel {
        session: state.selected,
        request: state.model_label_request,
    });
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
    let Some(Panel::Models(models)) = &mut state.panel else {
        return;
    };
    if models.session != session {
        return;
    }
    models.choices = choices;
    models.profiles = profiles;
    let row_count = models.row_count();
    if let ModelScreen::List { selected } = &mut models.screen {
        *selected = (*selected).min(row_count.saturating_sub(1));
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
    if let Some(Panel::Models(models)) = &mut state.panel
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
    label: Option<String>,
    facts: Option<ModelFacts>,
    error: Option<String>,
    effects: &mut Vec<Effect>,
) {
    let current = state
        .model_operation
        .is_some_and(|operation| operation.matches(session, request, ModelOperationKind::Apply));
    if !current {
        if state.selected == session || session.is_none() {
            reconcile_model_label(state, effects);
        }
        return;
    }
    state.model_operation = None;
    if state.selected != session {
        if session.is_none() {
            reconcile_model_label(state, effects);
        }
        return;
    }
    let matching_list = matches!(
        &state.panel,
        Some(Panel::Models(models))
            if models.session == session && matches!(models.screen, ModelScreen::List { .. })
    );
    let failed = error.is_some();
    state.model_label_request = state.generation();
    state.model_label = label;
    state.model_facts = facts;
    if matching_list || state.panel.is_none() {
        if let Some(error) = error {
            state.status = Some(Status::panel_request(session, request, error));
        } else if state
            .status
            .as_ref()
            .is_some_and(|status| status.belongs_to_panel_request(session, request))
        {
            state.status = None;
        }
    }
    if failed {
        if matching_list {
            load_models(state, session, effects);
        }
    } else if matching_list {
        dismiss(state, effects);
    }
}

pub(super) fn connection_saved(
    state: &mut UiState,
    request: u64,
    session: Option<SessionId>,
    error: Option<String>,
    notice: Option<String>,
    effects: &mut Vec<Effect>,
) {
    let Some(panel) = state.panel.take() else {
        reconcile_connection_save(state, effects);
        return;
    };
    let Panel::Models(mut models) = panel else {
        state.panel = Some(panel);
        reconcile_connection_save(state, effects);
        return;
    };
    let ModelScreen::Setup(form) = &mut models.screen else {
        state.panel = Some(Panel::Models(models));
        reconcile_connection_save(state, effects);
        return;
    };
    if state.selected != session
        || models.session != session
        || !form.saving
        || form.request != request
    {
        state.panel = Some(Panel::Models(models));
        reconcile_connection_save(state, effects);
        return;
    }

    if let Some(error) = error {
        form.saving = false;
        state.status = Some(Status::panel_request(
            session,
            request,
            if form.key_was_sent {
                format!("{error} Re-enter the API key before retrying.")
            } else {
                error
            },
        ));
        state.panel = Some(Panel::Models(models));
        reconcile_model_label(state, effects);
        return;
    }

    if state
        .status
        .as_ref()
        .is_some_and(|status| status.belongs_to_panel_request(session, request))
    {
        state.status = None;
    }
    let subscription = form.kind.subscription();
    if subscription {
        let login_request = state.generation();
        models.screen = ModelScreen::Login {
            request: login_request,
            state: LoginState::Connecting,
        };
        state.panel = Some(Panel::Models(models));
        effects.push(Effect::Login {
            profile: bone_app::ProfileId::chatgpt(),
            request: login_request,
        });
        if let Some(notice) = notice {
            state.status = Some(Status::panel_request(session, login_request, notice));
        }
    } else {
        state.panel = Some(Panel::Models(models));
        return_to_models(state, effects);
        if let Some(notice) = notice {
            state.status = Some(Status::panel_request(session, request, notice));
        }
    }
}

fn reconcile_connection_save(state: &mut UiState, effects: &mut Vec<Effect>) {
    let current = state.selected;
    if state.model_operation.is_none()
        && matches!(
            &state.panel,
            Some(Panel::Models(models))
                if models.session == current && matches!(models.screen, ModelScreen::List { .. })
        )
    {
        load_models(state, current, effects);
    }
    reconcile_model_label(state, effects);
}

pub(super) fn login_changed(
    state: &mut UiState,
    request: u64,
    login: LoginState,
    effects: &mut Vec<Effect>,
) {
    let selected = state.selected;
    let Some(Panel::Models(models)) = state.panel.as_mut() else {
        return;
    };
    if models.session != selected {
        return;
    }
    let ModelScreen::Login {
        request: current,
        state: current_state,
    } = &mut models.screen
    else {
        return;
    };
    if *current != request {
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
        return_to_models(state, effects);
    }
}

pub(super) fn open_history(state: &mut UiState, sequence: SessionSeq, effects: &mut Vec<Effect>) {
    let Some(content) = state.selected_ui().and_then(|ui| {
        ui.transcript
            .find(sequence)
            .and_then(|entry| ReaderContent::from_history(ui.id, entry))
    }) else {
        return;
    };
    pin_reading(state);
    replace(
        state,
        Panel::Reader(ReaderPanel { content, scroll: 0 }),
        effects,
    );
}

pub(super) fn open_job(state: &mut UiState, job: bone_app::JobRef, effects: &mut Vec<Effect>) {
    let Some(content) = state
        .selected_ui()
        .and_then(|ui| ui.snapshot.as_ref())
        .and_then(|snapshot| ReaderContent::from_job(snapshot, job))
    else {
        return;
    };
    pin_reading(state);
    replace(
        state,
        Panel::Reader(ReaderPanel { content, scroll: 0 }),
        effects,
    );
}

pub(super) fn refresh_reader(state: &mut UiState, snapshot: &bone_app::SessionView) {
    if let Some(Panel::Reader(reader)) = &mut state.panel {
        reader.content.refresh_job(snapshot);
    }
}

pub(super) fn open_objects(state: &mut UiState, effects: &mut Vec<Effect>) {
    let Some(ui) = state.selected_ui() else {
        state.status = Some(Status::selection(None, "There is no session to inspect"));
        return;
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
        Panel::Objects(ObjectPanel {
            session,
            choices,
            selected: 0,
        }),
        effects,
    );
}

pub(super) fn select_object(state: &mut UiState, index: usize, effects: &mut Vec<Effect>) {
    let Some(Panel::Objects(objects)) = &mut state.panel else {
        return;
    };
    if index >= objects.choices.len() {
        return;
    }
    objects.selected = index;
    open_object(state, effects);
}

fn open_object(state: &mut UiState, effects: &mut Vec<Effect>) {
    let Some((session, source)) = (match &state.panel {
        Some(Panel::Objects(objects)) => objects
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
        replace(
            state,
            Panel::Reader(ReaderPanel { content, scroll: 0 }),
            effects,
        );
        state.status = None;
    } else {
        state.status = Some(Status::selection(
            state.selected,
            "This object is no longer loaded; reopen /details",
        ));
    }
}

fn pin_reading(state: &mut UiState) {
    if let Some(ui) = state.selected_ui_mut() {
        ui.transcript.pin_reading();
    }
}

pub(super) fn panel_previous(state: &mut UiState) {
    match &mut state.panel {
        Some(Panel::Objects(objects)) => {
            objects.selected = objects.selected.saturating_sub(1);
        }
        Some(Panel::Models(ModelPanel {
            screen: ModelScreen::List { selected } | ModelScreen::Add { selected },
            ..
        })) => *selected = selected.saturating_sub(1),
        _ => {}
    }
}

pub(super) fn panel_next(state: &mut UiState) {
    match &mut state.panel {
        Some(Panel::Objects(objects)) => {
            objects.selected = (objects.selected + 1).min(objects.choices.len().saturating_sub(1));
        }
        Some(Panel::Models(models)) => {
            let row_count = models.row_count();
            match &mut models.screen {
                ModelScreen::List { selected } => {
                    *selected = (*selected + 1).min(row_count.saturating_sub(1));
                }
                ModelScreen::Add { selected } => {
                    *selected = (*selected + 1).min(ConnectionKind::ALL.len().saturating_sub(1));
                }
                ModelScreen::Setup(_) | ModelScreen::Login { .. } => {}
            }
        }
        _ => {}
    }
}

pub(super) fn activate(state: &mut UiState, effects: &mut Vec<Effect>) {
    match &state.panel {
        Some(Panel::Objects(objects)) => {
            let selected = objects.selected;
            select_object(state, selected, effects);
        }
        Some(Panel::Models(ModelPanel {
            screen: ModelScreen::List { selected },
            ..
        })) => {
            let selected = *selected;
            select_model(state, selected, effects);
        }
        Some(Panel::Models(ModelPanel {
            screen: ModelScreen::Add { selected },
            ..
        })) => {
            let selected = *selected;
            choose_connection_kind(state, selected);
        }
        Some(Panel::Models(ModelPanel {
            screen: ModelScreen::Setup(_),
            ..
        })) => save_connection(state, effects),
        _ => {}
    }
}

pub(super) fn scroll_reader(state: &mut UiState, amount: isize, max: usize) {
    if let Some(Panel::Reader(reader)) = &mut state.panel {
        reader.scroll = reader
            .scroll
            .min(max)
            .saturating_add_signed(amount)
            .min(max);
    }
}

pub(super) fn setup_text(state: &mut UiState, mut value: SecretText) {
    let Some(Panel::Models(ModelPanel {
        screen: ModelScreen::Setup(form),
        ..
    })) = &mut state.panel
    else {
        return;
    };
    if form.saving {
        return;
    }
    let text = form.text_mut();
    for ch in value.take().chars().filter(|ch| !ch.is_control()) {
        if text.len() + ch.len_utf8() <= 16 * 1024 {
            text.push(ch);
        }
    }
}

pub(super) fn setup_clear(state: &mut UiState) {
    if let Some(form) = setup_mut(state) {
        form.text_mut().clear();
    }
}

pub(super) fn setup_backspace(state: &mut UiState) {
    let Some(form) = setup_mut(state) else {
        return;
    };
    let text = form.text_mut();
    if let Some((byte, _)) = text.grapheme_indices(true).next_back() {
        text.truncate(byte);
    }
}

pub(super) fn move_setup_field(state: &mut UiState, forward: bool) {
    if let Some(form) = setup_mut(state) {
        form.move_field(forward);
    }
}

pub(super) fn select_setup_field(state: &mut UiState, field: SetupField) {
    let Some(form) = setup_mut(state) else {
        return;
    };
    if form.fields().contains(&field) {
        form.field = field;
    }
}

fn setup_mut(state: &mut UiState) -> Option<&mut ConnectionForm> {
    match &mut state.panel {
        Some(Panel::Models(ModelPanel {
            screen: ModelScreen::Setup(form),
            ..
        })) if !form.saving => Some(form),
        _ => None,
    }
}

pub(super) fn select_model(state: &mut UiState, index: usize, effects: &mut Vec<Effect>) {
    let Some(panel) = state.panel.take() else {
        return;
    };
    let Panel::Models(mut models) = panel else {
        state.panel = Some(panel);
        return;
    };
    if models.session != state.selected || models.busy(state.model_operation) {
        state.panel = Some(Panel::Models(models));
        return;
    }
    if index >= models.row_count() {
        state.panel = Some(Panel::Models(models));
        return;
    }
    let ModelScreen::List { selected } = &mut models.screen else {
        state.panel = Some(Panel::Models(models));
        return;
    };
    *selected = index;

    if let Some(choice) = models.choices.get(index) {
        let session = models.session;
        let request = state.generation();
        state.model_operation = Some(ModelOperation {
            session,
            request,
            kind: ModelOperationKind::Apply,
        });
        state.status = None;
        effects.push(Effect::SetModel {
            session,
            request,
            selection: choice.selection.clone(),
        });
    } else if let Some(profile) = index
        .checked_sub(models.choices.len())
        .and_then(|profile_index| models.profiles.get(profile_index))
    {
        let selection = models
            .choices
            .iter()
            .find(|choice| choice.selection.profile == profile.id)
            .map(|choice| choice.selection.clone());
        models.screen = ModelScreen::Setup(ConnectionForm::edit_selection(profile, selection));
        state.status = None;
    } else if index == models.row_count().saturating_sub(1) {
        models.screen = ModelScreen::Add { selected: 0 };
        state.status = None;
    }
    state.panel = Some(Panel::Models(models));
}

pub(super) fn choose_connection_kind(state: &mut UiState, index: usize) {
    let Some(kind) = ConnectionKind::ALL.get(index).copied() else {
        return;
    };
    let Some(panel) = state.panel.take() else {
        return;
    };
    let Panel::Models(mut models) = panel else {
        state.panel = Some(panel);
        return;
    };
    if models.session != state.selected || !matches!(models.screen, ModelScreen::Add { .. }) {
        state.panel = Some(Panel::Models(models));
        return;
    }
    let form = if kind.subscription() {
        models
            .profiles
            .iter()
            .find(|profile| profile.id == bone_app::ProfileId::chatgpt())
            .map(|profile| ConnectionForm::edit(profile, String::new()))
            .unwrap_or_else(|| ConnectionForm::new(kind))
    } else {
        ConnectionForm::new(kind)
    };
    models.screen = ModelScreen::Setup(form);
    state.panel = Some(Panel::Models(models));
    state.status = None;
}

pub(super) fn save_connection(state: &mut UiState, effects: &mut Vec<Effect>) {
    let Some(panel) = state.panel.take() else {
        return;
    };
    let Panel::Models(mut models) = panel else {
        state.panel = Some(panel);
        return;
    };
    if models.session != state.selected {
        state.panel = Some(Panel::Models(models));
        return;
    }
    let ModelScreen::Setup(form) = &mut models.screen else {
        state.panel = Some(Panel::Models(models));
        return;
    };
    if form.saving {
        state.panel = Some(Panel::Models(models));
        return;
    }
    let (profile, selection) = match form.validated() {
        Ok(value) => value,
        Err(error) => {
            state.panel = Some(Panel::Models(models));
            state.status = Some(Status::selection(state.selected, error));
            return;
        }
    };
    let request = state.generation();
    form.saving = true;
    form.request = request;
    form.key_was_sent = !form.key.is_empty();
    let key = (!form.key.is_empty()).then(|| SecretText::from(form.key.take()));
    let session = models.session;
    state.panel = Some(Panel::Models(models));
    state.status = None;
    effects.push(Effect::SaveConnection {
        request,
        session,
        profile,
        key,
        selection,
    });
}

fn return_to_models(state: &mut UiState, effects: &mut Vec<Effect>) {
    let Some(Panel::Models(models)) = &mut state.panel else {
        return;
    };
    models.screen = ModelScreen::List { selected: 0 };
    let session = models.session;
    load_models(state, session, effects);
    refresh_model_label(state, effects);
}

fn return_to_model_list(state: &mut UiState) {
    if let Some(Panel::Models(models)) = &mut state.panel {
        models.screen = ModelScreen::List { selected: 0 };
    }
}

pub(super) fn escape(state: &mut UiState, effects: &mut Vec<Effect>) -> bool {
    let Some(panel) = &state.panel else {
        return false;
    };
    match panel {
        Panel::Models(ModelPanel {
            screen: ModelScreen::Login { .. },
            ..
        }) => {
            effects.push(Effect::CancelLogin);
            state.status = None;
            return_to_models(state, effects);
        }
        Panel::Models(ModelPanel {
            screen: ModelScreen::Add { .. } | ModelScreen::Setup(_),
            ..
        }) => {
            state.status = None;
            return_to_model_list(state);
        }
        Panel::Models(ModelPanel {
            screen: ModelScreen::List { .. },
            ..
        })
        | Panel::Objects(_)
        | Panel::Help
        | Panel::Reader(_) => dismiss(state, effects),
    }
    true
}

pub(super) fn dismiss(state: &mut UiState, effects: &mut Vec<Effect>) {
    cancel_login(state, effects);
    state.panel = None;
}

fn replace(state: &mut UiState, panel: Panel, effects: &mut Vec<Effect>) {
    cancel_login(state, effects);
    state.panel = Some(panel);
}

fn cancel_login(state: &UiState, effects: &mut Vec<Effect>) {
    if matches!(
        &state.panel,
        Some(Panel::Models(ModelPanel {
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
    use crate::state::{Action, Focus, UiEvent, update};

    fn choice(model: &str) -> ModelChoice {
        ModelChoice {
            selection: bone_app::ModelSelection::new(bone_app::ProfileId::chatgpt(), model)
                .unwrap(),
            profile_label: "ChatGPT".into(),
        }
    }

    fn request(state: &UiState, kind: ModelOperationKind) -> u64 {
        let operation = state.model_operation.expect("active model operation");
        assert_eq!(operation.kind, kind);
        operation.request
    }

    fn ready_models(state: &mut UiState, choices: Vec<ModelChoice>) -> u64 {
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
        models.screen = ModelScreen::Setup(form);
        state.panel = Some(Panel::Models(models));
    }

    #[test]
    fn each_panel_instance_owns_only_its_legal_navigation_state() {
        let session = SessionId::new();
        let mut state = UiState::default();
        state.panel = Some(Panel::Objects(ObjectPanel {
            session,
            choices: vec![
                (ReaderSource::History(SessionSeq(1)), "one".into()),
                (ReaderSource::History(SessionSeq(2)), "two".into()),
            ],
            selected: 1,
        }));
        panel_previous(&mut state);
        assert!(matches!(
            &state.panel,
            Some(Panel::Objects(objects)) if objects.selected == 0
        ));

        let content = ReaderContent {
            session,
            source: ReaderSource::History(SessionSeq(1)),
            title: "reader".into(),
            text: "body".into(),
            layout_cache: std::cell::RefCell::new(None),
        };
        state.panel = Some(Panel::Reader(ReaderPanel { content, scroll: 7 }));
        panel_next(&mut state);
        scroll_reader(&mut state, -2, 20);
        assert!(matches!(
            &state.panel,
            Some(Panel::Reader(reader)) if reader.scroll == 5
        ));

        let mut models = ModelPanel::new(None);
        models.screen = ModelScreen::Add { selected: 3 };
        state.panel = Some(Panel::Models(models));
        panel_previous(&mut state);
        assert!(matches!(
            &state.panel,
            Some(Panel::Models(ModelPanel {
                screen: ModelScreen::Add { selected: 2 },
                ..
            }))
        ));
    }

    #[test]
    fn closing_a_panel_never_rewrites_workspace_focus() {
        let mut state = UiState::default();
        state.set_focus(Focus::Sessions);
        open_help(&mut state, &mut Vec::new());
        dismiss(&mut state, &mut Vec::new());
        assert!(state.panel.is_none());
        assert_eq!(state.focus, Focus::Sessions);
        assert_eq!(state.last_center_focus(), Focus::Composer);
    }

    #[test]
    fn replacement_cancels_login_once_before_starting_new_panel_work() {
        let mut state = UiState::default();
        let mut models = ModelPanel::new(None);
        models.screen = ModelScreen::Login {
            request: 41,
            state: LoginState::Connecting,
        };
        state.panel = Some(Panel::Models(models));
        let mut effects = Vec::new();

        open_models(&mut state, &mut effects);

        assert!(matches!(
            effects.as_slice(),
            [
                Effect::CancelLogin,
                Effect::LoadModels { session: None, .. }
            ]
        ));
        assert!(matches!(
            &state.panel,
            Some(Panel::Models(ModelPanel {
                screen: ModelScreen::List { .. },
                ..
            }))
        ));

        effects.clear();
        open_help(&mut state, &mut effects);
        assert!(effects.is_empty());
        assert!(matches!(state.panel, Some(Panel::Help)));
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
            &state.panel,
            Some(Panel::Models(models)) if models.choices.is_empty()
        ));

        models_loaded(&mut state, None, current, vec![choice("fresh")], vec![]);
        assert!(state.model_operation.is_none());
        assert!(matches!(
            &state.panel,
            Some(Panel::Models(models))
                if models.choices.first().unwrap().selection.model == "fresh"
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
            [Effect::SetModel { request, .. }] if *request == apply
        ));
        effects.clear();
        select_model(&mut state, 0, &mut effects);
        activate(&mut state, &mut effects);
        assert!(effects.is_empty());
        assert_eq!(request(&state, ModelOperationKind::Apply), apply);
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
        state.model_label = Some("visible model".into());
        effects.clear();

        model_applied(
            &mut state,
            None,
            old_apply,
            Some("stale model".into()),
            None,
            Some("stale error".into()),
            &mut effects,
        );

        assert_eq!(request(&state, ModelOperationKind::Load), current_load);
        assert_eq!(state.status_text(), Some("new panel status"));
        assert_eq!(state.model_label.as_deref(), Some("visible model"));
        assert!(matches!(
            &state.panel,
            Some(Panel::Models(models)) if models.choices.is_empty()
        ));
        assert!(matches!(
            effects.as_slice(),
            [Effect::LoadModelLabel { .. }]
        ));
    }

    #[test]
    fn stale_workspace_apply_reconciles_the_current_session() {
        let session = SessionId::new();
        let mut state = UiState::default();
        state.selected = Some(session);
        state.panel = Some(Panel::Models(ModelPanel::new(Some(session))));
        state.model_label = Some("visible model".into());
        let mut effects = Vec::new();
        load_models(&mut state, Some(session), &mut effects);
        let operation = state.model_operation.expect("current load");
        effects.clear();

        model_applied(
            &mut state,
            None,
            operation.request.wrapping_add(1),
            Some("stale workspace model".into()),
            None,
            None,
            &mut effects,
        );

        assert_eq!(state.model_operation, Some(operation));
        assert_eq!(state.model_label.as_deref(), Some("visible model"));
        assert!(matches!(
            effects.as_slice(),
            [Effect::LoadModelLabel {
                session: Some(current),
                ..
            }] if *current == session
        ));
    }

    #[test]
    fn matching_apply_invalidates_older_label_reads_and_only_closes_its_list() {
        let mut state = UiState::default();
        let mut effects = Vec::new();
        refresh_model_label(&mut state, &mut effects);
        let stale_label = state.model_label_request;
        ready_models(&mut state, vec![choice("new")]);
        select_model(&mut state, 0, &mut Vec::new());
        let apply = request(&state, ModelOperationKind::Apply);

        model_applied(
            &mut state,
            None,
            apply,
            Some("new".into()),
            None,
            None,
            &mut Vec::new(),
        );
        assert!(state.panel.is_none());
        assert_eq!(state.model_label.as_deref(), Some("new"));
        assert_ne!(state.model_label_request, stale_label);

        update(
            &mut state,
            UiEvent::ModelLabelLoaded {
                session: None,
                request: stale_label,
                label: Some("old".into()),
                facts: None,
            },
        );
        assert_eq!(state.model_label.as_deref(), Some("new"));
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
            Some("new".into()),
            None,
            None,
            &mut Vec::new(),
        );

        assert_eq!(state.status_text(), Some("newer status"));
    }

    #[test]
    fn failed_apply_keeps_authoritative_facts_and_its_error_during_reload() {
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
            Some("new-saved".into()),
            Some(ModelFacts {
                saved: Ok(saved),
                running: Some(running),
            }),
            Some("request failed".into()),
            &mut effects,
        );

        let reload = request(&state, ModelOperationKind::Load);
        assert_ne!(reload, apply);
        assert!(matches!(
            effects.as_slice(),
            [Effect::LoadModels { request, .. }] if *request == reload
        ));
        assert_eq!(state.model_footer(), "old-running · saved change");
        assert!(
            state
                .model_configuration_summary()
                .contains("Saved, not running: chatgpt/new-saved")
        );
        assert_eq!(state.status_text(), Some("request failed"));
        models_failed(&mut state, None, reload, "catalogue failed".into());
        assert_eq!(state.status_text(), Some("request failed"));
    }

    #[test]
    fn named_model_starts_only_an_apply_and_failure_uses_a_fresh_load() {
        let mut state = UiState::default();
        let mut effects = Vec::new();
        set_named_model(&mut state, "profile".into(), "model".into(), &mut effects);
        let apply = request(&state, ModelOperationKind::Apply);
        assert!(matches!(
            effects.as_slice(),
            [Effect::SetNamedModel { request, .. }] if *request == apply
        ));
        effects.clear();
        model_applied(
            &mut state,
            None,
            apply,
            None,
            None,
            Some("apply failed".into()),
            &mut effects,
        );
        let reload = request(&state, ModelOperationKind::Load);
        assert_ne!(reload, apply);
        assert!(matches!(
            effects.as_slice(),
            [Effect::LoadModels { request, .. }] if *request == reload
        ));
    }

    #[test]
    fn cancelled_login_reloads_saved_catalogue_and_rejects_its_late_receipt() {
        let mut state = UiState::default();
        let mut models = ModelPanel::new(None);
        models.screen = ModelScreen::Login {
            request: 41,
            state: LoginState::Connecting,
        };
        state.panel = Some(Panel::Models(models));
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
                .any(|effect| matches!(effect, Effect::LoadModelLabel { .. }))
        );
        assert!(matches!(
            &state.panel,
            Some(Panel::Models(ModelPanel {
                screen: ModelScreen::List { selected: 0 },
                ..
            }))
        ));

        login_changed(&mut state, 41, LoginState::Succeeded, &mut Vec::new());
        assert!(matches!(
            &state.panel,
            Some(Panel::Models(ModelPanel {
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
        let mut form = ConnectionForm::new(ConnectionKind::OpenAiResponses);
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
        connection_saved(
            &mut state,
            request,
            session,
            Some("could not apply connection".into()),
            None,
            &mut Vec::new(),
        );

        assert!(state.status_text().unwrap().contains("Re-enter"));
        assert!(matches!(
            &state.panel,
            Some(Panel::Models(ModelPanel {
                screen: ModelScreen::Setup(form),
                ..
            })) if !form.saving && form.key.is_empty()
        ));
        assert_eq!(state.orphan_draft.text(), "ordinary draft");
        assert!(state.model_operation.is_none());
        let mut retry = Vec::new();
        save_connection(&mut state, &mut retry);
        assert!(retry.is_empty());
        assert!(state.status_text().unwrap().contains("Re-enter"));
    }

    #[test]
    fn subscription_save_and_login_success_follow_one_owned_screen_lifecycle() {
        let mut state = UiState::default();
        setup_panel(
            &mut state,
            ConnectionForm::new(ConnectionKind::ChatGptSubscription),
        );
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
        effects.clear();
        connection_saved(
            &mut state,
            request,
            session,
            None,
            Some("saved".into()),
            &mut effects,
        );
        let [Effect::Login { request: login, .. }] = effects.as_slice() else {
            panic!("login effect")
        };
        let login = *login;
        assert!(matches!(
            &state.panel,
            Some(Panel::Models(ModelPanel {
                screen: ModelScreen::Login {
                    request,
                    state: LoginState::Connecting
                },
                ..
            })) if *request == login
        ));

        login_changed(
            &mut state,
            login.wrapping_add(1),
            LoginState::Succeeded,
            &mut Vec::new(),
        );
        assert!(matches!(
            &state.panel,
            Some(Panel::Models(ModelPanel {
                screen: ModelScreen::Login { request, .. },
                ..
            })) if *request == login
        ));

        effects.clear();
        login_changed(&mut state, login, LoginState::Succeeded, &mut effects);
        assert!(matches!(
            &state.panel,
            Some(Panel::Models(ModelPanel {
                screen: ModelScreen::List { selected: 0 },
                ..
            }))
        ));
        assert_eq!(effects.len(), 2);
        assert!(matches!(effects[0], Effect::LoadModels { .. }));
        assert!(matches!(effects[1], Effect::LoadModelLabel { .. }));
    }

    #[test]
    fn successful_connection_save_does_not_clear_a_newer_unowned_status() {
        let mut state = UiState::default();
        let mut form = ConnectionForm::new(ConnectionKind::OpenAiResponses);
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

        connection_saved(&mut state, request, session, None, None, &mut Vec::new());

        assert_eq!(state.status_text(), Some("newer status"));
    }

    #[test]
    fn late_connection_receipt_preserves_a_new_form_and_its_status() {
        let mut state = UiState::default();
        let mut old = ConnectionForm::new(ConnectionKind::OpenAiResponses);
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

        setup_panel(
            &mut state,
            ConnectionForm::new(ConnectionKind::AnthropicMessages),
        );
        state.status = Some("new validation".into());
        let mut receipts = Vec::new();
        connection_saved(
            &mut state,
            old_request,
            old_session,
            None,
            Some("old notice".into()),
            &mut receipts,
        );

        assert_eq!(state.status_text(), Some("new validation"));
        assert!(matches!(
            &state.panel,
            Some(Panel::Models(ModelPanel {
                screen: ModelScreen::Setup(form),
                ..
            })) if form.kind == ConnectionKind::AnthropicMessages
        ));
        assert!(matches!(
            receipts.as_slice(),
            [Effect::LoadModelLabel { .. }]
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
        state.set_focus(Focus::Sessions);
        let mut models = ModelPanel::new(Some(first));
        models.screen = ModelScreen::Login {
            request: 7,
            state: LoginState::Connecting,
        };
        state.panel = Some(Panel::Models(models));

        let effects = update(&mut state, UiEvent::Action(Action::SelectSession(second)));

        assert_eq!(state.selected, Some(second));
        assert_eq!(state.focus, Focus::Sessions);
        assert!(state.panel.is_none());
        assert_eq!(
            effects
                .iter()
                .filter(|effect| matches!(effect, Effect::CancelLogin))
                .count(),
            1
        );
    }
}
