use bone_app::{RequestId, SessionId, SubmitInput};
use unicode_segmentation::UnicodeSegmentation;

use super::{
    answer::{self, AnswerDraft, RecoveryCandidate},
    model::*,
    protocol::*,
};

pub fn update(state: &mut UiState, event: UiEvent) -> Vec<Effect> {
    if matches!(event, UiEvent::Tick | UiEvent::Action(Action::Noop)) {
        return Vec::new();
    }
    // Scheduling background work does not change the screen. In particular,
    // the 500ms draft timer must not continually re-show the native caret.
    if !matches!(
        event,
        UiEvent::PersistDraftsRequested | UiEvent::RefreshOverviewRequested
    ) {
        state.dirty = true;
    }
    let mut effects = Vec::new();
    match event {
        UiEvent::ConnectionSaved {
            request,
            session,
            error,
            notice,
        } => {
            if state.selected == session
                && matches!(state.panel, Some(Panel::ModelSetup))
                && state
                    .connection_form
                    .as_ref()
                    .is_some_and(|form| form.saving && form.request == request)
            {
                if let Some(error) = error {
                    let form = state.connection_form.as_mut().unwrap();
                    form.saving = false;
                    state.status = Some(if form.key_was_sent {
                        format!("{error} Re-enter the API key before retrying.")
                    } else {
                        error
                    });
                    // App writes may persist before a later live reload fails.
                    // Refresh authoritative facts without leaving the failed form.
                    refresh_model_label(state, &mut effects);
                } else {
                    let subscription = state.connection_form.as_ref().unwrap().kind.subscription();
                    state.connection_form = None;
                    state.status = None;
                    if subscription {
                        state.panel = Some(Panel::Login);
                        state.login_state = bone_app::LoginState::Connecting;
                        let request = state.generation();
                        state.login_request = Some(request);
                        effects.push(Effect::Login {
                            profile: bone_app::ProfileId::chatgpt(),
                            request,
                        });
                    } else {
                        return_to_models(state, &mut effects);
                    }
                    state.status = notice;
                }
            } else if state.selected == session {
                // A cancelled form can still finish a durable App write. Reload
                // facts without reviving its panel, secret, error or authorization.
                state.model_request = state.generation();
                effects.push(Effect::LoadModels {
                    session,
                    request: state.model_request,
                });
                refresh_model_label(state, &mut effects);
            }
        }

        UiEvent::LoginChanged {
            request,
            state: login,
        } => {
            if state.login_request == Some(request) && matches!(state.panel, Some(Panel::Login)) {
                let succeeded = matches!(login, bone_app::LoginState::Succeeded);
                state.login_state = login;
                if succeeded {
                    state.login_request = None;
                    return_to_models(state, &mut effects);
                }
            }
        }
        UiEvent::ModelLabelLoaded {
            session,
            request,
            label,
            facts,
        } => {
            if state.selected == session && state.model_label_request == request {
                state.model_label = label;
                state.model_facts = facts;
            }
        }
        UiEvent::ModelsFailed {
            session,
            request,
            error,
        } => {
            if state.selected == session && state.model_request == request {
                state.models_loading = false;
                if matches!(state.panel, Some(Panel::Models)) {
                    state.status = Some(error);
                }
            }
        }
        UiEvent::ModelsLoaded {
            session,
            request,
            choices,
            profiles,
        } => {
            if state.selected == session && state.model_request == request {
                state.model_choices = choices;
                state.model_profiles = profiles;
                state.models_loading = false;
                if matches!(state.panel, Some(Panel::Models)) {
                    state.panel_selection = state
                        .panel_selection
                        .min(state.model_row_count().saturating_sub(1));
                }
            }
        }
        UiEvent::ModelApplied {
            session,
            request,
            label,
            facts,
            error,
        } => {
            if state.selected == session && state.model_request == request {
                state.models_loading = false;
                state.model_label_request = state.generation();
                state.model_label = label;
                state.model_facts = facts;
                state.status = error;
                if state.status.is_some() {
                    effects.push(Effect::LoadModels { session, request });
                } else if matches!(state.panel, Some(Panel::Models)) {
                    close_panel(state);
                }
            } else if state.selected == session {
                // The write outlived its panel. Query current App facts instead
                // of accepting stale facts or changing the newer panel state.
                state.model_label_request = state.generation();
                effects.push(Effect::LoadModelLabel {
                    session,
                    request: state.model_label_request,
                });
            }
        }
        UiEvent::Action(action) => handle_action(state, action, &mut effects),
        UiEvent::WorkspaceOpened {
            id,
            label,
            sessions,
            last_active,
            model_label,
            statuses,
        } => {
            state.workspace = Some((id, label));
            state.model_label = model_label;
            state.session_statuses = statuses;
            state.sessions = sessions;
            let preferred = last_active.filter(|candidate| {
                state
                    .sessions
                    .iter()
                    .any(|s| s.id == *candidate && !s.archived)
            });
            if let Some(session) =
                preferred.or_else(|| state.sessions.iter().find(|s| !s.archived).map(|s| s.id))
            {
                select_session(state, session, &mut effects);
            }
        }
        UiEvent::SessionOpened {
            session,
            generation,
            snapshot,
            history,
        } => {
            if let Some(ui) = current_generation_mut(state, session, generation) {
                if !ui.hydrated {
                    if ui.draft.is_empty() {
                        ui.draft = snapshot.draft.clone();
                    } else if !snapshot.draft.is_empty() && ui.draft != snapshot.draft {
                        ui.draft = format!("{}\n{}", snapshot.draft, ui.draft);
                        ui.draft_revision = ui.draft_revision.wrapping_add(1);
                    }
                    ui.saved_draft = snapshot.draft.clone();
                    ui.draft_cursor = ui.draft.len();
                    ui.hydrated = true;
                }
                ui.snapshot = Some(snapshot);
                ui.history.clear();
                ui.history_bytes = 0;
                for entry in history.items {
                    ui.history_bytes = ui.history_bytes.saturating_add(history_entry_bytes(&entry));
                    ui.history.push_back(entry);
                }
                ui.history_cursor = history.snapshot_through;
                ui.older_cursor = history.older_cursor;
                ui.scroll_from_tail = 0;
                trim_history_front(ui);
                if let Some(pending) = ui.bootstrap_submission.take() {
                    let input = SubmitInput {
                        request_id: pending.request_id,
                        text: pending.text.clone(),
                        reply_to: None,
                    };
                    ui.submitting = Some(pending);
                    effects.push(Effect::Submit {
                        session,
                        generation: ui.generation,
                        input,
                    });
                }
            }
        }
        UiEvent::SessionChanged {
            session,
            generation,
            snapshot,
        } => {
            let selected = state.selected == Some(session);
            if selected
                && snapshot.session.id == session
                && state
                    .session_ui
                    .get(&session)
                    .is_some_and(|ui| ui.generation == generation)
                && let Some(Panel::Reader(content)) = &mut state.panel
            {
                content.refresh_job(&snapshot);
            }
            if let Some(ui) = current_generation_mut(state, session, generation) {
                let through = snapshot.history_through;
                ui.info = snapshot.session.clone();
                ui.snapshot = Some(snapshot);
                if through > ui.history_cursor && !ui.history_loading && !ui.newer_history_missing {
                    if ui.read_anchor.is_some() || ui.scroll_from_tail > 0 || ui.older_loading {
                        ui.newer_history_missing = true;
                    } else {
                        ui.history_loading = true;
                        effects.push(Effect::LoadHistory {
                            session,
                            generation,
                            after: ui.history_cursor,
                        });
                    }
                }
                if !selected {
                    ui.unread = ui.unread.saturating_add(1);
                }
            }
            if let Some(info) = state.session_ui.get(&session).map(|ui| ui.info.clone())
                && let Some(listed) = state.sessions.iter_mut().find(|item| item.id == session)
            {
                *listed = info;
            }
        }
        UiEvent::HistoryLoaded {
            session,
            generation,
            page,
        } => {
            if let Some(ui) = current_generation_mut(state, session, generation) {
                ui.history_loading = false;
                if ui.read_anchor.is_some() || ui.scroll_from_tail > 0 || ui.older_loading {
                    ui.newer_history_missing = true;
                    return effects;
                }
                for entry in page.items {
                    if ui
                        .history
                        .back()
                        .is_none_or(|old| old.sequence < entry.sequence)
                    {
                        ui.history_bytes =
                            ui.history_bytes.saturating_add(history_entry_bytes(&entry));
                        ui.history.push_back(entry);
                    }
                }
                ui.history_cursor = ui.history_cursor.max(page.next_cursor);
                trim_history_front(ui);
                if page.has_more {
                    ui.history_loading = true;
                    effects.push(Effect::LoadHistory {
                        session,
                        generation,
                        after: ui.history_cursor,
                    });
                }
            }
        }
        UiEvent::OlderHistoryLoaded {
            session,
            generation,
            page,
        } => {
            if let Some(ui) = current_generation_mut(state, session, generation) {
                ui.older_loading = false;
                let metrics = ui.older_metrics.take();
                ui.older_cursor = page.older_cursor;
                for entry in page.items.into_iter().rev() {
                    if ui
                        .history
                        .front()
                        .is_none_or(|old| entry.sequence < old.sequence)
                    {
                        ui.history_bytes =
                            ui.history_bytes.saturating_add(history_entry_bytes(&entry));
                        ui.history.push_front(entry);
                    }
                }
                let evicted = trim_history_back(ui);
                if let Some(metrics) = metrics {
                    let evicted_rows = evicted
                        .iter()
                        .filter_map(|sequence| metrics.event_rows.get(sequence))
                        .sum::<usize>();
                    ui.scroll_from_tail = ui.scroll_from_tail.saturating_sub(evicted_rows);
                }
            }
        }
        UiEvent::RecentHistoryReloaded {
            session,
            generation,
            page,
        } => {
            if let Some(ui) = current_generation_mut(state, session, generation) {
                if ui.read_anchor.is_some() {
                    ui.recent_loading = false;
                    ui.newer_history_missing = true;
                    return effects;
                }
                ui.history.clear();
                ui.history_bytes = 0;
                for entry in page.items {
                    ui.history_bytes = ui.history_bytes.saturating_add(history_entry_bytes(&entry));
                    ui.history.push_back(entry);
                }
                ui.history_cursor = page.snapshot_through;
                ui.older_cursor = page.older_cursor;
                ui.scroll_from_tail = 0;
                ui.read_anchor = None;
                ui.transcript_metrics = None;
                ui.recent_loading = false;
                ui.newer_history_missing = false;
                trim_history_front(ui);
            }
        }
        UiEvent::PersistDraftsRequested => {
            for ui in state.session_ui.values() {
                if ui.hydrated && ui.draft_revision > ui.saved_draft_revision {
                    effects.push(Effect::SaveDraft {
                        session: ui.info.id,
                        generation: ui.generation,
                        revision: ui.draft_revision,
                        text: ui.draft.clone(),
                    });
                }
            }
        }
        UiEvent::RefreshOverviewRequested => {
            if !state.overview_pending {
                state.overview_pending = true;
                state.overview_generation = state.overview_generation.wrapping_add(1).max(1);
                effects.push(Effect::RefreshOverview {
                    generation: state.overview_generation,
                });
            }
        }
        UiEvent::OverviewLoaded {
            generation,
            sessions,
            statuses,
        } => {
            if generation != state.overview_generation {
                return effects;
            }
            state.overview_pending = false;
            state.sessions = sessions;
            state.session_statuses = statuses;
            if state
                .selected
                .is_some_and(|id| !state.sessions.iter().any(|s| s.id == id && !s.archived))
            {
                state.selected = None;
                refresh_model_label(state, &mut effects);
            }
            if state.session_candidate.is_some_and(|id| {
                !state
                    .sessions
                    .iter()
                    .any(|info| info.id == id && !info.archived)
            }) {
                state.session_candidate = state.selected;
                state.session_scroll = None;
            }
        }
        UiEvent::DraftSaved {
            session,
            generation,
            revision,
            text,
        } => {
            if let Some(ui) = current_generation_mut(state, session, generation)
                && revision >= ui.saved_draft_revision
            {
                ui.saved_draft_revision = revision;
                ui.saved_draft = text;
            }
        }
        UiEvent::Submitted {
            session,
            generation: _,
            request_id,
            ..
        } => {
            if let Some(ui) = state.session_ui.get_mut(&session)
                && ui
                    .submitting
                    .as_ref()
                    .is_some_and(|pending| pending.request_id == request_id)
                && let Some(pending) = ui.submitting.take()
            {
                if let Some(question) = pending.reply_to {
                    if let Some(answer) = ui.answer_drafts.get_mut(&question) {
                        let cleared = answer.clear_if_submitted(
                            question,
                            pending.draft_revision,
                            &pending.text,
                        );
                        if cleared && ui.selected_answer == Some(question) {
                            ui.selected_answer = None;
                        }
                    }
                } else {
                    if ui.draft_revision == pending.draft_revision && ui.draft == pending.text {
                        ui.editor.checkpoint(&ui.draft, ui.draft_cursor);
                        ui.draft.clear();
                        ui.draft_cursor = 0;
                        ui.draft_revision = ui.draft_revision.wrapping_add(1);
                        effects.push(Effect::SaveDraft {
                            session,
                            generation: ui.generation,
                            revision: ui.draft_revision,
                            text: String::new(),
                        });
                    }
                    effects.push(Effect::AutoTitle {
                        session,
                        generation: ui.generation,
                        first_input: pending.text,
                    });
                }
            }
        }
        UiEvent::SubmitFailed {
            session,
            generation: _,
            request_id,
            message,
        } => {
            if let Some(ui) = state.session_ui.get_mut(&session)
                && let Some(pending) = &mut ui.submitting
                && pending.request_id == request_id
            {
                pending.failed = true;
                if state.selected == Some(session) {
                    state.status = Some(message);
                }
            }
        }
        UiEvent::SessionCreated { request_id, info } => {
            if state
                .pending_create
                .as_ref()
                .is_some_and(|pending| pending.request_id == request_id)
            {
                let pending = state
                    .pending_create
                    .take()
                    .expect("matching pending create");
                let transferred = pending.first_input.as_ref().map(|_| {
                    let text = std::mem::take(&mut state.orphan_draft);
                    let cursor = state.orphan_cursor;
                    let revision = state.orphan_revision;
                    state.orphan_cursor = 0;
                    state.orphan_revision = state.orphan_revision.wrapping_add(1);
                    let editor = std::mem::take(&mut state.orphan_editor);
                    (text, cursor, revision, editor)
                });
                if transferred.is_none() {
                    clear_create_source(state, pending.source, &mut effects);
                }
                if !state.sessions.iter().any(|session| session.id == info.id) {
                    state.sessions.insert(0, info.clone());
                }
                select_session(state, info.id, &mut effects);
                if let Some((text, cursor, revision, editor)) = transferred
                    && let Some(ui) = state.session_ui.get_mut(&info.id)
                {
                    ui.editor = editor;
                    ui.draft = text;
                    ui.draft_cursor = cursor;
                    ui.draft_revision = revision;
                    ui.bootstrap_submission = pending.first_input;
                }
                state.focus = Focus::Composer;
                if matches!(
                    state.status.as_deref(),
                    Some(
                        "Creating session; your input is preserved"
                            | "Confirming session creation; your input is preserved"
                            | "Opening the new session; your input is preserved"
                    )
                ) {
                    state.status = None;
                }
            }
        }
        UiEvent::SessionCreateFailed {
            request_id,
            message,
        } => {
            if let Some(pending) = &mut state.pending_create
                && pending.request_id == request_id
            {
                pending.failed = true;
                state.status = Some(message);
            }
        }
        UiEvent::SessionRenamed {
            session,
            generation,
            title,
        } => {
            if current_generation_mut(state, session, generation).is_some() {
                if let Some(info) = state.sessions.iter_mut().find(|info| info.id == session) {
                    info.title = title.clone();
                }
                if let Some(ui) = state.session_ui.get_mut(&session) {
                    ui.info.title = title;
                }
            }
        }
        UiEvent::SessionReleased {
            generation: _,
            receipt,
        } => {
            let released = matches!(
                receipt.status,
                bone_app::SessionReleaseStatus::Released | bone_app::SessionReleaseStatus::NotOpen
            );
            if released && let Some(ui) = state.session_ui.get_mut(&receipt.session) {
                ui.snapshot = None;
                ui.hydrated = false;
            }
            if state.selected == Some(receipt.session) {
                let next = state.generation();
                if let Some(ui) = state.session_ui.get_mut(&receipt.session) {
                    ui.generation = next;
                }
                effects.push(Effect::OpenSession {
                    session: receipt.session,
                    generation: next,
                });
            }
        }
        UiEvent::OperationFailed {
            kind,
            session,
            generation,
            message,
        } => {
            if session.zip(generation).is_none_or(|(id, generation)| {
                state
                    .session_ui
                    .get(&id)
                    .is_some_and(|ui| ui.generation == generation)
            }) {
                if let Some(id) = session
                    && let Some(ui) = state.session_ui.get_mut(&id)
                {
                    match kind {
                        OperationKind::LoadOlderHistory => {
                            ui.older_loading = false;
                            ui.older_metrics = None;
                        }
                        OperationKind::LoadHistory => {
                            ui.history_loading = false;
                            ui.recent_loading = false;
                        }
                        _ => {}
                    }
                }
                if kind == OperationKind::RefreshOverview {
                    state.overview_pending = false;
                }
                if session.is_none() || state.selected == session {
                    state.status = Some(message);
                }
            }
        }
        UiEvent::Resized => {}
        UiEvent::Tick => unreachable!(),
    }
    trim_editor_history(state);
    trim_global_history(state);
    effects
}

fn handle_action(state: &mut UiState, action: Action, effects: &mut Vec<Effect>) {
    if !matches!(action, Action::Input(_)) {
        state.editor_mut().3.typing = false;
    }
    if !matches!(
        action,
        Action::CursorVertical { .. }
            | Action::MoveCursor {
                direction: -2 | 2,
                ..
            }
            | Action::Noop
    ) {
        state.preferred_column = None;
    }
    match action {
        Action::RenameText(text) => {
            state
                .rename_input
                .push_str(&text.replace(['\r', '\n'], " "));
        }
        Action::RenameBackspace => {
            if let Some((start, _)) = state.rename_input.grapheme_indices(true).next_back() {
                state.rename_input.truncate(start);
            }
        }
        Action::NewSession => run_palette_command(state, "new", effects),
        Action::OpenCommands => open_panel(state, Panel::Commands),
        Action::OpenModels => open_models(state, effects),
        Action::OpenHelp => open_panel(state, Panel::Help),
        Action::OpenLatest => open_latest(state),
        Action::SelectObject(index) => {
            state.panel_selection = index;
            open_object(state);
        }
        Action::OpenHistory(sequence) => {
            if let Some(ui) = state.selected_ui()
                && let Some(entry) = ui.history.iter().find(|entry| entry.sequence == sequence)
                && let Some(content) = super::reader::ReaderContent::from_history(ui.info.id, entry)
            {
                state.panel_return = Focus::Conversation;
                state.panel_scroll = 0;
                pin_reading(state);
                state.panel = Some(Panel::Reader(content));
            }
        }
        Action::OpenJob(job) => {
            if let Some(snapshot) = state.selected_ui().and_then(|ui| ui.snapshot.as_ref())
                && let Some(content) = super::reader::ReaderContent::from_job(snapshot, job)
            {
                state.panel_return = Focus::Conversation;
                state.panel_scroll = 0;
                pin_reading(state);
                state.panel = Some(Panel::Reader(content));
            }
        }
        Action::PanelPrevious => {
            state.panel_selection = state.panel_selection.saturating_sub(1);
        }
        Action::PanelNext => {
            let count = match &state.panel {
                Some(Panel::Commands) => COMMANDS.len(),
                Some(Panel::Objects { choices, .. }) => choices.len(),
                Some(Panel::ModelAdd) => super::ConnectionKind::ALL.len(),
                Some(Panel::Models) => state.model_row_count(),
                _ => 0,
            };
            state.panel_selection = (state.panel_selection + 1).min(count.saturating_sub(1));
        }
        Action::SelectModel(index) => select_model_row(state, index, effects),
        Action::SetupText(mut value) => {
            if matches!(state.panel, Some(Panel::ModelSetup))
                && let Some(form) = &mut state.connection_form
                && !form.saving
            {
                let text = form.text_mut();
                for ch in value.take().chars().filter(|ch| !ch.is_control()) {
                    if text.len() + ch.len_utf8() <= 16 * 1024 {
                        text.push(ch);
                    }
                }
            }
        }
        Action::SetupClear => {
            if matches!(state.panel, Some(Panel::ModelSetup))
                && let Some(form) = &mut state.connection_form
                && !form.saving
            {
                form.text_mut().clear();
            }
        }
        Action::SetupBackspace => {
            if matches!(state.panel, Some(Panel::ModelSetup))
                && let Some(form) = &mut state.connection_form
                && !form.saving
            {
                let text = form.text_mut();
                if let Some((byte, _)) = text.grapheme_indices(true).next_back() {
                    text.truncate(byte);
                }
            }
        }
        Action::NextField | Action::PreviousField => {
            if matches!(state.panel, Some(Panel::ModelSetup))
                && let Some(form) = &mut state.connection_form
                && !form.saving
            {
                form.move_field(matches!(action, Action::NextField));
            }
        }
        Action::SelectField(field) => {
            if matches!(state.panel, Some(Panel::ModelSetup))
                && let Some(form) = &mut state.connection_form
                && !form.saving
                && form.fields().contains(&field)
            {
                form.field = field;
            }
        }
        Action::ChooseConnectionKind(index) => choose_connection_kind(state, index),
        Action::SaveConnection => save_connection(state, effects),
        Action::ActivatePanel => {
            if matches!(state.panel, Some(Panel::Commands)) {
                if let Some(command) = COMMANDS.get(state.panel_selection) {
                    run_palette_command(state, command.name, effects);
                }
            } else if matches!(state.panel, Some(Panel::Models)) {
                apply_model(state, effects);
            } else if matches!(state.panel, Some(Panel::ModelAdd)) {
                choose_connection_kind(state, state.panel_selection);
            } else if matches!(state.panel, Some(Panel::ModelSetup)) {
                save_connection(state, effects);
            } else if matches!(state.panel, Some(Panel::Rename)) {
                apply_rename(state, effects);
            } else if matches!(state.panel, Some(Panel::Objects { .. })) {
                open_object(state);
            }
        }
        Action::ScrollPanel { amount, max } => {
            state.panel_scroll = state
                .panel_scroll
                .min(max)
                .saturating_add_signed(amount)
                .min(max);
        }

        Action::Noop => {}
        Action::Focus(focus) => state.focus = focus,
        Action::FocusLeft => state.focus = Focus::Sessions,
        Action::FocusRight => {
            if state.focus == Focus::Sessions {
                state.focus = Focus::Conversation
            }
        }
        Action::FocusUp => {
            if state.focus == Focus::Composer {
                state.focus = Focus::Conversation
            }
        }
        Action::FocusDown => {
            if state.focus == Focus::Conversation {
                state.focus = Focus::Composer
            }
        }
        Action::SelectPrevious => move_session(state, -1),
        Action::SelectNext => move_session(state, 1),
        Action::ScrollSessions { start } => state.session_scroll = Some(start),
        Action::OpenCandidate => {
            if let Some(session) = state.session_candidate.or(state.selected).or_else(|| {
                state
                    .sessions
                    .iter()
                    .find(|info| !info.archived)
                    .map(|info| info.id)
            }) {
                select_session(state, session, effects);
                state.focus = Focus::Conversation;
            }
        }
        Action::SelectSession(session) => {
            state.panel = None;
            select_session(state, session, effects);
            state.focus = Focus::Conversation;
        }
        Action::SelectSlashPrevious => move_slash(state, -1),
        Action::SelectSlashNext => move_slash(state, 1),
        Action::CompleteSlash => complete_slash(state),
        Action::ExecuteSlash(index) => {
            if matches!(state.panel, Some(Panel::Commands)) {
                state.panel_selection = index;
                if let Some(command) = COMMANDS.get(index) {
                    run_palette_command(state, command.name, effects);
                }
                return;
            }
            state.slash_selection = index;
            submit(state, effects);
        }
        Action::Input(value) => insert_text(state, &value.to_string(), true),
        Action::Paste(text) => insert_text(state, &text, false),
        Action::Backspace => delete_before_cursor(state),
        Action::Delete => delete_after_cursor(state),
        Action::Undo | Action::Redo => {
            let redo = matches!(action, Action::Redo);
            let (text, cursor, revision, editor) = state.editor_mut();
            editor.undo(text, cursor, revision, redo);
            state.status = None;
            state.slash_selection = 0;
            state.slash_dismissed = None;
        }
        Action::MoveCursor {
            direction,
            width,
            select,
            word,
        } => {
            let mut preferred = state.preferred_column;
            let (text, cursor, _, editor) = state.editor_mut();
            if select {
                editor.anchor.get_or_insert(*cursor);
            } else {
                editor.anchor = None;
            }
            *cursor = match direction {
                -1 | 1 if word => crate::text::word_cursor(text, *cursor, direction > 0),
                -1 | 1 => moved_cursor(text, *cursor, direction as isize),
                -2 | 2 => crate::text::vertical_cursor(
                    text,
                    *cursor,
                    width,
                    direction > 0,
                    &mut preferred,
                ),
                -3 | 3 => line_edge(text, *cursor, direction > 0),
                _ => *cursor,
            };
            state.preferred_column = preferred;
        }
        Action::DragCursor(byte) => {
            let (text, cursor, _, editor) = state.editor_mut();
            editor.anchor.get_or_insert(*cursor);
            *cursor = grapheme_boundary(text, byte);
        }
        Action::PlaceCursor(byte) => {
            state.focus = Focus::Composer;
            let (text, cursor, _, editor) = state.editor_mut();
            *cursor = grapheme_boundary(text, byte);
            editor.anchor = Some(*cursor);
        }
        Action::CursorLeft => move_cursor(state, -1),
        Action::CursorRight => move_cursor(state, 1),
        Action::CursorVertical { down, width } => {
            let mut preferred = state.preferred_column;
            let (text, cursor, _, editor) = state.editor_mut();
            editor.anchor = None;
            *cursor = crate::text::vertical_cursor(text, *cursor, width, down, &mut preferred);
            state.preferred_column = preferred;
        }
        Action::CursorHome => set_cursor_line_edge(state, false),
        Action::CursorEnd => set_cursor_line_edge(state, true),
        Action::InsertNewline => insert_text(state, "\n", false),
        Action::Submit => submit(state, effects),
        Action::ClickSubmit => {
            state.focus = Focus::Composer;
            submit(state, effects);
        }
        Action::AnswerQuestion(question) => {
            if let Some(ui) = state.selected_ui_mut() {
                if ui
                    .snapshot
                    .as_ref()
                    .is_some_and(|snapshot| answer::active_question(snapshot, question).is_some())
                {
                    ui.answer_drafts
                        .entry(question)
                        .or_insert_with(|| AnswerDraft::new(question));
                    ui.selected_answer = Some(question);
                    state.focus = Focus::Composer;
                    state.status = None;
                } else {
                    state.status = Some("This question is no longer active".into());
                }
            }
        }
        Action::LeaveAnswer => leave_answer(state),
        Action::ConvertAnswer => convert_answer(state),
        Action::RestoreInput(input) => recover_input(state, input, false, effects),
        Action::RetryInput(input) => recover_input(state, input, true, effects),
        Action::RetrySubmission => retry_submission(state, effects),
        Action::Escape => {
            if matches!(
                state.panel,
                Some(Panel::Login | Panel::ModelSetup | Panel::ModelAdd)
            ) {
                if matches!(state.panel, Some(Panel::Login)) {
                    effects.push(Effect::CancelLogin);
                }
                state.login_request = None;
                state.connection_form = None;
                state.panel = Some(Panel::Models);
                state.panel_selection = 0;
                state.status = None;
                return;
            }
            if state.panel.is_some() {
                close_panel(state);
                return;
            }
            if state.focus == Focus::Sessions {
                state.session_candidate = state.selected;
                state.session_scroll = None;
                state.focus = Focus::Conversation;
                return;
            }
            if !state.slash_matches().is_empty() {
                state.slash_dismissed = Some(state.draft_identity());
                state.status = None;
            } else if state
                .selected_ui()
                .is_some_and(|ui| ui.selected_answer.is_some())
            {
                leave_answer(state);
            } else {
                effects.extend(stop_selected(state));
            }
        }
        Action::ScrollUp { amount, metrics } => scroll_up(state, amount, metrics, effects),
        Action::ScrollDown(amount) => {
            if let Some(ui) = state.selected_ui_mut() {
                let target = ui.transcript_metrics.as_ref().map(|metrics| {
                    ui.read_anchor
                        .and_then(|anchor| metrics.row_for_anchor(anchor))
                        .unwrap_or(metrics.start_row)
                        .saturating_add(amount)
                });
                let follow_tail = ui
                    .transcript_metrics
                    .as_ref()
                    .zip(target)
                    .is_some_and(|(metrics, target)| target >= metrics.max_scroll());
                if follow_tail || ui.scroll_from_tail <= amount && ui.transcript_metrics.is_none() {
                    ui.read_anchor = None;
                    ui.scroll_from_tail = 0;
                    ui.unread = 0;
                    if ui.newer_history_missing && !ui.recent_loading {
                        ui.recent_loading = true;
                        effects.push(Effect::ReloadRecentHistory {
                            session: ui.info.id,
                            generation: ui.generation,
                        });
                    }
                } else {
                    ui.scroll_from_tail = ui.scroll_from_tail.saturating_sub(amount);
                    ui.read_anchor = ui
                        .transcript_metrics
                        .as_ref()
                        .zip(target)
                        .and_then(|(metrics, target)| metrics.anchor_at_start(target));
                }
            }
        }
        Action::Stop => effects.extend(stop_selected(state)),
        Action::Quit | Action::Terminate => {
            prepare_exit(state);
            effects.push(Effect::Shutdown);
        }
    }
}

fn prepare_exit(state: &mut UiState) {
    for ui in state.session_ui.values_mut() {
        for (_, answer) in std::mem::take(&mut ui.answer_drafts) {
            if !answer.text.is_empty() {
                ui.editor.checkpoint(&ui.draft, ui.draft_cursor);
                ui.draft = answer::append_restored_text(
                    &ui.draft,
                    &format!("[Unsent answer / 未发送回答]\n{}", answer.text),
                );
                ui.draft_cursor = ui.draft.len();
                ui.draft_revision = ui.draft_revision.wrapping_add(1);
            }
        }
        ui.selected_answer = None;
    }
    state.quitting = true;
}

fn submit(state: &mut UiState, effects: &mut Vec<Effect>) {
    if state.focus != Focus::Composer {
        return;
    }
    let text = state.draft().to_owned();
    if text.trim().is_empty() {
        return;
    }
    let answering = state
        .selected_ui()
        .is_some_and(|ui| ui.selected_answer.is_some());
    if !answering && let Some(command) = text.trim_start().strip_prefix('/') {
        execute_command(state, command, effects);
        return;
    }
    if state.selected_ui().is_none() {
        create_for_first_input(state, text, effects);
        return;
    }
    let ui = state.selected_ui_mut().expect("selected session checked");
    if ui.bootstrap_submission.is_some() {
        state.status = Some("Opening the new session; your input is preserved".into());
        return;
    }
    let reply_to = ui.selected_answer;
    if let Some(pending) = &ui.submitting {
        if !pending.failed {
            return;
        }
        if pending.text != text || pending.reply_to != reply_to {
            state.status = Some("Confirm the previous submission before sending new text".into());
            return;
        }
    }
    let request_id = ui
        .submitting
        .as_ref()
        .map_or_else(RequestId::new, |pending| pending.request_id);
    let (mut input, revision) = if let Some(question) = reply_to {
        let Some(answer) = ui.answer_drafts.get(&question) else {
            return;
        };
        let Some(snapshot) = ui.snapshot.as_ref() else {
            return;
        };
        match answer.submission(snapshot, request_id) {
            Ok(input) => (input, answer.revision),
            Err(_) => {
                state.status = Some("This question has ended. Your answer is preserved; convert it explicitly to send ordinary text".into());
                return;
            }
        }
    } else {
        (SubmitInput::new(text.clone()), ui.draft_revision)
    };
    input.request_id = request_id;
    ui.submitting = Some(PendingSubmission {
        request_id,
        text,
        draft_revision: revision,
        failed: false,
        reply_to,
    });
    effects.push(Effect::Submit {
        session: ui.info.id,
        generation: ui.generation,
        input,
    });
}

fn create_for_first_input(state: &mut UiState, text: String, effects: &mut Vec<Effect>) {
    if let Some(pending) = state.pending_create.as_mut() {
        if pending.failed && pending.first_input.is_some() {
            pending.failed = false;
            effects.push(Effect::CreateSession {
                request_id: pending.request_id,
                title: pending.title.clone(),
                provisional: pending.provisional,
            });
            state.status = Some("Confirming session creation; your input is preserved".into());
        } else if pending.failed {
            state.status =
                Some("Previous session creation is unresolved; use /new --retry first".into());
        }
        return;
    }
    let request_id = RequestId::new();
    let pending = PendingCreate {
        request_id,
        title: "New conversation".into(),
        provisional: true,
        failed: false,
        source: DraftSource::Orphan {
            revision: state.orphan_revision,
            text: text.clone(),
        },
        first_input: Some(PendingSubmission {
            request_id: RequestId::new(),
            text,
            draft_revision: state.orphan_revision,
            failed: false,
            reply_to: None,
        }),
    };
    effects.push(Effect::CreateSession {
        request_id,
        title: pending.title.clone(),
        provisional: true,
    });
    state.pending_create = Some(pending);
    state.status = Some("Creating session; your input is preserved".into());
}

fn leave_answer(state: &mut UiState) {
    if let Some(ui) = state.selected_ui_mut() {
        ui.selected_answer = None;
    }
    state.preferred_column = None;
    state.status = None;
}

fn convert_answer(state: &mut UiState) {
    if let Some(ui) = state.selected_ui_mut() {
        let Some(question) = ui.selected_answer else {
            return;
        };
        let Some(answer) = ui.answer_drafts.remove(&question) else {
            return;
        };
        ui.editor.checkpoint(&ui.draft, ui.draft_cursor);
        ui.draft = answer::append_restored_text(&ui.draft, &answer.text);
        ui.draft_cursor = ui.draft.len();
        ui.draft_revision = ui.draft_revision.wrapping_add(1);
        ui.selected_answer = None;
        state.status = Some("Answer copied to ordinary draft; review before sending".into());
        state.preferred_column = None;
    }
}

fn recover_input(
    state: &mut UiState,
    input: bone_app::InputId,
    retry: bool,
    effects: &mut Vec<Effect>,
) {
    let Some(ui) = state.selected_ui_mut() else {
        return;
    };
    let Some(snapshot) = &ui.snapshot else {
        return;
    };
    let candidate = answer::recoverable_inputs(snapshot, &ui.history)
        .into_iter()
        .find(|candidate| match candidate {
            RecoveryCandidate::Retry { input: id }
            | RecoveryCandidate::Restore { input: id, .. } => *id == input,
        });
    match candidate {
        Some(RecoveryCandidate::Retry { input }) if retry => {
            effects.push(Effect::RetryInput {
                session: ui.info.id,
                generation: ui.generation,
                input,
            });
            state.status = Some("Retry requested for the saved input".into());
        }
        Some(RecoveryCandidate::Restore { text, reply_to, .. }) if !retry => {
            if let Some(question) = reply_to {
                let answer = ui
                    .answer_drafts
                    .entry(question)
                    .or_insert_with(|| AnswerDraft::new(question));
                answer.replace(
                    answer::append_restored_text(&answer.text, &text),
                    usize::MAX,
                );
                ui.selected_answer = Some(question);
                state.status = Some("Answer restored with its original question; an expired answer cannot be sent without explicit conversion".into());
            } else {
                ui.editor.checkpoint(&ui.draft, ui.draft_cursor);
                ui.draft = answer::append_restored_text(&ui.draft, &text);
                ui.draft_cursor = ui.draft.len();
                ui.draft_revision = ui.draft_revision.wrapping_add(1);
                ui.selected_answer = None;
                state.status = Some("Input restored to your draft; review before sending".into());
            }
            state.focus = Focus::Composer;
            state.preferred_column = None;
        }
        _ => {
            state.status = Some(
                "This input has no matching recovery action; refresh or load its history".into(),
            )
        }
    }
}

fn retry_submission(state: &mut UiState, effects: &mut Vec<Effect>) {
    let Some(ui) = state.selected_ui_mut() else {
        return;
    };
    let Some(pending) = ui.submitting.as_mut().filter(|pending| pending.failed) else {
        return;
    };
    let input = SubmitInput {
        request_id: pending.request_id,
        text: pending.text.clone(),
        reply_to: pending.reply_to,
    };
    pending.failed = false;
    effects.push(Effect::Submit {
        session: ui.info.id,
        generation: ui.generation,
        input,
    });
    state.status = Some("Confirming the original submission; newer draft is preserved".into());
}

fn execute_command(state: &mut UiState, raw: &str, effects: &mut Vec<Effect>) {
    let trimmed = raw.trim();
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let typed = parts.next().unwrap_or_default();
    let argument = parts.next().unwrap_or_default().trim();
    let selected = if let Some(command) = COMMANDS.iter().find(|command| command.name == typed) {
        Some(*command)
    } else if argument.is_empty() {
        state
            .slash_matches()
            .get(state.slash_selection)
            .copied()
            .copied()
            .or_else(|| COMMANDS.iter().find(|spec| spec.name == typed).copied())
    } else {
        COMMANDS.iter().find(|spec| spec.name == typed).copied()
    };
    let Some(selected) = selected else {
        state.status = Some(format!("Unknown command: /{typed}"));
        return;
    };
    match selected.kind {
        CommandKind::Answer if argument.is_empty() => {
            clear_current_draft(state, effects);
            let question = state
                .selected_ui()
                .and_then(|ui| ui.snapshot.as_ref())
                .and_then(|snapshot| {
                    super::answer::active_questions(snapshot)
                        .first()
                        .map(|question| question.id)
                });
            if let Some(question) = question {
                handle_action(state, Action::AnswerQuestion(question), effects);
            } else {
                state.status = Some("No active question".into());
            }
        }
        CommandKind::Recover if argument.is_empty() => {
            clear_current_draft(state, effects);
            let input = state.selected_ui().and_then(|ui| {
                ui.snapshot.as_ref().and_then(|snapshot| {
                    super::answer::recoverable_inputs(snapshot, &ui.history)
                        .into_iter()
                        .rev()
                        .find_map(|candidate| match candidate {
                            super::answer::RecoveryCandidate::Restore { input, .. } => Some(input),
                            _ => None,
                        })
                })
            });
            if let Some(input) = input {
                handle_action(state, Action::RestoreInput(input), effects);
            } else {
                state.status = Some("No cancelled input to restore".into());
            }
        }
        CommandKind::Retry if argument.is_empty() => {
            clear_current_draft(state, effects);
            if state
                .selected_ui()
                .is_some_and(|ui| ui.submitting.as_ref().is_some_and(|pending| pending.failed))
            {
                handle_action(state, Action::RetrySubmission, effects);
            } else {
                let input = state.selected_ui().and_then(|ui| {
                    ui.snapshot.as_ref().and_then(|snapshot| {
                        super::answer::recoverable_inputs(snapshot, &ui.history)
                            .into_iter()
                            .rev()
                            .find_map(|candidate| match candidate {
                                super::answer::RecoveryCandidate::Retry { input } => Some(input),
                                _ => None,
                            })
                    })
                });
                if let Some(input) = input {
                    handle_action(state, Action::RetryInput(input), effects);
                } else {
                    state.status = Some("No input needs retry".into());
                }
            }
        }

        CommandKind::Model => {
            let parts: Vec<_> = argument.split_whitespace().collect();
            if parts.is_empty() {
                clear_current_draft(state, effects);
                open_models(state, effects);
            } else if parts.len() == 2 {
                let profile = parts[0].to_owned();
                let model = parts[1].to_owned();
                clear_current_draft(state, effects);
                open_models(state, effects);
                effects.push(Effect::SetNamedModel {
                    session: state.selected,
                    request: state.model_request,
                    profile,
                    model,
                });
            } else {
                state.status = Some("Usage: /model [profile model]".into());
            }
        }
        CommandKind::Details if argument.is_empty() => {
            clear_current_draft(state, effects);
            open_objects(state);
        }

        CommandKind::New => {
            if let Some(pending) = &state.pending_create {
                let retry = pending.failed
                    && (argument == "--retry"
                        || state.command_from_palette
                        || create_source_matches(state, &pending.source));
                if retry {
                    effects.push(Effect::CreateSession {
                        request_id: pending.request_id,
                        title: pending.title.clone(),
                        provisional: pending.provisional,
                    });
                } else if pending.failed {
                    state.status = Some(
                        "Previous session creation is unresolved; use /new --retry first".into(),
                    );
                }
                if retry {
                    state
                        .pending_create
                        .as_mut()
                        .expect("pending create")
                        .failed = false;
                }
                return;
            }
            let request_id = RequestId::new();
            let source = if state.command_from_palette {
                DraftSource::None
            } else {
                state.selected_ui().map_or_else(
                    || DraftSource::Orphan {
                        revision: state.orphan_revision,
                        text: state.orphan_draft.clone(),
                    },
                    |ui| DraftSource::Session {
                        id: ui.info.id,
                        generation: ui.generation,
                        revision: ui.draft_revision,
                        text: ui.draft.clone(),
                    },
                )
            };
            let title: String = if argument.is_empty() {
                "新会话".into()
            } else {
                argument.into()
            };
            let provisional = argument.is_empty();
            state.pending_create = Some(PendingCreate {
                first_input: None,
                request_id,
                source,
                title: title.clone(),
                provisional,
                failed: false,
            });
            effects.push(Effect::CreateSession {
                request_id,
                title,
                provisional,
            });
        }
        CommandKind::Sessions if argument.is_empty() => {
            state.focus = Focus::Sessions;
            clear_current_draft(state, effects);
        }
        CommandKind::Rename if argument.is_empty() => {
            if let Some(ui) = state.selected_ui() {
                state.rename_input = ui.info.title.clone();
                state.rename_target = state.selected_ui().map(|ui| (ui.info.id, ui.generation));
                clear_current_draft(state, effects);
                open_panel(state, Panel::Rename);
            } else {
                state.status = Some("There is no session to rename".into());
            }
        }
        CommandKind::Rename if !argument.is_empty() => {
            if let Some(ui) = state.selected_ui() {
                effects.push(Effect::RenameSession {
                    session: ui.info.id,
                    generation: ui.generation,
                    title: argument.into(),
                });
                clear_current_draft(state, effects);
            } else {
                state.status = Some("There is no session to rename".into());
            }
        }
        CommandKind::Help if argument.is_empty() => {
            clear_current_draft(state, effects);
            open_panel(state, Panel::Help);
        }
        CommandKind::Quit if argument.is_empty() => {
            clear_current_draft(state, effects);
            prepare_exit(state);
            effects.push(Effect::Shutdown);
        }
        _ => state.status = Some(format!("Usage: /{} {}", selected.name, selected.usage)),
    }
}

fn create_source_matches(state: &UiState, source: &DraftSource) -> bool {
    match source {
        DraftSource::None => false,
        DraftSource::Orphan { revision, text } => {
            state.selected.is_none()
                && state.orphan_revision == *revision
                && state.orphan_draft == *text
        }
        DraftSource::Session {
            id,
            generation,
            revision,
            text,
        } => {
            state.selected == Some(*id)
                && state.session_ui.get(id).is_some_and(|ui| {
                    ui.generation == *generation
                        && ui.draft_revision == *revision
                        && ui.draft == *text
                })
        }
    }
}

fn clear_current_draft(state: &mut UiState, effects: &mut Vec<Effect>) {
    if state.command_from_palette {
        return;
    }
    if let Some(ui) = state.selected_ui_mut() {
        ui.editor.checkpoint(&ui.draft, ui.draft_cursor);
        ui.draft.clear();
        ui.draft_cursor = 0;
        ui.draft_revision = ui.draft_revision.wrapping_add(1);
        effects.push(Effect::SaveDraft {
            session: ui.info.id,
            generation: ui.generation,
            revision: ui.draft_revision,
            text: String::new(),
        });
    } else {
        state
            .orphan_editor
            .checkpoint(&state.orphan_draft, state.orphan_cursor);
        state.orphan_draft.clear();
        state.orphan_cursor = 0;
        state.orphan_revision = state.orphan_revision.wrapping_add(1);
    }
    state.slash_selection = 0;
    state.slash_dismissed = None;
}

fn clear_create_source(state: &mut UiState, source: DraftSource, effects: &mut Vec<Effect>) {
    match source {
        DraftSource::None => {}
        DraftSource::Orphan { revision, text }
            if state.orphan_revision == revision && state.orphan_draft == text =>
        {
            state
                .orphan_editor
                .checkpoint(&state.orphan_draft, state.orphan_cursor);
            state.orphan_draft.clear();
            state.orphan_cursor = 0;
            state.orphan_revision = state.orphan_revision.wrapping_add(1);
        }
        DraftSource::Session {
            id,
            generation,
            revision,
            text,
        } => {
            if let Some(ui) = state.session_ui.get_mut(&id)
                && ui.generation == generation
                && ui.draft_revision == revision
                && ui.draft == text
            {
                ui.editor.checkpoint(&ui.draft, ui.draft_cursor);
                ui.draft.clear();
                ui.draft_cursor = 0;
                ui.draft_revision = ui.draft_revision.wrapping_add(1);
                effects.push(Effect::SaveDraft {
                    session: id,
                    generation,
                    revision: ui.draft_revision,
                    text: String::new(),
                });
            }
        }
        DraftSource::Orphan { .. } => {}
    }
}

fn insert_text(state: &mut UiState, text: &str, typing: bool) {
    state.status = None;
    state.slash_selection = 0;
    state.slash_dismissed = None;
    let (draft, cursor, revision, editor) = state.editor_mut();
    if text.is_empty() {
        return;
    }
    let selection = editor.selection(*cursor);
    if !typing || !editor.typing || selection.is_some() {
        editor.checkpoint(draft, *cursor);
    }
    editor.typing = typing;
    if let Some(range) = selection {
        *cursor = range.start;
        draft.replace_range(range, "");
    }
    draft.insert_str(*cursor, text);
    *cursor = next_grapheme_boundary(draft, *cursor + text.len());
    *revision = revision.wrapping_add(1);
}

fn delete_before_cursor(state: &mut UiState) {
    state.status = None;
    state.slash_selection = 0;
    state.slash_dismissed = None;
    let (draft, cursor, revision, editor) = state.editor_mut();
    if let Some(range) = editor.selection(*cursor) {
        editor.checkpoint(draft, *cursor);
        *cursor = range.start;
        draft.replace_range(range, "");
        *cursor = next_grapheme_boundary(draft, *cursor);
        *revision = revision.wrapping_add(1);
    } else if let Some((index, _)) = draft[..*cursor].grapheme_indices(true).next_back() {
        editor.checkpoint(draft, *cursor);
        draft.drain(index..*cursor);
        *cursor = next_grapheme_boundary(draft, index);
        *revision = revision.wrapping_add(1);
        state.status = None;
        state.slash_selection = 0;
        state.slash_dismissed = None;
    }
}

fn delete_after_cursor(state: &mut UiState) {
    state.status = None;
    state.slash_selection = 0;
    state.slash_dismissed = None;
    let (draft, cursor, revision, editor) = state.editor_mut();
    if let Some(range) = editor.selection(*cursor) {
        editor.checkpoint(draft, *cursor);
        *cursor = range.start;
        draft.replace_range(range, "");
        *cursor = next_grapheme_boundary(draft, *cursor);
        *revision = revision.wrapping_add(1);
    } else if let Some(len) = draft[*cursor..].graphemes(true).next().map(str::len) {
        editor.checkpoint(draft, *cursor);
        draft.drain(*cursor..*cursor + len);
        *cursor = next_grapheme_boundary(draft, *cursor);
        *revision = revision.wrapping_add(1);
        state.status = None;
        state.slash_selection = 0;
        state.slash_dismissed = None;
    }
}

fn move_cursor(state: &mut UiState, delta: isize) {
    let (draft, cursor, _, editor) = state.editor_mut();
    *cursor = if let Some(range) = editor.selection(*cursor) {
        if delta < 0 { range.start } else { range.end }
    } else {
        moved_cursor(draft, *cursor, delta)
    };
    editor.anchor = None;
}

fn moved_cursor(text: &str, cursor: usize, delta: isize) -> usize {
    if delta < 0 {
        text[..cursor]
            .grapheme_indices(true)
            .next_back()
            .map_or(cursor, |(index, _)| index)
    } else {
        text[cursor..]
            .graphemes(true)
            .next()
            .map_or(cursor, |value| cursor + value.len())
    }
}

fn set_cursor_line_edge(state: &mut UiState, end: bool) {
    let (draft, cursor, _, editor) = state.editor_mut();
    editor.anchor = None;
    *cursor = line_edge(draft, *cursor, end);
}

fn line_edge(text: &str, cursor: usize, end: bool) -> usize {
    if end {
        cursor + text[cursor..].find('\n').unwrap_or(text.len() - cursor)
    } else {
        text[..cursor].rfind('\n').map_or(0, |index| index + 1)
    }
}

fn complete_slash(state: &mut UiState) {
    if let Some(command) = state.slash_matches().get(state.slash_selection).copied() {
        let replacement = format!(
            "/{}{}",
            command.name,
            if command.usage.is_empty() { "" } else { " " }
        );
        if let Some(ui) = state.selected_ui_mut() {
            ui.editor.checkpoint(&ui.draft, ui.draft_cursor);
            ui.draft = replacement;
            ui.draft_cursor = ui.draft.len();
            ui.draft_revision = ui.draft_revision.wrapping_add(1);
        } else {
            state
                .orphan_editor
                .checkpoint(&state.orphan_draft, state.orphan_cursor);
            state.orphan_draft = replacement;
            state.orphan_cursor = state.orphan_draft.len();
            state.orphan_revision = state.orphan_revision.wrapping_add(1);
        }
        state.slash_dismissed = None;
    }
}

fn move_slash(state: &mut UiState, delta: isize) {
    let len = state.slash_matches().len();
    if len == 0 {
        state.slash_selection = 0;
        return;
    }
    state.slash_selection =
        (state.slash_selection as isize + delta).rem_euclid(len as isize) as usize;
}

fn move_session(state: &mut UiState, delta: isize) {
    let available: Vec<_> = state
        .sessions
        .iter()
        .filter(|s| !s.archived)
        .map(|s| s.id)
        .collect();
    if available.is_empty() {
        return;
    }
    let current = state
        .session_candidate
        .or(state.selected)
        .and_then(|id| available.iter().position(|candidate| *candidate == id));
    let next = current.map_or(0, |current| {
        (current as isize + delta).clamp(0, available.len() as isize - 1) as usize
    });
    state.session_candidate = Some(available[next]);
    state.session_scroll = None;
}

fn select_session(state: &mut UiState, id: SessionId, effects: &mut Vec<Effect>) {
    let Some(info) = state
        .sessions
        .iter()
        .find(|s| s.id == id && !s.archived)
        .cloned()
    else {
        return;
    };
    state.session_candidate = Some(id);
    state.session_scroll = None;
    if state.selected == Some(id) {
        return;
    }
    if let Some(previous) = state.selected
        && let Some(ui) = state.session_ui.get(&previous)
        && ui.draft_revision <= ui.saved_draft_revision
        && ui.submitting.is_none()
    {
        effects.push(Effect::ReleaseSession {
            session: previous,
            generation: ui.generation,
        });
    }
    state.selected = Some(id);
    refresh_model_label(state, effects);
    state.slash_dismissed = None;
    let generation = state.generation();
    let ui = state
        .session_ui
        .entry(id)
        .or_insert_with(|| SessionUi::new(info.clone(), generation));
    ui.info = info;
    ui.generation = generation;
    ui.unread = 0;
    effects.push(Effect::OpenSession {
        session: id,
        generation,
    });
    if let Some((workspace, _)) = state.workspace {
        effects.push(Effect::RememberSession {
            workspace,
            session: id,
        });
    }
}

fn scroll_up(
    state: &mut UiState,
    amount: usize,
    metrics: Option<std::sync::Arc<crate::layout::TranscriptMetrics>>,
    effects: &mut Vec<Effect>,
) {
    if let Some(ui) = state.selected_ui_mut() {
        let metrics = metrics.or_else(|| ui.transcript_metrics.clone());
        if let Some(metrics) = &metrics {
            let start = ui
                .read_anchor
                .and_then(|anchor| metrics.row_for_anchor(anchor))
                .unwrap_or(metrics.start_row)
                .saturating_sub(amount);
            ui.read_anchor = metrics.anchor_at_start(start);
            ui.scroll_from_tail = metrics
                .total_rows
                .saturating_sub(start + metrics.viewport_rows);
            ui.transcript_metrics = Some(metrics.clone());
        } else {
            ui.scroll_from_tail = ui.scroll_from_tail.saturating_add(amount);
        }
        if metrics
            .as_ref()
            .is_some_and(|metrics| metrics.near_start(ui.scroll_from_tail))
            && !ui.older_loading
            && let Some(cursor) = ui.older_cursor
        {
            ui.older_loading = true;
            ui.older_metrics = metrics;
            effects.push(Effect::LoadOlderHistory {
                session: ui.info.id,
                generation: ui.generation,
                cursor,
            });
        }
    }
}

fn stop_selected(state: &UiState) -> Vec<Effect> {
    state
        .selected_ui()
        .filter(|ui| ui.working())
        .map(|ui| Effect::Stop {
            session: ui.info.id,
            generation: ui.generation,
        })
        .into_iter()
        .collect()
}

fn current_generation_mut(
    state: &mut UiState,
    session: SessionId,
    generation: u64,
) -> Option<&mut SessionUi> {
    state
        .session_ui
        .get_mut(&session)
        .filter(|ui| ui.generation == generation)
}

fn trim_history_front(ui: &mut SessionUi) {
    while ui.history.len() > HISTORY_CACHE_ITEMS || ui.history_bytes > HISTORY_CACHE_BYTES {
        let preserve_front = ui.history.len() > 1
            && ui.read_anchor.is_some_and(|anchor| {
                ui.history
                    .front()
                    .is_some_and(|entry| entry.sequence == anchor.sequence)
            });
        let Some(entry) = (if preserve_front {
            ui.newer_history_missing = true;
            ui.history.pop_back()
        } else {
            ui.history.pop_front()
        }) else {
            break;
        };
        ui.history_bytes = ui.history_bytes.saturating_sub(history_entry_bytes(&entry));
    }
}

fn trim_history_back(ui: &mut SessionUi) -> Vec<bone_app::SessionSeq> {
    let mut evicted = Vec::new();
    while ui.history.len() > HISTORY_CACHE_ITEMS || ui.history_bytes > HISTORY_CACHE_BYTES {
        let preserve_back = ui.history.len() > 1
            && ui.read_anchor.is_some_and(|anchor| {
                ui.history
                    .back()
                    .is_some_and(|entry| entry.sequence == anchor.sequence)
            });
        let Some(entry) = (if preserve_back {
            ui.history.pop_front()
        } else {
            ui.history.pop_back()
        }) else {
            break;
        };
        ui.history_bytes = ui.history_bytes.saturating_sub(history_entry_bytes(&entry));
        evicted.push(entry.sequence);
        ui.newer_history_missing = true;
    }
    evicted
}

pub(crate) fn retain_transcript(
    state: &mut UiState,
    metrics: std::sync::Arc<crate::layout::TranscriptMetrics>,
) -> bool {
    retain_transcript_with_limit(state, metrics, HISTORY_CACHE_BYTES)
}

fn retain_transcript_with_limit(
    state: &mut UiState,
    metrics: std::sync::Arc<crate::layout::TranscriptMetrics>,
    limit: usize,
) -> bool {
    if let Some(ui) = state.selected_ui_mut() {
        if ui.read_anchor.is_some() {
            ui.scroll_from_tail = metrics.scroll_from_tail;
        }
        ui.transcript_metrics = (metrics.allocated_bytes() <= limit).then_some(metrics);
    }
    trim_history_to_limit(state, limit);
    state
        .selected_ui()
        .is_some_and(|ui| ui.transcript_metrics.is_some())
}

fn retained_history_bytes(state: &UiState) -> usize {
    state
        .session_ui
        .values()
        .map(|ui| {
            ui.history_bytes
                + ui.transcript_metrics
                    .as_ref()
                    .map_or(0, |metrics| metrics.allocated_bytes())
                + ui.older_metrics
                    .as_ref()
                    .filter(|older| {
                        ui.transcript_metrics
                            .as_ref()
                            .is_none_or(|current| !std::sync::Arc::ptr_eq(current, older))
                    })
                    .map_or(0, |metrics| metrics.allocated_bytes())
        })
        .sum()
}

fn trim_global_history(state: &mut UiState) {
    trim_history_to_limit(state, HISTORY_CACHE_BYTES);
}

fn trim_history_to_limit(state: &mut UiState, limit: usize) {
    for (id, ui) in &mut state.session_ui {
        if Some(*id) != state.selected {
            ui.transcript_metrics = None;
            ui.older_metrics = None;
        }
    }
    let mut total = retained_history_bytes(state);
    // Derived layouts are expendable. Never evict source history merely because
    // an old or individually oversized layout cannot fit the cache budget.
    if total > limit {
        if let Some(ui) = state.selected_ui_mut() {
            ui.older_metrics = None;
        }
        total = retained_history_bytes(state);
    }
    if total > limit {
        if let Some(ui) = state.selected_ui_mut() {
            ui.transcript_metrics = None;
        }
        total = retained_history_bytes(state);
    }
    while total > limit {
        let victim = state
            .session_ui
            .iter()
            .find(|(id, ui)| Some(**id) != state.selected && !ui.history.is_empty())
            .map(|(id, _)| *id)
            .or_else(|| {
                state.selected.filter(|id| {
                    state
                        .session_ui
                        .get(id)
                        .is_some_and(|ui| !ui.history.is_empty())
                })
            });
        let Some(victim) = victim else {
            break;
        };
        let ui = state
            .session_ui
            .get_mut(&victim)
            .expect("selected cache victim exists");
        let preserve_front = ui.history.len() > 1
            && ui.read_anchor.is_some_and(|anchor| {
                ui.history
                    .front()
                    .is_some_and(|entry| entry.sequence == anchor.sequence)
            });
        let Some(entry) = (if preserve_front {
            ui.newer_history_missing = true;
            ui.history.pop_back()
        } else {
            ui.history.pop_front()
        }) else {
            break;
        };
        let bytes = history_entry_bytes(&entry);
        ui.history_bytes = ui.history_bytes.saturating_sub(bytes);
        total = total.saturating_sub(bytes);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn background_scheduling_does_not_redraw_or_clear_pending_paint() {
        for dirty in [false, true] {
            let mut state = super::UiState::default();
            state.dirty = dirty;
            super::update(&mut state, super::UiEvent::PersistDraftsRequested);
            assert_eq!(state.dirty, dirty);
            super::update(&mut state, super::UiEvent::RefreshOverviewRequested);
            assert_eq!(state.dirty, dirty);
        }
    }

    use super::*;

    #[test]
    fn unknown_slash_is_never_submitted() {
        let mut state = UiState::default();
        update(
            &mut state,
            UiEvent::Action(Action::Paste("/does-not-exist".into())),
        );
        let effects = update(&mut state, UiEvent::Action(Action::Submit));
        assert!(effects.is_empty());
        assert_eq!(state.orphan_draft, "/does-not-exist");
    }

    #[test]
    fn orphan_text_is_kept_until_new_session_exists() {
        let mut state = UiState::default();
        update(&mut state, UiEvent::Action(Action::Paste("keep me".into())));
        let effects = update(&mut state, UiEvent::Action(Action::Submit));
        assert!(matches!(effects.as_slice(), [Effect::CreateSession { .. }]));
        assert_eq!(state.orphan_draft, "keep me");
    }
}

fn open_panel(state: &mut UiState, panel: Panel) {
    state.panel_return = state.focus;
    state.panel = Some(panel);
    state.panel_selection = 0;
    state.panel_scroll = 0;
}
fn pin_reading(state: &mut UiState) {
    if let Some(ui) = state.selected_ui_mut()
        && ui.read_anchor.is_none()
        && let Some(metrics) = &ui.transcript_metrics
    {
        ui.read_anchor = metrics.anchor_at_start(metrics.start_row);
    }
}

fn close_panel(state: &mut UiState) {
    state.connection_form = None;
    state.login_request = None;
    state.panel = None;
    state.focus = state.panel_return;
}
fn open_models(state: &mut UiState, effects: &mut Vec<Effect>) {
    open_panel(state, Panel::Models);
    state.models_loading = true;
    state.model_request = state.generation();
    effects.push(Effect::LoadModels {
        session: state.selected,
        request: state.model_request,
    });
}
fn apply_model(state: &mut UiState, effects: &mut Vec<Effect>) {
    select_model_row(state, state.panel_selection, effects);
}

fn run_palette_command(state: &mut UiState, command: &str, effects: &mut Vec<Effect>) {
    close_panel(state);
    state.command_from_palette = true;
    execute_command(state, command, effects);
    state.command_from_palette = false;
}
fn open_latest(state: &mut UiState) {
    let content = state.selected_ui().and_then(|ui| {
        ui.snapshot
            .as_ref()
            .and_then(|snapshot| {
                snapshot
                    .jobs
                    .last()
                    .and_then(|job| super::reader::ReaderContent::from_job(snapshot, job.id))
            })
            .or_else(|| {
                ui.history
                    .iter()
                    .rev()
                    .find_map(|entry| super::reader::ReaderContent::from_history(ui.info.id, entry))
            })
    });
    if let Some(content) = content {
        state.panel_return = Focus::Conversation;
        state.panel_scroll = 0;
        pin_reading(state);
        state.panel = Some(Panel::Reader(content));
    } else {
        state.status = Some("No task or tool result to open yet".into());
    }
}

fn refresh_model_label(state: &mut UiState, effects: &mut Vec<Effect>) {
    state.model_label = None;
    state.model_facts = None;
    state.model_label_request = state.generation();
    effects.push(Effect::LoadModelLabel {
        session: state.selected,
        request: state.model_label_request,
    });
}

#[cfg(test)]
mod async_identity_tests {
    use super::*;

    #[test]
    fn late_model_callbacks_do_not_change_another_open_panel() {
        let mut state = UiState::default();
        let mut effects = Vec::new();
        open_models(&mut state, &mut effects);
        let request = state.model_request;
        update(&mut state, UiEvent::Action(Action::Escape));
        update(&mut state, UiEvent::Action(Action::OpenCommands));
        state.panel_selection = 4;
        update(
            &mut state,
            UiEvent::ModelsLoaded {
                session: None,
                request,
                choices: vec![],
                profiles: vec![],
            },
        );
        assert_eq!(state.panel_selection, 4);
        update(
            &mut state,
            UiEvent::ModelApplied {
                session: None,
                request,
                label: Some("saved-model".into()),
                facts: None,
                error: None,
            },
        );
        assert!(matches!(state.panel, Some(Panel::Commands)));
        assert_eq!(state.panel_selection, 4);
        assert_eq!(state.model_label.as_deref(), Some("saved-model"));
    }

    #[test]
    fn cancelled_login_cannot_overwrite_reopened_attempt() {
        let mut state = UiState::default();
        open_panel(&mut state, Panel::Login);
        state.login_request = Some(1);
        let effects = update(&mut state, UiEvent::Action(Action::Escape));
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::CancelLogin))
        );
        assert_eq!(state.login_request, None);
        open_panel(&mut state, Panel::Login);
        state.login_request = Some(2);
        update(
            &mut state,
            UiEvent::LoginChanged {
                request: 1,
                state: bone_app::LoginState::Succeeded,
            },
        );
        assert!(matches!(
            state.login_state,
            bone_app::LoginState::Connecting
        ));
        update(
            &mut state,
            UiEvent::LoginChanged {
                request: 2,
                state: bone_app::LoginState::Succeeded,
            },
        );
        assert!(matches!(state.login_state, bone_app::LoginState::Succeeded));
    }

    #[test]
    fn switching_sessions_clears_label_and_rejects_late_previous_selection() {
        let mut state = UiState::default();
        let workspace = bone_app::WorkspaceId::new();
        let a = SessionId::new();
        let b = SessionId::new();
        state.sessions = [a, b]
            .into_iter()
            .map(|id| bone_app::SessionInfo {
                id,
                workspace,
                title: "test".into(),
                archived: false,
            })
            .collect();
        state.model_label = Some("workspace-model".into());
        let mut effects = Vec::new();
        select_session(&mut state, a, &mut effects);
        assert_eq!(state.model_label, None);
        let first = state.model_label_request;
        select_session(&mut state, b, &mut effects);
        select_session(&mut state, a, &mut effects);
        let current = state.model_label_request;
        assert_ne!(first, current);
        update(
            &mut state,
            UiEvent::ModelLabelLoaded {
                session: Some(a),
                request: first,
                label: Some("stale".into()),
                facts: None,
            },
        );
        assert_eq!(state.model_label, None);
        update(
            &mut state,
            UiEvent::ModelLabelLoaded {
                session: Some(a),
                request: current,
                label: Some("current".into()),
                facts: None,
            },
        );
        assert_eq!(state.model_label.as_deref(), Some("current"));
        assert_eq!(
            effects
                .iter()
                .filter(|effect| matches!(effect, Effect::LoadModelLabel { .. }))
                .count(),
            3
        );
    }

    #[test]
    fn model_apply_invalidates_earlier_label_read() {
        let mut state = UiState::default();
        let mut effects = Vec::new();
        refresh_model_label(&mut state, &mut effects);
        let stale = state.model_label_request;
        update(
            &mut state,
            UiEvent::ModelApplied {
                session: None,
                request: 0,
                label: Some("new".into()),
                facts: None,
                error: None,
            },
        );
        update(
            &mut state,
            UiEvent::ModelLabelLoaded {
                session: None,
                request: stale,
                label: Some("old".into()),
                facts: None,
            },
        );
        assert_eq!(state.model_label.as_deref(), Some("new"));
    }
}

fn apply_rename(state: &mut UiState, effects: &mut Vec<Effect>) {
    let title = state.rename_input.trim().to_owned();
    if title.is_empty() || title.len() > 200 {
        state.status = Some("Use a nonempty title of at most 200 bytes".into());
        return;
    }
    let Some((session, generation)) = state.rename_target else {
        return;
    };
    if state
        .session_ui
        .get(&session)
        .is_none_or(|ui| ui.generation != generation)
    {
        state.status = Some("The session changed; reopen Rename".into());
        return;
    }
    state.status = None;
    effects.push(Effect::RenameSession {
        session,
        generation,
        title,
    });
    close_panel(state);
}

#[cfg(test)]
mod panel_draft_tests {
    use super::*;
    use bone_app::{
        HistoryEntry, InputId, JobRef, OutcomeKind, QuestionId, RuntimeId, SessionEvent,
        SessionInfo, SessionSeq, WorkspaceId,
    };

    fn fixture() -> (UiState, QuestionId) {
        let id = SessionId::new();
        let question = QuestionId {
            runtime: RuntimeId::new(),
            record: 7,
            reply_to: InputId(1),
        };
        let info = SessionInfo {
            id,
            workspace: WorkspaceId::new(),
            title: "Existing title".into(),
            archived: false,
        };
        let mut ui = SessionUi::new(info.clone(), 1);
        ui.draft = "ordinary draft".into();
        ui.draft_cursor = 4;
        ui.draft_revision = 8;
        let mut answer = AnswerDraft::new(question);
        answer.replace("answer draft".into(), 3);
        ui.answer_drafts.insert(question, answer);
        ui.selected_answer = Some(question);
        ui.history.push_back(HistoryEntry {
            sequence: SessionSeq(1),
            occurred_at: 0,
            event: SessionEvent::JobFinished {
                job: JobRef {
                    runtime: question.runtime,
                    id: 1,
                },
                outcome: OutcomeKind::Completed,
                summary: "Done".into(),
                remaining: vec![],
            },
        });
        let mut state = UiState::default();
        state.sessions.push(info);
        state.selected = Some(id);
        state.session_ui.insert(id, ui);
        (state, question)
    }

    #[test]
    fn editor_history_isolated_between_orphan_session_and_answer() {
        let (mut state, question) = fixture();
        let session = state.selected;
        state.selected = None;
        update(&mut state, UiEvent::Action(Action::Paste("orphan".into())));
        state.selected = session;
        update(&mut state, UiEvent::Action(Action::Paste("ANSWER".into())));
        state.selected_ui_mut().unwrap().selected_answer = None;
        update(&mut state, UiEvent::Action(Action::Paste("SESSION".into())));
        update(&mut state, UiEvent::Action(Action::Undo));
        assert_eq!(state.draft(), "ordinary draft");
        state.selected_ui_mut().unwrap().selected_answer = Some(question);
        assert!(state.draft().contains("ANSWER"));
        update(&mut state, UiEvent::Action(Action::Undo));
        assert_eq!(state.draft(), "answer draft");
        state.selected = None;
        assert_eq!(state.draft(), "orphan");
        update(&mut state, UiEvent::Action(Action::Undo));
        assert_eq!(state.draft(), "");
        update(&mut state, UiEvent::Action(Action::Redo));
        assert_eq!(state.draft(), "orphan");
    }

    #[test]
    fn unicode_selection_replacement_and_coalesced_typing_are_undoable() {
        let mut state = UiState::default();
        for ch in "中e\u{301}🙂".chars() {
            update(&mut state, UiEvent::Action(Action::Input(ch)));
        }
        update(&mut state, UiEvent::Action(Action::Undo));
        assert_eq!(state.draft(), "");
        update(&mut state, UiEvent::Action(Action::Redo));
        for _ in 0..2 {
            update(
                &mut state,
                UiEvent::Action(Action::MoveCursor {
                    direction: -1,
                    select: true,
                    word: false,
                    width: 8,
                }),
            );
        }
        assert_eq!(
            state.editor().selection(state.draft_cursor()),
            Some(3.."中e\u{301}🙂".len())
        );
        update(&mut state, UiEvent::Action(Action::Paste("字".into())));
        assert_eq!(state.draft(), "中字");
        update(&mut state, UiEvent::Action(Action::Undo));
        assert_eq!(state.draft(), "中e\u{301}🙂");
    }

    #[test]
    fn undo_back_to_submitted_text_does_not_allow_stale_receipt_to_clear_it() {
        let (mut state, question) = fixture();
        for answer in [false, true] {
            let ui = state.selected_ui_mut().unwrap();
            ui.selected_answer = answer.then_some(question);
            let text = if answer {
                ui.answer_drafts[&question].text.clone()
            } else {
                ui.draft.clone()
            };
            let revision = if answer {
                ui.answer_drafts[&question].revision
            } else {
                ui.draft_revision
            };
            let request_id = RequestId::new();
            ui.submitting = Some(PendingSubmission {
                reply_to: answer.then_some(question),
                request_id,
                text: text.clone(),
                draft_revision: revision,
                failed: false,
            });
            let session = ui.info.id;
            update(&mut state, UiEvent::Action(Action::Paste("later".into())));
            update(&mut state, UiEvent::Action(Action::Undo));
            assert_eq!(state.draft(), text);
            update(
                &mut state,
                UiEvent::Submitted {
                    session,
                    generation: 1,
                    request_id,
                    receipt: bone_app::SubmissionReceipt {
                        input: InputId(7),
                        saved_at: SessionSeq(7),
                    },
                },
            );
            assert_eq!(state.draft(), text);
        }
    }

    #[test]
    fn restored_answer_can_be_undone_without_touching_ordinary_draft() {
        let (mut state, question) = fixture();
        let answer = state
            .selected_ui_mut()
            .unwrap()
            .answer_drafts
            .get_mut(&question)
            .unwrap();
        answer.replace(
            answer::append_restored_text(&answer.text, "restored"),
            usize::MAX,
        );
        update(&mut state, UiEvent::Action(Action::Undo));
        assert_eq!(state.draft(), "answer draft");
        assert_eq!(state.selected_ui().unwrap().draft, "ordinary draft");
    }

    #[test]
    fn editor_history_budget_is_global_and_prefers_current_buffer() {
        let (mut state, question) = fixture();
        let text = "x".repeat(1024 * 1024);
        for _ in 0..4 {
            state.orphan_editor.checkpoint(&text, text.len());
        }
        let ui = state.selected_ui_mut().unwrap();
        for _ in 0..4 {
            ui.editor.checkpoint(&text, text.len());
        }
        for _ in 0..4 {
            ui.answer_drafts
                .get_mut(&question)
                .unwrap()
                .editor
                .checkpoint(&text, text.len());
        }
        trim_editor_history(&mut state);
        let ui = state.selected_ui().unwrap();
        let total = state.orphan_editor.history_bytes()
            + ui.editor.history_bytes()
            + ui.answer_drafts[&question].editor.history_bytes();
        assert!(total <= 8 * 1024 * 1024);
        assert_eq!(
            ui.answer_drafts[&question].editor.history_bytes(),
            4 * 1024 * 1024
        );
    }

    #[test]
    fn details_menu_keeps_identifiers_and_opens_the_selected_object() {
        use super::super::reader::ReaderSource;
        let (mut state, question) = fixture();
        let job = JobRef {
            runtime: question.runtime,
            id: 2,
        };
        let ui = state.selected_ui_mut().unwrap();
        ui.history.push_back(HistoryEntry {
            sequence: SessionSeq(2),
            occurred_at: 0,
            event: SessionEvent::ToolFinished {
                call: bone_app::CallRef {
                    runtime: question.runtime,
                    id: 9,
                },
                job,
                tool: "read_file".into(),
                outcome: bone_app::ToolOutcome {
                    result: Ok(serde_json::json!("x".repeat(1024 * 1024))),
                    external_effect: bone_app::ExternalEffect::None,
                },
            },
        });
        ui.snapshot = Some(std::sync::Arc::new(bone_app::SessionView {
            session: ui.info.clone(),
            runtime: bone_app::RuntimeState::Detached,
            draft: String::new(),
            inputs: vec![],
            activity: vec![],
            history_through: SessionSeq(2),
            problem: None,
            jobs: vec![bone_app::JobView {
                id: job,
                owner: bone_app::JobOwner::User,
                inputs: vec![],
                goal: "Current live task".into(),
                scope: "workspace".into(),
                done_when: "done".into(),
                state: bone_app::JobState::Running,
                report: None,
            }],
        }));
        open_objects(&mut state);
        let Some(Panel::Objects { choices, .. }) = &state.panel else {
            panic!("object menu")
        };
        assert_eq!(choices.len(), 3);
        assert!(choices.iter().map(|(_, label)| label.len()).sum::<usize>() < 300);
        let target = choices
            .iter()
            .position(|(source, _)| *source == ReaderSource::History(SessionSeq(1)))
            .unwrap();
        update(&mut state, UiEvent::Action(Action::SelectObject(target)));
        assert!(
            matches!(&state.panel, Some(Panel::Reader(content)) if content.source == ReaderSource::History(SessionSeq(1)) && content.text.contains("Done"))
        );
        assert_drafts(&state, question);
        update(&mut state, UiEvent::Action(Action::Escape));
        open_objects(&mut state);
        state.panel_selection = 0;
        update(&mut state, UiEvent::Action(Action::ActivatePanel));
        assert!(
            matches!(&state.panel, Some(Panel::Reader(content)) if content.source == ReaderSource::Job(job))
        );
        assert_drafts(&state, question);
    }

    #[test]
    fn expired_or_cross_session_object_menu_never_substitutes_another_object() {
        let (mut state, question) = fixture();
        open_objects(&mut state);
        state.selected_ui_mut().unwrap().history.clear();
        update(&mut state, UiEvent::Action(Action::ActivatePanel));
        assert!(matches!(state.panel, Some(Panel::Objects { .. })));
        assert!(
            state
                .status
                .as_deref()
                .unwrap()
                .contains("no longer loaded")
        );
        assert_drafts(&state, question);
        state.selected = Some(SessionId::new());
        update(&mut state, UiEvent::Action(Action::SelectObject(0)));
        assert!(matches!(state.panel, Some(Panel::Objects { .. })));
        assert!(state.status.as_deref().unwrap().contains("session changed"));
    }

    fn assert_drafts(state: &UiState, question: QuestionId) {
        let ui = state.selected_ui().unwrap();
        assert_eq!(
            (&*ui.draft, ui.draft_cursor, ui.draft_revision),
            ("ordinary draft", 4, 8)
        );
        assert_eq!(ui.selected_answer, Some(question));
        let answer = &ui.answer_drafts[&question];
        assert_eq!(
            (&*answer.text, answer.cursor, answer.revision),
            ("answer draft", 3, 1)
        );
    }

    #[test]
    fn live_job_reader_refreshes_only_its_identity_without_moving_the_editor() {
        let (mut state, question) = fixture();
        let session = state.selected.unwrap();
        let job = JobRef {
            runtime: question.runtime,
            id: 1,
        };
        let mut snapshot = bone_app::SessionView {
            session: state.selected_ui().unwrap().info.clone(),
            runtime: bone_app::RuntimeState::Detached,
            draft: String::new(),
            inputs: vec![],
            activity: vec![],
            history_through: SessionSeq(0),
            problem: None,
            jobs: vec![bone_app::JobView {
                id: job,
                owner: bone_app::JobOwner::User,
                inputs: vec![],
                goal: "Original job".into(),
                scope: "scope".into(),
                done_when: "done".into(),
                state: bone_app::JobState::Running,
                report: None,
            }],
        };
        state.panel = Some(Panel::Reader(
            super::super::reader::ReaderContent::from_job(&snapshot, job).unwrap(),
        ));
        state.panel_scroll = 8;
        let focus = state.focus;
        snapshot.jobs[0].state = bone_app::JobState::Finished {
            outcome: OutcomeKind::Completed,
            summary: "New result".into(),
        };
        update(
            &mut state,
            UiEvent::SessionChanged {
                session,
                generation: 99,
                snapshot: std::sync::Arc::new(snapshot.clone()),
            },
        );
        assert!(
            matches!(&state.panel, Some(Panel::Reader(content)) if content.text.contains("Running"))
        );
        update(
            &mut state,
            UiEvent::SessionChanged {
                session,
                generation: 1,
                snapshot: std::sync::Arc::new(snapshot.clone()),
            },
        );
        assert!(
            matches!(&state.panel, Some(Panel::Reader(content)) if content.text.contains("New result"))
        );
        // Same numeric ID in another runtime is a different job, never a substitute.
        snapshot.jobs[0].id.runtime = RuntimeId::new();
        update(
            &mut state,
            UiEvent::SessionChanged {
                session,
                generation: 1,
                snapshot: std::sync::Arc::new(snapshot.clone()),
            },
        );
        assert!(
            matches!(&state.panel, Some(Panel::Reader(content)) if content.source == super::super::reader::ReaderSource::Job(job) && content.text.contains("no longer in the current snapshot") && !content.text.contains("New result"))
        );
        assert_eq!(state.panel_scroll, 8);
        assert_eq!(state.focus, focus);
        assert_drafts(&state, question);

        let original = super::super::reader::ReaderContent::from_history(
            session,
            &state.selected_ui().unwrap().history[0],
        )
        .unwrap();
        state.panel = Some(Panel::Reader(original.clone()));
        update(
            &mut state,
            UiEvent::SessionChanged {
                session,
                generation: 1,
                snapshot: std::sync::Arc::new(snapshot),
            },
        );
        assert!(matches!(&state.panel, Some(Panel::Reader(content)) if *content == original));
    }

    #[test]
    fn command_model_and_detail_round_trips_preserve_both_drafts() {
        let (mut state, question) = fixture();
        for command in ["model", "details", "help"] {
            update(&mut state, UiEvent::Action(Action::OpenCommands));
            state.panel_selection = COMMANDS
                .iter()
                .position(|spec| spec.name == command)
                .unwrap();
            let effects = update(&mut state, UiEvent::Action(Action::ActivatePanel));
            assert!(
                effects.iter().all(|effect| !matches!(
                    effect,
                    Effect::SaveDraft { .. } | Effect::Submit { .. }
                ))
            );
            assert!(state.panel.is_some());
            if command == "model" {
                state.models_loading = false;
                let add = state.model_row_count() - 1;
                update(&mut state, UiEvent::Action(Action::SelectModel(add)));
                update(&mut state, UiEvent::Action(Action::ChooseConnectionKind(0)));
                update(&mut state, UiEvent::Action(Action::SetupClear));
                update(
                    &mut state,
                    UiEvent::Action(Action::SetupText("e\u{301}👩‍💻".to_owned().into())),
                );
                update(&mut state, UiEvent::Action(Action::SetupBackspace));
                assert_eq!(state.connection_form.as_ref().unwrap().label, "e\u{301}");
                update(&mut state, UiEvent::Action(Action::SetupBackspace));
                assert!(state.connection_form.as_ref().unwrap().label.is_empty());
                update(&mut state, UiEvent::Action(Action::Escape));
                assert!(matches!(state.panel, Some(Panel::Models)));
            }
            update(&mut state, UiEvent::Action(Action::Escape));
            assert!(state.panel.is_none());
            assert_drafts(&state, question);
        }
    }

    #[test]
    fn rename_menu_uses_its_own_buffer_and_existing_success_failure_events() {
        let (mut state, question) = fixture();
        update(&mut state, UiEvent::Action(Action::OpenCommands));
        state.panel_selection = COMMANDS
            .iter()
            .position(|spec| spec.name == "rename")
            .unwrap();
        update(&mut state, UiEvent::Action(Action::ActivatePanel));
        assert!(matches!(state.panel, Some(Panel::Rename)));
        assert_eq!(state.rename_input, "Existing title");
        state.rename_input.clear();
        assert!(update(&mut state, UiEvent::Action(Action::ActivatePanel)).is_empty());
        assert!(matches!(state.panel, Some(Panel::Rename)));
        update(
            &mut state,
            UiEvent::Action(Action::RenameText("New e\u{301}".into())),
        );
        update(&mut state, UiEvent::Action(Action::RenameBackspace));
        assert_eq!(state.rename_input, "New ");
        let effects = update(&mut state, UiEvent::Action(Action::ActivatePanel));
        let [
            Effect::RenameSession {
                session,
                generation,
                title,
            },
        ] = effects.as_slice()
        else {
            panic!("expected rename effect")
        };
        assert_eq!(title, "New");
        assert!(state.panel.is_none());
        assert_drafts(&state, question);
        update(
            &mut state,
            UiEvent::OperationFailed {
                kind: OperationKind::RenameSession,
                session: Some(*session),
                generation: Some(*generation),
                message: "Could not save title".into(),
            },
        );
        assert_eq!(state.status.as_deref(), Some("Could not save title"));
        assert_drafts(&state, question);
        update(
            &mut state,
            UiEvent::SessionRenamed {
                session: *session,
                generation: *generation,
                title: title.clone(),
            },
        );
        assert_eq!(state.selected_ui().unwrap().info.title, "New");
        assert_drafts(&state, question);
    }
}

fn grapheme_boundary(text: &str, byte: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(at, _)| at)
        .chain(std::iter::once(text.len()))
        .take_while(|at| *at <= byte)
        .last()
        .unwrap_or(0)
}
fn next_grapheme_boundary(text: &str, byte: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(at, _)| at)
        .chain(std::iter::once(text.len()))
        .find(|at| *at >= byte)
        .unwrap_or(text.len())
}

// Editing history has a separate 8 MiB global budget. Inactive buffers are
// trimmed first; current text and revisions are never part of eviction.
fn trim_editor_history(state: &mut UiState) {
    let selected = state.selected;
    let mut editors = Vec::new();
    for (id, ui) in &mut state.session_ui {
        editors.push((
            Some(*id) == selected && ui.selected_answer.is_none(),
            &mut ui.editor,
        ));
        for (question, answer) in &mut ui.answer_drafts {
            editors.push((
                Some(*id) == selected && ui.selected_answer == Some(*question),
                &mut answer.editor,
            ));
        }
    }
    editors.push((selected.is_none(), &mut state.orphan_editor));
    editors.sort_by_key(|(active, _)| *active);
    let mut bytes: usize = editors
        .iter()
        .map(|(_, editor)| editor.history_bytes())
        .sum();
    for (_, editor) in editors {
        while bytes > 8 * 1024 * 1024 {
            let before = editor.history_bytes();
            if !editor.evict_oldest() {
                break;
            }
            bytes -= before - editor.history_bytes();
        }
    }
}

#[cfg(test)]
mod session_browsing_tests {
    use super::*;

    fn fixture() -> (UiState, Vec<SessionId>) {
        let ids = (0..3).map(|_| SessionId::new()).collect::<Vec<_>>();
        let workspace = bone_app::WorkspaceId::new();
        let mut state = UiState::default();
        state.sessions = ids
            .iter()
            .map(|id| bone_app::SessionInfo {
                id: *id,
                workspace,
                title: "test".into(),
                archived: false,
            })
            .collect();
        let mut effects = Vec::new();
        select_session(&mut state, ids[0], &mut effects);
        state.focus = Focus::Sessions;
        state.selected_ui_mut().unwrap().draft = "unsent original".into();
        (state, ids)
    }

    #[test]
    fn arrows_only_browse_enter_opens_and_click_opens_immediately() {
        let (mut state, ids) = fixture();
        assert!(update(&mut state, UiEvent::Action(Action::SelectNext)).is_empty());
        assert_eq!(state.selected, Some(ids[0]));
        assert_eq!(state.session_candidate, Some(ids[1]));
        let effects = update(&mut state, UiEvent::Action(Action::OpenCandidate));
        assert!(effects.iter().any(
            |effect| matches!(effect, Effect::OpenSession { session, .. } if *session == ids[1])
        ));
        assert_eq!(state.selected, Some(ids[1]));
        assert_eq!(state.focus, Focus::Conversation);
        assert_eq!(state.session_ui[&ids[0]].draft, "unsent original");
        update(&mut state, UiEvent::Action(Action::SelectSession(ids[2])));
        assert_eq!(state.selected, Some(ids[2]));
        assert_eq!(state.session_candidate, Some(ids[2]));
        assert_eq!(state.focus, Focus::Conversation);
    }

    #[test]
    fn wheel_does_not_change_candidate_or_current_and_escape_returns_original() {
        let (mut state, ids) = fixture();
        update(&mut state, UiEvent::Action(Action::SelectNext));
        assert!(
            update(
                &mut state,
                UiEvent::Action(Action::ScrollSessions { start: 2 })
            )
            .is_empty()
        );
        assert_eq!(state.selected, Some(ids[0]));
        assert_eq!(state.session_candidate, Some(ids[1]));
        assert_eq!(state.session_scroll, Some(2));
        assert!(update(&mut state, UiEvent::Action(Action::Escape)).is_empty());
        assert_eq!(state.selected, Some(ids[0]));
        assert_eq!(state.session_candidate, Some(ids[0]));
        assert_eq!(state.focus, Focus::Conversation);
        assert_eq!(state.single_pane(), crate::layout::SinglePane::Conversation);
        assert_eq!(state.session_scroll, None);
    }
}

fn open_objects(state: &mut UiState) {
    use super::reader::ReaderSource;
    let Some(ui) = state.selected_ui() else {
        state.status = Some("There is no session to inspect".into());
        return;
    };
    let session = ui.info.id;
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
    for entry in ui.history.iter().rev() {
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
    open_panel(state, Panel::Objects { session, choices });
}

fn open_object(state: &mut UiState) {
    use super::reader::{ReaderContent, ReaderSource};
    let Some(Panel::Objects { session, choices }) = &state.panel else {
        return;
    };
    let Some((source, _)) = choices.get(state.panel_selection) else {
        return;
    };
    if state.selected != Some(*session) {
        state.status = Some("The session changed; reopen /details".into());
        return;
    }
    let content = state.selected_ui().and_then(|ui| match *source {
        ReaderSource::Job(job) => ui
            .snapshot
            .as_ref()
            .and_then(|snapshot| ReaderContent::from_job(snapshot, job)),
        ReaderSource::History(sequence) => ui
            .history
            .iter()
            .find(|entry| entry.sequence == sequence)
            .and_then(|entry| ReaderContent::from_history(*session, entry)),
    });
    if let Some(content) = content {
        state.panel_scroll = 0;
        pin_reading(state);
        state.panel = Some(Panel::Reader(content));
        state.status = None;
    } else {
        state.status = Some("This object is no longer loaded; reopen /details".into());
    }
}

#[cfg(test)]
mod final_integration_regressions {
    use super::*;

    fn session(state: &mut UiState) -> SessionId {
        let id = SessionId::new();
        let info = bone_app::SessionInfo {
            id,
            workspace: bone_app::WorkspaceId::new(),
            title: "session".into(),
            archived: false,
        };
        state.sessions.push(info.clone());
        state.session_ui.insert(id, SessionUi::new(info, 1));
        id
    }

    #[test]
    fn pointer_submission_focuses_composer_but_reading_submission_does_not() {
        let mut state = UiState::default();
        state.orphan_draft = "send this".into();
        state.orphan_cursor = state.orphan_draft.len();
        state.focus = Focus::Conversation;
        assert!(update(&mut state, UiEvent::Action(Action::Submit)).is_empty());
        assert!(state.pending_create.is_none());
        let effects = update(&mut state, UiEvent::Action(Action::ClickSubmit));
        assert_eq!(state.focus, Focus::Composer);
        assert!(matches!(effects.as_slice(), [Effect::CreateSession { .. }]));
        assert_eq!(
            state
                .pending_create
                .as_ref()
                .unwrap()
                .first_input
                .as_ref()
                .unwrap()
                .text,
            "send this"
        );
    }

    #[test]
    fn activating_saved_model_preserves_ordinary_draft() {
        let mut state = UiState::default();
        state.panel = Some(Panel::Models);
        state.orphan_draft = "missing foo".into();
        let choice = ModelChoice {
            selection: bone_app::ModelSelection::new(bone_app::ProfileId::chatgpt(), "gpt-5.5")
                .unwrap(),
            profile_label: "ChatGPT".into(),
        };
        state.model_choices.push(choice.clone());
        let effects = update(&mut state, UiEvent::Action(Action::SelectModel(0)));
        assert!(
            matches!(effects.as_slice(), [Effect::SetModel { selection, .. }] if selection == &choice.selection)
        );
        assert_eq!(state.orphan_draft, "missing foo");
        assert!(update(&mut state, UiEvent::Action(Action::SelectModel(0))).is_empty());
        state.models_loading = false;
        let effects = update(&mut state, UiEvent::Action(Action::ActivatePanel));
        assert!(
            matches!(effects.as_slice(), [Effect::SetModel { selection, .. }] if selection == &choice.selection)
        );
    }

    #[test]
    fn old_model_write_refreshes_facts_without_touching_reopened_panel() {
        let mut state = UiState::default();
        update(&mut state, UiEvent::Action(Action::OpenModels));
        let old = state.model_request;
        update(&mut state, UiEvent::Action(Action::Escape));
        update(&mut state, UiEvent::Action(Action::OpenModels));
        let current = state.model_request;
        state.model_label = Some("previous".into());
        state.orphan_draft = "newer draft".into();
        state.panel_selection = 3;
        let effects = update(
            &mut state,
            UiEvent::ModelApplied {
                session: None,
                request: old,
                label: Some("stale callback".into()),
                facts: None,
                error: None,
            },
        );
        let [
            Effect::LoadModelLabel {
                session: None,
                request,
            },
        ] = effects.as_slice()
        else {
            panic!("query fresh facts")
        };
        let refresh = *request;
        assert_ne!(refresh, old);
        assert_eq!(state.model_request, current);
        assert!(state.models_loading);
        assert!(matches!(state.panel, Some(Panel::Models)));
        assert_eq!(state.panel_selection, 3);
        assert_eq!(state.orphan_draft, "newer draft");
        assert_eq!(state.model_label.as_deref(), Some("previous"));
        update(
            &mut state,
            UiEvent::ModelLabelLoaded {
                session: None,
                request: refresh,
                label: Some("actual App value".into()),
                facts: None,
            },
        );
        assert_eq!(state.model_label.as_deref(), Some("actual App value"));
        assert!(matches!(state.panel, Some(Panel::Models)));
        let other = session(&mut state);
        state.selected = Some(other);
        assert!(
            update(
                &mut state,
                UiEvent::ModelApplied {
                    session: None,
                    request: old,
                    label: None,
                    facts: None,
                    error: None
                }
            )
            .is_empty()
        );
    }

    #[test]
    fn background_failures_update_the_owner_without_overwriting_current_status() {
        let mut state = UiState::default();
        let background = session(&mut state);
        let current = session(&mut state);
        state.selected = Some(current);
        state.status = Some("current status".into());
        let request = RequestId::new();
        let ui = state.session_ui.get_mut(&background).unwrap();
        ui.submitting = Some(PendingSubmission {
            reply_to: None,
            request_id: request,
            text: "background input".into(),
            draft_revision: 0,
            failed: false,
        });
        ui.history_loading = true;
        update(
            &mut state,
            UiEvent::SubmitFailed {
                session: background,
                generation: 1,
                request_id: request,
                message: "background submission failed".into(),
            },
        );
        assert!(
            state.session_ui[&background]
                .submitting
                .as_ref()
                .unwrap()
                .failed
        );
        assert_eq!(state.status.as_deref(), Some("current status"));
        update(
            &mut state,
            UiEvent::OperationFailed {
                kind: OperationKind::LoadHistory,
                session: Some(background),
                generation: Some(1),
                message: "background history failed".into(),
            },
        );
        assert!(!state.session_ui[&background].history_loading);
        assert_eq!(state.status.as_deref(), Some("current status"));
        update(
            &mut state,
            UiEvent::OperationFailed {
                kind: OperationKind::SaveDraft,
                session: Some(current),
                generation: Some(1),
                message: "current save failed".into(),
            },
        );
        assert_eq!(state.status.as_deref(), Some("current save failed"));
    }
}

#[cfg(test)]
mod transcript_budget_tests {
    use super::*;
    use crate::layout::{AnchorPart, ContentAnchor, TranscriptMetrics};
    use std::sync::Arc;

    fn fixture() -> UiState {
        let mut state = UiState::default();
        let info = bone_app::SessionInfo {
            id: SessionId::new(),
            workspace: bone_app::WorkspaceId::new(),
            title: "budget".into(),
            archived: false,
        };
        let mut ui = SessionUi::new(info.clone(), 1);
        let entry = bone_app::HistoryEntry {
            sequence: bone_app::SessionSeq(1),
            occurred_at: 0,
            event: bone_app::SessionEvent::InputCancelled {
                input: bone_app::InputId(1),
            },
        };
        ui.history_bytes = history_entry_bytes(&entry);
        ui.history.push_back(entry);
        ui.read_anchor = Some(ContentAnchor {
            sequence: bone_app::SessionSeq(1),
            byte: 0,
            part: AnchorPart::Text,
        });
        state.selected = Some(info.id);
        state.session_ui.insert(info.id, ui);
        state
    }

    #[test]
    fn individually_oversized_metrics_are_rejected_without_evicting_history_or_anchor() {
        let mut state = fixture();
        let anchor = state.selected_ui().unwrap().read_anchor;
        let metrics = Arc::new(TranscriptMetrics {
            event_rows: (1..=8).map(|seq| (bone_app::SessionSeq(seq), 1)).collect(),
            ..TranscriptMetrics::default()
        });
        let limit = metrics.allocated_bytes() - 1;
        assert!(state.selected_ui().unwrap().history_bytes < limit);
        assert!(!retain_transcript_with_limit(&mut state, metrics, limit));
        let ui = state.selected_ui().unwrap();
        assert_eq!(ui.history.len(), 1);
        assert_eq!(ui.read_anchor, anchor);
        assert!(ui.transcript_metrics.is_none());
        assert!(retained_history_bytes(&state) <= limit);
    }

    #[test]
    fn combined_metrics_overflow_drops_old_layout_before_current_or_history() {
        let mut state = fixture();
        let anchor = state.selected_ui().unwrap().read_anchor;
        let current = Arc::new(TranscriptMetrics {
            event_rows: [(bone_app::SessionSeq(1), 1)].into(),
            ..TranscriptMetrics::default()
        });
        let older = Arc::new(TranscriptMetrics {
            event_rows: [(bone_app::SessionSeq(2), 1)].into(),
            ..TranscriptMetrics::default()
        });
        let limit = state.selected_ui().unwrap().history_bytes + current.allocated_bytes();
        assert!(current.allocated_bytes() < limit && older.allocated_bytes() < limit);
        state.selected_ui_mut().unwrap().older_metrics = Some(older);
        assert!(retain_transcript_with_limit(
            &mut state,
            current.clone(),
            limit
        ));
        let ui = state.selected_ui().unwrap();
        assert!(ui.older_metrics.is_none());
        assert!(Arc::ptr_eq(
            ui.transcript_metrics.as_ref().unwrap(),
            &current
        ));
        assert_eq!(ui.history.len(), 1);
        assert_eq!(ui.read_anchor, anchor);
        assert!(retained_history_bytes(&state) <= limit);
    }
}

fn return_to_models(state: &mut UiState, effects: &mut Vec<Effect>) {
    state.panel = Some(Panel::Models);
    state.panel_selection = 0;
    state.models_loading = true;
    state.model_request = state.generation();
    effects.push(Effect::LoadModels {
        session: state.selected,
        request: state.model_request,
    });
    refresh_model_label(state, effects);
}

fn select_model_row(state: &mut UiState, index: usize, effects: &mut Vec<Effect>) {
    if state.models_loading || !matches!(state.panel, Some(Panel::Models)) {
        return;
    }
    state.panel_selection = index;
    if let Some(choice) = state.model_choices.get(index) {
        state.models_loading = true;
        effects.push(Effect::SetModel {
            session: state.selected,
            request: state.model_request,
            selection: choice.selection.clone(),
        });
    } else if let Some(profile) = index
        .checked_sub(state.model_choices.len())
        .and_then(|index| state.model_profiles.get(index))
    {
        let selection = state
            .model_choices
            .iter()
            .find(|choice| choice.selection.profile == profile.id)
            .map(|choice| choice.selection.clone());
        state.connection_form = Some(super::ConnectionForm::edit_selection(profile, selection));
        state.panel = Some(Panel::ModelSetup);
        state.status = None;
    } else if index == state.model_row_count() - 1 {
        state.panel = Some(Panel::ModelAdd);
        state.panel_selection = 0;
        state.status = None;
    }
}

fn choose_connection_kind(state: &mut UiState, index: usize) {
    if !matches!(state.panel, Some(Panel::ModelAdd)) {
        return;
    }
    let Some(kind) = super::ConnectionKind::ALL.get(index).copied() else {
        return;
    };
    let form = if kind.subscription() {
        state
            .model_profiles
            .iter()
            .find(|profile| profile.id == bone_app::ProfileId::chatgpt())
            .map(|profile| super::ConnectionForm::edit(profile, String::new()))
            .unwrap_or_else(|| super::ConnectionForm::new(kind))
    } else {
        super::ConnectionForm::new(kind)
    };
    state.connection_form = Some(form);
    state.panel = Some(Panel::ModelSetup);
    state.status = None;
}

fn save_connection(state: &mut UiState, effects: &mut Vec<Effect>) {
    if !matches!(state.panel, Some(Panel::ModelSetup)) {
        return;
    }
    let Some(form) = &state.connection_form else {
        return;
    };
    if form.saving {
        return;
    }
    let (profile, selection) = match form.validated() {
        Ok(value) => value,
        Err(error) => {
            state.status = Some(error);
            return;
        }
    };
    let request = state.generation();
    let form = state.connection_form.as_mut().unwrap();
    form.saving = true;
    form.request = request;
    form.key_was_sent = !form.key.is_empty();
    let key = (!form.key.is_empty()).then(|| super::SecretText::from(form.key.take()));
    state.status = None;
    effects.push(Effect::SaveConnection {
        request,
        session: state.selected,
        profile,
        key,
        selection,
    });
}

#[cfg(test)]
mod connection_tests {
    use super::*;
    use crate::state::{ConnectionForm, ConnectionKind, SecretText, SetupField};

    fn setup() -> UiState {
        let mut state = UiState::default();
        let info = bone_app::SessionInfo {
            id: SessionId::new(),
            workspace: bone_app::WorkspaceId::new(),
            title: "connection test".into(),
            archived: false,
        };
        let mut ui = SessionUi::new(info.clone(), 1);
        ui.draft = "ordinary draft".into();
        let question = bone_app::QuestionId {
            runtime: bone_app::RuntimeId::new(),
            record: 1,
            reply_to: bone_app::InputId(1),
        };
        let mut answer = AnswerDraft::new(question);
        answer.replace("answer draft".into(), 12);
        ui.answer_drafts.insert(question, answer);
        ui.selected_answer = Some(question);
        state.selected = Some(info.id);
        state.session_ui.insert(info.id, ui);
        state.panel = Some(Panel::ModelSetup);
        state.connection_form = Some(ConnectionForm::new(ConnectionKind::OpenAiResponses));
        state
    }
    fn assert_drafts(state: &UiState) {
        let ui = state.selected_ui().unwrap();
        assert_eq!(ui.draft, "ordinary draft");
        assert_eq!(ui.active_answer().unwrap().text, "answer draft");
    }
    fn act(state: &mut UiState, action: Action) -> Vec<Effect> {
        update(state, UiEvent::Action(action))
    }

    #[test]
    fn secret_input_save_failure_and_cancel_never_enter_debug_or_chat_drafts() {
        let mut state = setup();
        let secret = "secret-that-must-not-appear";
        let action = Action::SetupText(SecretText::from(secret.to_owned()));
        assert!(!format!("{action:?}").contains(secret));
        act(&mut state, Action::SelectField(SetupField::Key));
        act(&mut state, action);
        assert!(!format!("{state:?}").contains(secret));
        let effects = act(&mut state, Action::SaveConnection);
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
        assert!(state.connection_form.as_ref().unwrap().key.is_empty());
        assert!(act(&mut state, Action::SaveConnection).is_empty());
        update(
            &mut state,
            UiEvent::ConnectionSaved {
                request: *request,
                session: *session,
                error: Some("Could not save key".into()),
                notice: None,
            },
        );
        assert!(!state.connection_form.as_ref().unwrap().saving);
        assert!(state.status.as_ref().unwrap().contains("Re-enter"));
        assert!(act(&mut state, Action::SaveConnection).is_empty());
        act(&mut state, Action::Escape);
        assert!(state.connection_form.is_none());
        assert!(matches!(state.panel, Some(Panel::Models)));
        assert_drafts(&state);
    }

    #[test]
    fn cancelled_save_receipt_cannot_replace_new_form_or_start_login() {
        let mut state = setup();
        state.connection_form = Some(ConnectionForm::new(ConnectionKind::ChatGptSubscription));
        let effects = act(&mut state, Action::SaveConnection);
        let [
            Effect::SaveConnection {
                request, session, ..
            },
        ] = effects.as_slice()
        else {
            panic!("save effect")
        };
        act(&mut state, Action::Escape);
        state.panel = Some(Panel::ModelAdd);
        act(&mut state, Action::ChooseConnectionKind(3));
        state.status = Some("new form validation".into());
        let effects = update(
            &mut state,
            UiEvent::ConnectionSaved {
                request: *request,
                session: *session,
                error: None,
                notice: None,
            },
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
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::Login { .. }))
        );
        assert!(matches!(state.panel, Some(Panel::ModelSetup)));
        assert_eq!(
            state.connection_form.as_ref().unwrap().kind,
            ConnectionKind::AnthropicMessages
        );
        assert_drafts(&state);
        let reload = state.model_request;
        let selected = state.selected;
        update(
            &mut state,
            UiEvent::ModelsFailed {
                session: selected,
                request: reload,
                error: "stale reload failure".into(),
            },
        );
        assert_eq!(state.status.as_deref(), Some("new form validation"));
    }

    #[test]
    fn model_rows_edit_profiles_and_subscription_authorization_returns_to_models() {
        let mut state = setup();
        state.panel = Some(Panel::Models);
        state.connection_form = None;
        state.model_profiles = vec![bone_app::Profile::chatgpt()];
        assert_eq!(state.model_row_count(), 2);
        act(&mut state, Action::SelectModel(0));
        assert_eq!(
            state.connection_form.as_ref().unwrap().existing,
            Some(bone_app::ProfileId::chatgpt())
        );
        let effects = act(&mut state, Action::SaveConnection);
        let [
            Effect::SaveConnection {
                request, session, ..
            },
        ] = effects.as_slice()
        else {
            panic!("save effect")
        };
        let effects = update(
            &mut state,
            UiEvent::ConnectionSaved {
                request: *request,
                session: *session,
                error: None,
                notice: None,
            },
        );
        assert!(matches!(effects.as_slice(), [Effect::Login { .. }]));
        assert!(matches!(state.panel, Some(Panel::Login)));
        act(&mut state, Action::Escape);
        assert!(matches!(state.panel, Some(Panel::Models)));
        assert_drafts(&state);
        assert!(!COMMANDS.iter().any(|command| command.name == "login"));
    }

    #[test]
    fn editing_same_model_credentials_preserves_full_selection_options() {
        let mut state = setup();
        let profile = bone_app::Profile::new(
            bone_app::ProfileId::new("api").unwrap(),
            "API",
            bone_app::EndpointConfig::OpenAiResponses { base_url: None },
        )
        .unwrap();
        let mut selection = bone_app::ModelSelection::new(profile.id.clone(), "gpt-5.5").unwrap();
        selection.options = Some(serde_json::from_value(serde_json::json!({ "type": "openai_responses", "reasoning": { "effort": "high" } })).unwrap());
        state.panel = Some(Panel::Models);
        state.model_profiles = vec![profile];
        state.model_choices = vec![ModelChoice {
            selection: selection.clone(),
            profile_label: "API".into(),
        }];
        act(&mut state, Action::SelectModel(1));
        let form = state.connection_form.as_mut().unwrap();
        form.key = "rotated-key".to_owned().into();
        form.label = "renamed connection".into();
        assert_eq!(form.validated().unwrap().1.as_ref(), Some(&selection));
        let mut changed = form.clone();
        changed.model = "gpt-other".into();
        assert!(changed.validated().unwrap().1.unwrap().options.is_none());
        let effects = act(&mut state, Action::SaveConnection);
        assert!(
            matches!(effects.as_slice(), [Effect::SaveConnection { selection: Some(saved), .. }] if saved == &selection)
        );
        assert_drafts(&state);
    }

    #[test]
    fn api_credentials_may_be_retained_only_for_the_unchanged_endpoint() {
        let profile = bone_app::Profile::new(
            bone_app::ProfileId::new("api").unwrap(),
            "API",
            bone_app::EndpointConfig::OpenAiResponses { base_url: None },
        )
        .unwrap();
        let mut form = ConnectionForm::edit(&profile, String::new());
        assert!(form.validated().is_ok());
        form.base_url = "https://example.com/v1".into();
        assert!(form.validated().is_err());
        form.key = "new-secret".to_owned().into();
        assert!(form.validated().is_ok());
        form.base_url.clear();
        form.key.take();
        form.kind = ConnectionKind::AnthropicMessages;
        assert!(form.validated().is_err());
    }
}
